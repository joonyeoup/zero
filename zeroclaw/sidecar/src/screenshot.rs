//! `screenshot` tool: obtain a fresh PNG of the TV screen.
//!
//! exec mode : spawn the tizenscreenshot binary with a timeout and pick up
//!             the PNG from the configured fixed path (or from a path the
//!             binary printed to stdout, as a fallback pattern).
//! watch mode: never spawn anything — wait for the configured output path to
//!             be (re)written by an externally triggered capture. This is the
//!             fallback for TVs where subprocess spawning is blocked.

use crate::config::ScreenshotConfig;
use crate::stagelog::StageLog;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};
use wait_timeout::ChildExt;

pub fn capture(cfg: &ScreenshotConfig, log: &StageLog) -> Result<PathBuf, String> {
    match cfg.mode.as_str() {
        "watch" => watch(cfg, log),
        _ => exec(cfg, log),
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn exec(cfg: &ScreenshotConfig, log: &StageLog) -> Result<PathBuf, String> {
    let out_path = PathBuf::from(&cfg.output_path);
    let before = mtime(&out_path);

    log.note(&format!("exec {} {:?}", cfg.binary_path, cfg.args));
    let mut child = Command::new(&cfg.binary_path)
        .args(&cfg.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", cfg.binary_path))?;

    let status = child
        .wait_timeout(Duration::from_secs(cfg.timeout_secs))
        .map_err(|e| format!("wait failed: {e}"))?;
    let status = match status {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("screenshot binary timed out after {}s", cfg.timeout_secs));
        }
    };

    let output = child.wait_with_output().map_err(|e| format!("read output failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "screenshot binary exited with {status}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // Pattern 1: fixed output path, freshly written.
    if out_path.is_file() && mtime(&out_path) != before {
        return Ok(out_path);
    }

    // Pattern 2: binary printed the PNG path to stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(p) = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && Path::new(l).is_file())
    {
        log.note(&format!("using path from stdout: {p}"));
        return Ok(PathBuf::from(p));
    }

    // Pattern 1 fallback: fixed path exists but mtime did not change
    // (some filesystems have coarse mtime resolution) — accept it.
    if out_path.is_file() {
        log.note("output mtime unchanged; accepting existing file");
        return Ok(out_path);
    }

    Err(format!(
        "screenshot binary succeeded but no PNG found at {} and stdout had no valid path",
        out_path.display()
    ))
}

fn watch(cfg: &ScreenshotConfig, log: &StageLog) -> Result<PathBuf, String> {
    let out_path = PathBuf::from(&cfg.output_path);
    let before = mtime(&out_path);
    log.note(&format!(
        "watch mode: waiting up to {}s for {} to be written",
        cfg.timeout_secs,
        out_path.display()
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(cfg.timeout_secs);
    while std::time::Instant::now() < deadline {
        if out_path.is_file() && mtime(&out_path) != before {
            // Give the writer a moment to finish the file.
            std::thread::sleep(Duration::from_millis(200));
            return Ok(out_path);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "no fresh screenshot appeared at {} within {}s (watch mode)",
        out_path.display(),
        cfg.timeout_secs
    ))
}
