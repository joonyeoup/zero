#!/usr/bin/env bash
# Deploy the analyze-screen pipeline to a Samsung Tizen TV over sdb.
#
# Prereqs on this machine:
#   - Tizen Studio CLI (`tizen`, `sdb`) in PATH
#   - A TV in developer mode with this machine's IP whitelisted
#     (Apps panel -> enter 12345 on the remote -> Developer mode ON + host IP)
#   - The sidecar cross-compiled for the TV's architecture (see README
#     "Cross-compiling"): scripts expect the binary path in SIDECAR_BIN.
#
# Usage:
#   TV_IP=192.168.1.50 ./deploy_tv.sh [steps...]
#   steps: connect app sidecar config smoke all   (default: all)
set -euo pipefail

# ----------------------------------------------------------- placeholders
TV_IP="${TV_IP:-<TV_IP_HERE>}"                       # e.g. 192.168.1.50
TV_HOME="${TV_HOME:-/opt/usr/home/owner}"            # writable dir on the TV
SIDECAR_BIN="${SIDECAR_BIN:-../zeroclaw/sidecar/target/armv7-unknown-linux-gnueabi/release/analyze-screen-sidecar}"
SIDECAR_TOML="${SIDECAR_TOML:-../zeroclaw/config/sidecar.toml}"
CERT_PROFILE="${CERT_PROFILE:-tvcert}"               # tizen security-profile name
APP_DIR="$(cd "$(dirname "$0")/../tizen-app" && pwd)"
WGT_OUT="${WGT_OUT:-/tmp/AnalyzeScreen.wgt}"

if [[ "$TV_IP" == "<TV_IP_HERE>" ]]; then
    echo "Set TV_IP first, e.g.: TV_IP=192.168.1.50 $0" >&2
    exit 1
fi

step() { printf '\n\033[1;34m== %s ==\033[0m\n' "$*"; }

do_connect() {
    step "sdb connect $TV_IP"
    sdb connect "$TV_IP:26101"
    sdb devices
}

do_app() {
    step "package + install Tizen web app"
    # Signing requires a Samsung certificate profile created in Tizen Studio
    # (Certificate Manager) that includes your TV's DUID.
    tizen package -t wgt -s "$CERT_PROFILE" -- "$APP_DIR" -o "$(dirname "$WGT_OUT")"
    mv "$(dirname "$WGT_OUT")"/*.wgt "$WGT_OUT" 2>/dev/null || true
    tizen install -n "$WGT_OUT" -t "$(sdb devices | awk 'NR==2{print $1}')"
    echo "Launch from the TV's Apps rail, or: tizen run -p AnLzScrn01"
}

do_sidecar() {
    step "push sidecar binary"
    [[ -f "$SIDECAR_BIN" ]] || {
        echo "sidecar binary not found at $SIDECAR_BIN — cross-compile first (see README)" >&2
        exit 1
    }
    sdb push "$SIDECAR_BIN" "$TV_HOME/analyze-screen-sidecar"
    sdb shell "chmod +x $TV_HOME/analyze-screen-sidecar"
}

do_config() {
    step "push sidecar config (+ optional ZeroClaw fragment)"
    sdb push "$SIDECAR_TOML" "$TV_HOME/sidecar.toml"
    echo "NOTE: edit $TV_HOME/sidecar.toml on the TV (sdb shell) and fill in:"
    echo "  screenshot.binary_path  -> actual tizenscreenshot location"
    echo "  screenshot.output_path  -> where it writes its PNG"
    echo "  vlm.base_url / vlm.model -> your vLLM server"
    echo
    echo "If ZeroClaw's agent should also trigger analyses, merge"
    echo "zeroclaw/config-fragment.toml into the TV's ZeroClaw config."
}

do_smoke() {
    step "smoke test on the TV"
    echo "-- can the sidecar's context spawn subprocesses? (decides exec vs watch mode)"
    sdb shell "echo exec-ok && $TV_HOME/analyze-screen-sidecar --help >/dev/null 2>&1; \
               ls -l $TV_HOME/analyze-screen-sidecar"
    echo "-- try one screenshot by hand:"
    echo "   sdb shell '<PATH_TO_BINARY>' && sdb shell 'ls -l <SCREENSHOT_OUTPUT_PATH>'"
    echo "   If this fails with a permission error, switch screenshot.mode to \"watch\"."
    echo "-- start the sidecar:"
    echo "   sdb shell 'cd $TV_HOME && SIDECAR_CONFIG=$TV_HOME/sidecar.toml ./analyze-screen-sidecar &'"
    echo "-- then from the TV shell:"
    echo "   sdb shell 'curl -s http://127.0.0.1:8787/health'"
}

steps=("${@:-all}")
for s in "${steps[@]}"; do
    case "$s" in
        connect) do_connect ;;
        app)     do_app ;;
        sidecar) do_sidecar ;;
        config)  do_config ;;
        smoke)   do_smoke ;;
        all)     do_connect; do_sidecar; do_config; do_app; do_smoke ;;
        *)       echo "unknown step: $s (connect|app|sidecar|config|smoke|all)" >&2; exit 1 ;;
    esac
done
