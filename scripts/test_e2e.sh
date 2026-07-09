#!/usr/bin/env bash
# End-to-end test of the analyze-screen pipeline on the local machine:
#   mock VLM (FastAPI) + stub tizenscreenshot (copies a sample PNG)
#   -> sidecar -> curl POST /analyze-screen -> schema assertions.
#
# Covers: happy path, fenced-output extraction, LLM repair pass,
# structured-error path, watch-mode capture, and /health.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$HERE")"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/analyze-e2e.XXXXXX")"

MOCK_PORT="${MOCK_PORT:-8008}"
SIDECAR_PORT="${SIDECAR_PORT:-8787}"
GATEWAY="http://127.0.0.1:${SIDECAR_PORT}"
MOCK="http://127.0.0.1:${MOCK_PORT}"

PIDS=()
cleanup() {
    for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
    rm -rf "$WORK"
}
trap cleanup EXIT

say()  { printf '\n\033[1;34m== %s ==\033[0m\n' "$*"; }
pass() { printf '\033[1;32mPASS\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31mFAIL\033[0m %s\n' "$*"; exit 1; }

wait_http() { # url, name
    for _ in $(seq 1 50); do
        curl -sf "$1" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    fail "$2 did not come up at $1"
}

# Validate a response body against the JSON schema contract.
# usage: assert_schema <file> <expect_error: yes|no>
assert_schema() {
    python3 - "$1" "$2" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
expect_error = sys.argv[2] == "yes"
errs = []
def need(k, t):
    if not isinstance(data.get(k), t): errs.append(f"{k} wrong/missing")
need("screen_type", str); need("title", str); need("summary", str)
if not isinstance(data.get("detected_elements"), list):
    errs.append("detected_elements not a list")
else:
    for i, el in enumerate(data["detected_elements"]):
        if not isinstance(el.get("name"), str): errs.append(f"el[{i}].name")
        if not isinstance(el.get("description"), str): errs.append(f"el[{i}].description")
        c = el.get("confidence")
        if not isinstance(c, (int, float)) or not 0 <= c <= 1: errs.append(f"el[{i}].confidence")
if not (isinstance(data.get("suggested_actions"), list)
        and all(isinstance(a, str) for a in data["suggested_actions"])):
    errs.append("suggested_actions")
err = data.get("error", "MISSING")
if expect_error:
    if not (isinstance(err, dict) and isinstance(err.get("code"), str)
            and isinstance(err.get("message"), str)):
        errs.append("error object malformed")
elif err is not None:
    errs.append(f"unexpected error field: {err}")
if errs:
    print("schema violations:", "; ".join(errs)); sys.exit(1)
PY
}

post_analyze() { # outfile -> echoes http status
    curl -s -o "$1" -w '%{http_code}' -X POST "$GATEWAY/analyze-screen" \
         -H 'Content-Type: application/json' -d '{}'
}

set_mode() { curl -sf -X POST "$MOCK/_mode" -d "{\"mode\":\"$1\"}" >/dev/null; }

# ---------------------------------------------------------------- build
say "building sidecar (release)"
cargo build --release --quiet --manifest-path "$ROOT/zeroclaw/sidecar/Cargo.toml"
SIDECAR_BIN="$ROOT/zeroclaw/sidecar/target/release/analyze-screen-sidecar"

# ------------------------------------------------------- fake TV pieces
say "setting up stub tizenscreenshot + mock VLM"
SHOT_OUT="$WORK/screenshot.png"
STUB="$WORK/tizenscreenshot"
cat > "$STUB" <<EOF
#!/bin/sh
# Stub of the TV's tizenscreenshot: "captures" by copying a sample PNG
# to the fixed output path, like the real binary does.
cp "$ROOT/mock/assets/sample_screen.png" "$SHOT_OUT"
EOF
chmod +x "$STUB"

python3 "$ROOT/server/mock_vlm_server.py" --port "$MOCK_PORT" &
PIDS+=($!)
disown 2>/dev/null || true
wait_http "$MOCK/health" "mock VLM"

# ---------------------------------------------------------- run sidecar
cat > "$WORK/sidecar.toml" <<EOF
[server]
port = ${SIDECAR_PORT}
total_timeout_secs = 90

[screenshot]
mode = "exec"
binary_path = "${STUB}"
output_path = "${SHOT_OUT}"
timeout_secs = 10

[vlm]
base_url = "${MOCK}/v1"
model = "mock-qwen3-vl"
timeout_secs = 60

[downscale]
enabled = false
EOF

SIDECAR_CONFIG="$WORK/sidecar.toml" "$SIDECAR_BIN" 2> "$WORK/sidecar.log" &
SIDECAR_PID=$!
PIDS+=($SIDECAR_PID)
disown 2>/dev/null || true
wait_http "$GATEWAY/health" "sidecar"
pass "mock VLM + sidecar up"

# --------------------------------------------------------------- tests
say "test 1: happy path (VLM returns clean JSON)"
set_mode valid
[ "$(post_analyze "$WORK/r1.json")" = "200" ] || fail "expected HTTP 200"
assert_schema "$WORK/r1.json" no || fail "schema"
grep -q '"screenshot"' <<<"$(curl -s -D - -o /dev/null -X POST "$GATEWAY/analyze-screen")" \
    && pass "valid JSON accepted, timings header present" \
    || fail "X-Timings-Ms header missing"

say "test 2: fenced output -> local extraction (no LLM call)"
set_mode fenced
[ "$(post_analyze "$WORK/r2.json")" = "200" ] || fail "expected HTTP 200"
assert_schema "$WORK/r2.json" no || fail "schema"
pass "markdown-fenced JSON recovered by extraction"

say "test 3: invalid JSON -> one LLM repair pass"
set_mode invalid
[ "$(post_analyze "$WORK/r3.json")" = "200" ] || fail "expected HTTP 200"
assert_schema "$WORK/r3.json" no || fail "schema"
# Wait a moment for logs to flush
sleep 0.2
grep -q "llm_repair" "$WORK/sidecar.log" || { echo "Log contents:"; cat "$WORK/sidecar.log"; fail "repair stage never ran"; }
pass "invalid output repaired via LLM pass"

say "test 4: unrepairable garbage -> structured error object"
set_mode broken
[ "$(post_analyze "$WORK/r4.json")" = "422" ] || fail "expected HTTP 422"
assert_schema "$WORK/r4.json" yes || fail "schema"
grep -q "VLM_OUTPUT_INVALID" "$WORK/r4.json" || fail "wrong error code"
pass "structured error returned after failed repair"

say "test 5: watch mode (no subprocess spawn by sidecar)"
kill "$SIDECAR_PID" 2>/dev/null || true
wait "$SIDECAR_PID" 2>/dev/null || true
sed -i.bak 's/mode = "exec"/mode = "watch"/; s/timeout_secs = 10/timeout_secs = 15/' \
    "$WORK/sidecar.toml"
SIDECAR_CONFIG="$WORK/sidecar.toml" "$SIDECAR_BIN" 2>> "$WORK/sidecar.log" &
SIDECAR_PID=$!
PIDS+=($SIDECAR_PID)
disown 2>/dev/null || true
wait_http "$GATEWAY/health" "sidecar (watch mode)"
set_mode valid
( sleep 1; "$STUB" ) &   # external capture trigger writes the PNG
[ "$(post_analyze "$WORK/r5.json")" = "200" ] || fail "expected HTTP 200"
assert_schema "$WORK/r5.json" no || fail "schema"
pass "watch mode picked up externally written PNG"

say "all e2e tests passed"
echo "per-stage latency lines from the sidecar log:"
grep "done in" "$WORK/sidecar.log" | tail -8
