//! `vlm_analyze` tool: base64-encode a PNG (optionally downscaled first) and
//! send it to the OpenAI-compatible VLM endpoint. Returns the raw assistant
//! text — validation happens elsewhere.

use crate::config::{Config, DownscaleConfig, VlmConfig};
use crate::stagelog::StageLog;
use base64::Engine;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub const SYSTEM_PROMPT: &str = r#"You are a TV screen analyzer. You receive one screenshot of a Samsung TV screen. Respond with ONLY a single JSON object — no markdown fences, no prose — exactly matching this schema:
{
  "screen_type": "string (e.g. live_tv, streaming_app, menu, game)",
  "title": "string, short headline of what's on screen",
  "summary": "string, 1-3 sentence description",
  "detected_elements": [ { "name": "string", "description": "string", "confidence": 0.0 } ],
  "suggested_actions": [ "string" ],
  "error": null
}
confidence is a number between 0 and 1. error must be null on success."#;

const USER_PROMPT: &str =
    "Analyze this TV screen and return the JSON object described in the system prompt.";

pub fn analyze(cfg: &Config, png_path: &Path, log: &StageLog) -> Result<String, String> {
    let png_bytes = prepare_image(&cfg.downscale, png_path, log)?;
    log.note(&format!("image payload {} bytes", png_bytes.len()));
    let data_uri = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    );

    let body = json!({
        "model": cfg.vlm.model,
        "temperature": 0,
        "max_tokens": cfg.vlm.max_tokens,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": [
                { "type": "image_url", "image_url": { "url": data_uri } },
                { "type": "text", "text": USER_PROMPT }
            ]}
        ]
    });

    chat_completion(&cfg.vlm, body, log)
}

/// One repair pass through the (possibly separate) LLM endpoint. Text-only.
pub fn repair(cfg: &Config, broken: &str, schema_hint: &str, log: &StageLog) -> Result<String, String> {
    let ep = cfg.llm_endpoint();
    let prompt = format!(
        "Repair the following text so it becomes a single valid JSON object that matches this \
         schema. Return ONLY the JSON object, with no markdown fences and no commentary.\n\n\
         Schema:\n{schema_hint}\n\nText to repair:\n{broken}"
    );
    let body = json!({
        "model": ep.model,
        "temperature": 0,
        "max_tokens": ep.max_tokens,
        "messages": [ { "role": "user", "content": prompt } ]
    });
    chat_completion(ep, body, log)
}

fn chat_completion(ep: &VlmConfig, body: Value, log: &StageLog) -> Result<String, String> {
    let url = format!("{}/chat/completions", ep.base_url.trim_end_matches('/'));
    log.note(&format!("POST {url} model={}", ep.model));
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(ep.timeout_secs))
        .build();
    let mut req = agent.post(&url).set("Content-Type", "application/json");
    if let Some(key) = &ep.api_key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp: Value = req
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => format!(
                "VLM endpoint returned HTTP {code}: {}",
                r.into_string().unwrap_or_default().chars().take(300).collect::<String>()
            ),
            other => format!("VLM request failed: {other}"),
        })?
        .into_json()
        .map_err(|e| format!("VLM response was not JSON: {e}"))?;

    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "VLM response missing choices[0].message.content".to_string())
}

fn prepare_image(ds: &DownscaleConfig, path: &Path, log: &StageLog) -> Result<Vec<u8>, String> {
    if !ds.enabled {
        return std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
    }
    if !ds.command.is_empty() {
        return downscale_external(ds, path, log);
    }
    downscale_builtin(ds, path, log)
}

fn downscale_external(ds: &DownscaleConfig, path: &Path, log: &StageLog) -> Result<Vec<u8>, String> {
    let out = std::env::temp_dir().join("analyze_screen_downscaled.png");
    let cmdline = ds
        .command
        .replace("{in}", &path.display().to_string())
        .replace("{out}", &out.display().to_string())
        .replace("{max}", &ds.max_long_edge.to_string());
    log.note(&format!("external downscale: {cmdline}"));
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmdline)
        .status()
        .map_err(|e| format!("downscale command failed to start: {e}"))?;
    if !status.success() {
        return Err(format!("downscale command exited with {status}"));
    }
    std::fs::read(&out).map_err(|e| format!("cannot read downscaled {}: {e}", out.display()))
}

#[cfg(feature = "downscale")]
fn downscale_builtin(ds: &DownscaleConfig, path: &Path, log: &StageLog) -> Result<Vec<u8>, String> {
    let img = image::open(path).map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
    let (w, h) = (img.width(), img.height());
    let max = ds.max_long_edge;
    if w.max(h) <= max {
        log.note(&format!("{w}x{h} already within {max}px, skipping downscale"));
        return std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
    }
    let resized = img.resize(max, max, image::imageops::FilterType::Triangle);
    log.note(&format!("downscaled {w}x{h} -> {}x{}", resized.width(), resized.height()));
    let mut buf = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("re-encode failed: {e}"))?;
    Ok(buf.into_inner())
}

#[cfg(not(feature = "downscale"))]
fn downscale_builtin(_ds: &DownscaleConfig, path: &Path, log: &StageLog) -> Result<Vec<u8>, String> {
    log.note("built without `downscale` feature and no external command set; sending full-size PNG");
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}
