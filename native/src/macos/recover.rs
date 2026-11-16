//! The Windows build recovers from WebView2 exiting underneath the panel (see `src/recover.rs`).
//! That recovery is built on WebView2's process APIs; nothing equivalent is wired up for
//! WKWebView, so on macOS these are no-ops with the same surface and the shared call sites
//! stay unchanged.

use std::path::Path;
use tauri::{AppHandle, WebviewWindow};

pub fn quitting() {}

pub fn blocks_show(_app: &AppHandle) -> bool {
    false
}

pub fn watch(_app: &AppHandle, _window: &WebviewWindow) {}

/// Never a recovery restart: every start follows the command line.
pub fn resume_in_tray(_data_dir: &Path) -> Option<bool> {
    None
}
