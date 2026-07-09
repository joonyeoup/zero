use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub screenshot: ScreenshotConfig,
    pub vlm: VlmConfig,
    /// Endpoint for the JSON repair pass. Falls back to [vlm] when absent.
    pub llm: Option<VlmConfig>,
    #[serde(default)]
    pub downscale: DownscaleConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "d_host")]
    pub host: String,
    #[serde(default = "d_port")]
    pub port: u16,
    /// Total budget for one /analyze-screen request, seconds.
    #[serde(default = "d_total_timeout")]
    pub total_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotConfig {
    /// "exec": run the binary ourselves. "watch": don't spawn anything, wait
    /// for `output_path` to be (re)written by an externally triggered capture
    /// — fallback for TVs where ZeroClaw's context may not spawn subprocesses.
    #[serde(default = "d_shot_mode")]
    pub mode: String,
    /// Path to the tizenscreenshot binary on the TV.
    #[serde(default = "d_shot_bin")]
    pub binary_path: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Fixed path the binary writes the PNG to. If the binary instead prints
    /// the path to stdout, that is detected automatically as a fallback.
    #[serde(default = "d_shot_out")]
    pub output_path: String,
    #[serde(default = "d_shot_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VlmConfig {
    /// OpenAI-compatible base URL, e.g. http://dgx:8000/v1
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default = "d_vlm_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "d_max_tokens")]
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownscaleConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Max pixels on the long edge before sending to the VLM.
    #[serde(default = "d_max_edge")]
    pub max_long_edge: u32,
    /// External command template with {in}, {out} and {max} placeholders,
    /// e.g. "convert {in} -resize {max}x{max}> {out}". When empty, the
    /// built-in `image`-crate resizer is used (requires the `downscale`
    /// feature at compile time).
    #[serde(default)]
    pub command: String,
}

fn d_host() -> String { "127.0.0.1".into() }
fn d_port() -> u16 { 8787 }
fn d_total_timeout() -> u64 { 90 }
fn d_shot_mode() -> String { "exec".into() }
fn d_shot_bin() -> String { "/opt/usr/home/owner/tizenscreenshot".into() }
fn d_shot_out() -> String { "/tmp/screenshot.png".into() }
fn d_shot_timeout() -> u64 { 10 }
fn d_vlm_timeout() -> u64 { 60 }
fn d_max_tokens() -> u32 { 1024 }
fn d_max_edge() -> u32 { 1280 }

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: d_host(), port: d_port(), total_timeout_secs: d_total_timeout() }
    }
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            mode: d_shot_mode(),
            binary_path: d_shot_bin(),
            args: Vec::new(),
            output_path: d_shot_out(),
            timeout_secs: d_shot_timeout(),
        }
    }
}

impl Config {
    /// Load from SIDECAR_CONFIG (or ./sidecar.toml), then apply env overrides
    /// so nothing secret has to live in the file.
    pub fn load() -> Result<Self, String> {
        let path: PathBuf = std::env::var("SIDECAR_CONFIG")
            .unwrap_or_else(|_| "sidecar.toml".into())
            .into();
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&raw).map_err(|e| format!("bad config {}: {e}", path.display()))?;

        if let Ok(v) = std::env::var("SIDECAR_PORT") {
            cfg.server.port = v.parse().map_err(|_| "SIDECAR_PORT not a port number")?;
        }
        if let Ok(v) = std::env::var("SCREENSHOT_BIN") { cfg.screenshot.binary_path = v; }
        if let Ok(v) = std::env::var("SCREENSHOT_OUTPUT") { cfg.screenshot.output_path = v; }
        if let Ok(v) = std::env::var("SCREENSHOT_MODE") { cfg.screenshot.mode = v; }
        if let Ok(v) = std::env::var("VLM_BASE_URL") { cfg.vlm.base_url = v; }
        if let Ok(v) = std::env::var("VLM_MODEL") { cfg.vlm.model = v; }
        if let Ok(v) = std::env::var("VLM_API_KEY") { cfg.vlm.api_key = Some(v); }
        if let Ok(v) = std::env::var("LLM_BASE_URL") {
            let llm = cfg.llm.get_or_insert_with(|| cfg.vlm.clone());
            llm.base_url = v;
        }
        if let Ok(v) = std::env::var("LLM_MODEL") {
            let llm = cfg.llm.get_or_insert_with(|| cfg.vlm.clone());
            llm.model = v;
        }

        if !matches!(cfg.screenshot.mode.as_str(), "exec" | "watch") {
            return Err(format!("screenshot.mode must be exec|watch, got {}", cfg.screenshot.mode));
        }
        Ok(cfg)
    }

    pub fn llm_endpoint(&self) -> &VlmConfig {
        self.llm.as_ref().unwrap_or(&self.vlm)
    }
}
