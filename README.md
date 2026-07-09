# Analyze Screen — ZeroClaw-orchestrated screen analysis on a Tizen TV

Press a button on the TV → a screenshot is captured → sent to a remote VLM
(Qwen3-VL via vLLM) → the JSON answer is schema-validated (with a local
extraction pass and one LLM repair pass as fallbacks) → rendered as an
overlay on the TV.

## Architecture

```
┌────────────────────────────── Samsung Tizen TV ──────────────────────────────┐
│                                                                              │
│  ┌─────────────────┐  ENTER / click                                          │
│  │  Tizen web app  │───────────────┐                                         │
│  │  (tizen-app/)   │               ▼                                         │
│  │  overlay UI:    │   POST http://127.0.0.1:8787/analyze-screen             │
│  │  loading/error/ │◄──────────────┐ validated JSON (+ X-Timings-Ms)         │
│  │  result         │               │                                         │
│  └─────────────────┘               │                                         │
│                        ┌───────────┴────────────┐      ┌──────────────────┐  │
│                        │  analyze-screen-sidecar │      │ ZeroClaw daemon  │  │
│                        │  (zeroclaw/sidecar/)    │      │ gateway :42617   │  │
│                        │  1 screenshot (exec     │      │ (untouched; can  │  │
│                        │    or watch mode) ──────┼──┐   │  optionally call │  │
│                        │  2 base64 + downscale   │  │   │  the sidecar via │  │
│                        │  3 vlm_analyze ─────────┼──┼─┐ │  http_request)   │  │
│                        │  4 validate_json        │  │ │ └──────────────────┘  │
│                        │  5 (extract → repair)   │  │ │                       │
│                        └─────────────────────────┘  │ │                       │
│                                                     ▼ │                       │
│                                     ./tizenscreenshot │ writes PNG            │
└────────────────────────────────────────────────────┼──┼───────────────────────┘
                                                     │  │  HTTPS/HTTP (LAN)
                                                     │  ▼
                                       ┌─────────────┴──────────────┐
                                       │  vLLM server (DGX)         │
                                       │  Qwen3-VL-8B               │
                                       │  OpenAI-compatible         │
                                       │  /v1/chat/completions      │
                                       │  (mocked locally by        │
                                       │   server/mock_vlm_server)  │
                                       └────────────────────────────┘
```

Why a sidecar instead of a ZeroClaw route/tool: the installed ZeroClaw
(0.8.2) hard-codes its gateway routes and its WASM plugins cannot register
HTTP endpoints — details and coexistence notes in
[`zeroclaw/README.md`](zeroclaw/README.md). The handler is deliberately
split (`run_pipeline` has no HTTP types in it) so an async job-polling
variant (`POST /analyze-screen/jobs` → poll) can be added later without
touching the tool chain.

## Repo layout

| Path | What |
|---|---|
| `zeroclaw/sidecar/` | Rust sidecar: `POST /analyze-screen` → screenshot → VLM → validate |
| `zeroclaw/config/sidecar.toml` | Sidecar config (all placeholders live here) |
| `zeroclaw/config-fragment.toml` | Optional ZeroClaw config additions |
| `tizen-app/` | Tizen 6.0+ web app (10-foot UI, remote-key handling) |
| `server/mock_vlm_server.py` | FastAPI mock of vLLM's OpenAI endpoint (valid/fenced/invalid/broken modes) |
| `scripts/test_e2e.sh` | Full local e2e: mock VLM + stub screenshot + curl + schema asserts |
| `scripts/deploy_tv.sh` | sdb-based TV deploy steps |

## Quick start (local, no TV, no DGX)

```bash
# 1. run everything end-to-end (builds sidecar, starts mock VLM, asserts schema)
scripts/test_e2e.sh

# 2. or run the pieces by hand:
python3 server/mock_vlm_server.py --port 8008 &
cargo build --release --manifest-path zeroclaw/sidecar/Cargo.toml
SIDECAR_CONFIG=zeroclaw/config/sidecar.toml \
  VLM_BASE_URL=http://127.0.0.1:8008/v1 \
  SCREENSHOT_BIN=/path/to/stub \
  zeroclaw/sidecar/target/release/analyze-screen-sidecar
curl -s -X POST http://127.0.0.1:8787/analyze-screen | python3 -m json.tool
```

To preview the TV app on this machine: open `tizen-app/index.html` in a
browser while the sidecar runs (CORS is open on the sidecar).

## Configuration table

Every placeholder from the spec, where it lives, and its env override.
No secrets go in files — use the env vars.

| Placeholder | Config location | Env override | Default |
|---|---|---|---|
| `{{PATH_TO_BINARY}}` | `sidecar.toml` `screenshot.binary_path` | `SCREENSHOT_BIN` | `/opt/usr/home/owner/tizenscreenshot` |
| `{{SCREENSHOT_OUTPUT_PATH}}` | `screenshot.output_path` | `SCREENSHOT_OUTPUT` | `/tmp/screenshot.png` |
| (capture mode) | `screenshot.mode` (`exec`\|`watch`) | `SCREENSHOT_MODE` | `exec` |
| screenshot timeout | `screenshot.timeout_secs` | — | `10` |
| `{{VLM_BASE_URL}}` | `vlm.base_url` | `VLM_BASE_URL` | *(fill in)* |
| `{{VLM_MODEL_NAME}}` | `vlm.model` | `VLM_MODEL` | `Qwen/Qwen3-VL-8B-Instruct` |
| VLM API key | *(env only)* | `VLM_API_KEY` | none |
| VLM timeout | `vlm.timeout_secs` | — | `60` |
| `{{LLM_BASE_URL}}` (repair) | `[llm]` table (optional) | `LLM_BASE_URL`, `LLM_MODEL` | reuses `[vlm]` |
| `{{ZEROCLAW_PORT}}` (sidecar port) | `server.port` | `SIDECAR_PORT` | `8787` |
| total request timeout | `server.total_timeout_secs` | — | `90` |
| downscaling | `[downscale]` `enabled` / `max_long_edge` / `command` | — | off / `1280` / built-in |
| config file path | — | `SIDECAR_CONFIG` | `./sidecar.toml` |
| gateway URL in the app | `tizen-app/js/config.js` `GATEWAY_URL` | — | `http://127.0.0.1:8787/analyze-screen` |

`{{ZEROCLAW_CONFIG_PATH}}` (ZeroClaw's own config, `~/.zeroclaw/config.toml`)
is only touched if you merge `zeroclaw/config-fragment.toml`.

## Validation pipeline (VLM output contract)

1. **Strict**: parse + schema-check the raw VLM text. Pure Rust, no LLM.
2. **Extract**: strip ```` ```json ```` fences / take the first balanced
   `{...}`; re-validate.
3. **Repair**: one LLM pass ("fix to match this schema, return only JSON");
   re-validate.
4. **Fail**: HTTP 422 with a schema-shaped body whose `error` field is
   `{"code": "VLM_OUTPUT_INVALID", "message": ...}`. Screenshot/VLM
   transport failures return 502 the same way (`SCREENSHOT_FAILED`,
   `VLM_FAILED`), timeouts 504.

Per-stage latency: the sidecar logs every stage with RFC3339 timestamps and
returns `X-Timings-Ms: {"screenshot":…,"vlm_call":…,"validate":…,"total":…}`;
the TV app prints its own `button_press`/`gateway_response`/`render` marks to
the console and shows the header values in the result overlay.

## Deploying to the TV

1. **Cross-compile the sidecar.** Most Samsung TVs are 32-bit ARM:
   ```bash
   rustup target add armv7-unknown-linux-gnueabi
   cargo build --release --manifest-path zeroclaw/sidecar/Cargo.toml \
     --target armv7-unknown-linux-gnueabi
   ```
   You need a matching linker (e.g. Tizen Studio's native toolchain or
   `arm-linux-gnueabi-gcc`) configured in `.cargo/config.toml`. If the
   `image` crate makes the cross-build awkward, drop it:
   `--no-default-features` (downscaling then needs `[downscale].command`
   or stays off).
2. **Deploy:** `TV_IP=192.168.1.50 scripts/deploy_tv.sh all` — connects sdb,
   pushes binary + config, packages/installs the `.wgt`, and prints the
   smoke-test commands.
3. **Decide exec vs watch mode** with the smoke test the script prints: if
   `sdb shell '<PATH_TO_BINARY>'` works from the sidecar's context, keep
   `mode = "exec"`; if subprocess spawning is blocked, set `mode = "watch"`
   and have whatever *can* run the binary (ZeroClaw's shell tool, a cron,
   a key-hook daemon) write the PNG to `screenshot.output_path`.

## Troubleshooting

**CORS / localhost from the Tizen app.** Tizen web apps run from a `file://`
origin, so the browser sends `Origin: null` and enforces CORS on `fetch()`.
The sidecar answers preflights and sets `Access-Control-Allow-Origin: *` on
everything, and `config.xml` must keep both the `<access origin>` and
`<tizen:allow-navigation>` entries for `http://127.0.0.1`. If you still see
`TypeError: Failed to fetch`, confirm the sidecar is bound and reachable
*from the TV itself*: `sdb shell 'curl -v http://127.0.0.1:8787/health'`.

**Mixed content.** The app is served from `file://`, not `https://`, so
plain-HTTP calls to `127.0.0.1` are allowed. If you later host the app page
over HTTPS (e.g. remote hosted UI), browsers will block the `http://127.0.0.1`
call as mixed content — keep the app packaged as a `.wgt`, or put TLS on the
sidecar.

**Binary exec permissions.** `sdb push` does not preserve the execute bit —
always `chmod +x` after pushing (deploy script does). If exec fails with
`Permission denied` despite the bit, the partition may be mounted `noexec`
or Smack policy blocks it; try `$TV_HOME` (`/opt/usr/home/owner`) rather
than `/tmp`, or fall back to `screenshot.mode = "watch"`.

**vLLM image payload limits.** A 4K PNG base64-encodes to ~10–30 MB and can
exceed vLLM's request cap or blow up prefill time. Fixes: enable
`[downscale]` (1280px long edge ≈ hundreds of KB), raise the server caps
(`--max-model-len`, and for multimodal `--limit-mm-per-prompt image=1`;
behind a reverse proxy also `client_max_body_size`), and keep
`vlm.timeout_secs` generous — first requests compile CUDA graphs and are
slow.

**Screenshot appears stale.** In exec mode the sidecar compares the output
file's mtime before/after running the binary and accepts the file if the
binary succeeded; if your TV's filesystem has coarse mtime granularity and
you get yesterday's screen, point `screenshot.output_path` at a
timestamped-per-capture location or have the capture wrapper `rm` the file
first.

**Port collisions with ZeroClaw.** ZeroClaw's gateway defaults to 42617;
the sidecar to 8787. If 8787 is taken, change `server.port` *and*
`GATEWAY_URL` in `tizen-app/js/config.js`.
