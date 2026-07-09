/* Gateway location + timeouts. Edit here (or regenerate at deploy time) —
 * no values are baked into main.js. */
var APP_CONFIG = {
    // ZeroClaw analyze-screen sidecar on the TV ({{ZEROCLAW_PORT}} = 8787)
    GATEWAY_URL: "http://127.0.0.1:8787/analyze-screen",
    // Client-side cap; must be >= the sidecar's total_timeout_secs (90s)
    REQUEST_TIMEOUT_MS: 95000
};
