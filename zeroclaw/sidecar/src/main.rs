//! analyze-screen-sidecar — thin HTTP server that sits next to ZeroClaw on
//! the TV and implements the deterministic tool chain behind
//! POST /analyze-screen:
//!
//!   screenshot (exec/watch) -> vlm_analyze -> validate_json (-> llm repair)
//!
//! Synchronous request/response for the POC. `run_pipeline` is deliberately
//! free of any HTTP types so an async/job-polling variant can later wrap the
//! same function (submit -> job id -> poll) without touching the tool chain.

mod config;
mod screenshot;
mod stagelog;
mod validate;
mod vlm;

use config::Config;
use serde_json::Value;
use stagelog::StageLog;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tiny_http::{Header, Method, Response, Server};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);

fn main() {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    };
    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "analyze-screen-sidecar listening on http://{addr} \
         (screenshot mode: {}, vlm: {})",
        cfg.screenshot.mode, cfg.vlm.base_url
    );

    let cfg = Arc::new(cfg);
    for request in server.incoming_requests() {
        let cfg = Arc::clone(&cfg);
        // Thread-per-request: /health and CORS preflights must not queue
        // behind a slow analyze call.
        std::thread::spawn(move || handle(request, &cfg));
    }
}

fn cors_headers() -> Vec<Header> {
    [
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "POST, GET, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type"),
        ("Access-Control-Expose-Headers", "X-Timings-Ms"),
    ]
    .iter()
    .map(|(k, v)| Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap())
    .collect()
}

fn json_response(status: u16, body: &Value, extra: Vec<Header>) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    for h in cors_headers().into_iter().chain(extra) {
        resp = resp.with_header(h);
    }
    resp
}

fn handle(mut request: tiny_http::Request, cfg: &Config) {
    let method = request.method().clone();
    let url = request.url().split('?').next().unwrap_or("").to_string();

    let result = match (&method, url.as_str()) {
        (Method::Options, _) => {
            let mut resp = Response::empty(204);
            for h in cors_headers() {
                resp = resp.with_header(h);
            }
            request.respond(resp)
        }
        (Method::Get, "/health") => {
            request.respond(json_response(200, &serde_json::json!({"status": "ok"}), vec![]))
        }
        (Method::Post, "/analyze-screen") => {
            // Body is unused today but drained so keep-alive stays correct;
            // a future variant may accept options here.
            let mut body = String::new();
            let _ = request.as_reader().take(64 * 1024).read_to_string(&mut body);

            let id = format!("req-{}", REQ_COUNTER.fetch_add(1, Ordering::Relaxed));
            let mut log = StageLog::new(&id);
            let (status, value) = run_pipeline(cfg, &mut log);
            let timings = log.finish();
            let timings_header =
                Header::from_bytes(&b"X-Timings-Ms"[..], timings.as_bytes()).unwrap();
            request.respond(json_response(status, &value, vec![timings_header]))
        }
        _ => request.respond(json_response(
            404,
            &validate::error_object("NOT_FOUND", &format!("no route {method} {url}")),
            vec![],
        )),
    };
    if let Err(e) = result {
        eprintln!("failed to send response: {e}");
    }
}

/// Steps 3–6 of the flow. Always returns a schema-shaped JSON object; on
/// failure the `error` field is populated (structured error contract).
fn run_pipeline(cfg: &Config, log: &mut StageLog) -> (u16, Value) {
    let budget = cfg.server.total_timeout_secs;

    // Step 3a: capture.
    log.stage("screenshot");
    let png_path = match screenshot::capture(&cfg.screenshot, log) {
        Ok(p) => p,
        Err(e) => return (502, validate::error_object("SCREENSHOT_FAILED", &e)),
    };

    if log.elapsed_secs() >= budget {
        return (504, validate::error_object("TIMEOUT", "total request budget exhausted after screenshot"));
    }

    // Steps 3b–4: encode + VLM call.
    log.stage("vlm_call");
    let raw = match vlm::analyze(cfg, &png_path, log) {
        Ok(t) => t,
        Err(e) => return (502, validate::error_object("VLM_FAILED", &e)),
    };

    // Step 5a: strict local parse + schema validation. No LLM.
    log.stage("validate");
    let first_errors = match validate::parse_and_validate(&raw) {
        Ok(v) => return (200, v),
        Err(errs) => errs,
    };
    log.note(&format!("strict validation failed: {}", first_errors.join("; ")));

    // Step 5b: strip fences / extract first JSON object, revalidate.
    if let Some(candidate) = validate::extract_json_candidate(&raw) {
        match validate::parse_and_validate(&candidate) {
            Ok(v) => {
                log.note("recovered via fence/JSON extraction");
                return (200, v);
            }
            Err(errs) => log.note(&format!("extracted candidate still invalid: {}", errs.join("; "))),
        }
    } else {
        log.note("no JSON object found to extract");
    }

    if log.elapsed_secs() >= budget {
        return (504, validate::error_object("TIMEOUT", "total request budget exhausted before repair pass"));
    }

    // Step 5c: one repair pass through the LLM, then final validation.
    log.stage("llm_repair");
    match vlm::repair(cfg, &raw, validate::SCHEMA_HINT, log) {
        Ok(repaired) => {
            let candidate = validate::extract_json_candidate(&repaired).unwrap_or(repaired);
            match validate::parse_and_validate(&candidate) {
                Ok(v) => {
                    log.note("recovered via LLM repair pass");
                    (200, v)
                }
                Err(errs) => (
                    // Step 5d: structured error object.
                    422,
                    validate::error_object(
                        "VLM_OUTPUT_INVALID",
                        &format!(
                            "VLM output failed validation even after repair: {}",
                            errs.join("; ")
                        ),
                    ),
                ),
            }
        }
        Err(e) => (502, validate::error_object("REPAIR_FAILED", &e)),
    }
}
