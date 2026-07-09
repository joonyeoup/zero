# ZeroClaw integration — why a sidecar, and how they coexist

## Why steps 3–6 live in a sidecar, not inside ZeroClaw

This was checked against the installed runtime (ZeroClaw **0.8.2**, this repo,
config `~/.zeroclaw/config.toml`):

1. **The gateway's HTTP routes are hard-coded.** All routes (`/webhook`,
   `/api/*`, `/a2a/*`, …) are registered in
   `crates/zeroclaw-gateway/src/lib.rs`. There is no config mechanism —
   `[gateway]`, `[hooks]`, or otherwise — that maps a custom path like
   `POST /analyze-screen` to a tool chain.
2. **Plugins can't add routes either.** ZeroClaw plugins are WASM components
   (`crates/zeroclaw-plugins`) that register *tools* the agent may call; they
   get outbound `wasi:http`, but no way to register gateway endpoints.
3. **The agentic path is the wrong tool here anyway.** Routing the request
   through `/webhook` would ask the configured local model to orchestrate
   screenshot → VLM → validation via tool calls. Small local models are
   unreliable at multi-step tool calling in this runtime, and the pipeline is
   fully deterministic — there is no decision for an LLM to make.

Hence the contingency from the spec: a **thin Rust sidecar**
(`sidecar/`) implements steps 3–6 behind `POST /analyze-screen`. The tool
names from the spec map to modules: `screenshot.rs`, `vlm.rs`
(`vlm_analyze`), `validate.rs` (`validate_json`).

## Coexistence with ZeroClaw on the TV

- The sidecar binds `127.0.0.1:8787` (configurable); ZeroClaw's gateway keeps
  its own port (default 42617). They share nothing at runtime — no port, no
  state, no config file — so either can restart independently.
- The Tizen app talks **only** to the sidecar. ZeroClaw keeps serving its
  normal channels/webhook duties untouched.
- Optional: the ZeroClaw *agent* can itself trigger an analysis, because the
  sidecar is just an HTTP endpoint — the agent's built-in `http_request` tool
  can `POST http://127.0.0.1:8787/analyze-screen`. `config-fragment.toml`
  shows the relevant knobs.
- Process supervision: start both from the same boot script, e.g.
  `zeroclaw daemon &` and `SIDECAR_CONFIG=/path/sidecar.toml ./analyze-screen-sidecar &`.

## Files

| File | Purpose |
|---|---|
| `sidecar/` | Rust crate: HTTP server + screenshot/vlm_analyze/validate_json chain |
| `config/sidecar.toml` | Default sidecar config (all placeholders documented in top-level README) |
| `config-fragment.toml` | Optional additions to `~/.zeroclaw/config.toml` for coexistence |
