//! Recovery when WKWebView's Web Content process is terminated underneath the panel.
//!
//! Unlike WebView2, the WKWebView view lives in this process. Only the Web Content process can
//! exit on its own (memory pressure or a renderer crash), which leaves a blank page in an
//! otherwise intact window. A process-wide restart is the wrong tool: Tauri 2.12 already
//! reloads the webview when no custom handler is set, and a custom handler must do the same.
//!
//! clipkeep's panel is transparent, borderless and always on top. A blank page in that window
//! would swallow clicks until reload finishes, so the handler hides the panel (and the tray
//! menu, if that webview died) first, then reloads. The menu-bar icon is left alone. Reloads
//! are rate-limited so a page that crashes on load cannot loop.

use crate::{hide_main, hide_tray_menu_window, win};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Webview, WebviewWindow};

const RELOAD_WINDOW: Duration = Duration::from_secs(10);
const RELOAD_LIMIT: usize = 3;

/// clipkeep is quitting on purpose. A Web Content process going away with it is not a failure.
static QUITTING: AtomicBool = AtomicBool::new(false);
static RELOADS: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

/// Call before quitting on purpose, so a terminate event is not answered with a hide/reload.
pub fn quitting() {
    QUITTING.store(true, Ordering::Relaxed);
}

pub fn blocks_show(_app: &AppHandle) -> bool {
    // The view itself survives; after a reload the panel may be shown again.
    false
}

/// The official terminate hook is installed on `tauri::Builder`; nothing to attach per window.
pub fn watch(_app: &AppHandle, _window: &WebviewWindow) {}

/// Never a recovery restart: every start follows the command line.
pub fn resume_in_tray(_data_dir: &std::path::Path) -> Option<bool> {
    None
}

/// Whether another reload is allowed in the sliding window. `attempts` is newest-last.
fn allow_reload(attempts: &mut Vec<Instant>, now: Instant) -> bool {
    attempts.retain(|at| now.saturating_duration_since(*at) < RELOAD_WINDOW);
    if attempts.len() >= RELOAD_LIMIT {
        return false;
    }
    attempts.push(now);
    true
}

/// Called on the main thread when WKWebView reports `webViewWebContentProcessDidTerminate`.
pub fn on_content_process_terminated(webview: &Webview) {
    if QUITTING.load(Ordering::Relaxed) {
        return;
    }
    let label = webview.label().to_owned();
    win::log_diagnostic(&format!("webview-content-process-terminated label={label}"));

    let app = webview.app_handle();
    match label.as_str() {
        "main" => hide_main(app),
        "tray_menu" => hide_tray_menu_window(app),
        _ => {}
    }

    let allowed = RELOADS
        .lock()
        .ok()
        .is_some_and(|mut attempts| allow_reload(&mut attempts, Instant::now()));
    if !allowed {
        win::log_diagnostic(&format!("webview-reload-skipped cooldown label={label}"));
        return;
    }
    if let Err(e) = webview.reload() {
        win::log_diagnostic(&format!("webview-reload-failed label={label} {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_reload_rate_limits_within_the_window() {
        let start = Instant::now();
        let mut attempts = Vec::new();
        for i in 0..RELOAD_LIMIT {
            assert!(
                allow_reload(&mut attempts, start + Duration::from_millis(i as u64 * 10)),
                "attempt {i} should be allowed"
            );
        }
        let last = start + Duration::from_millis((RELOAD_LIMIT as u64 - 1) * 10);
        assert!(
            !allow_reload(&mut attempts, last),
            "a fourth reload inside the window must be refused"
        );
        // The window is measured from each attempt, so wait until the newest one expires.
        assert!(
            allow_reload(
                &mut attempts,
                last + RELOAD_WINDOW + Duration::from_millis(1)
            ),
            "after the window expires a reload is allowed again"
        );
        assert_eq!(attempts.len(), 1);
    }

    #[test]
    fn resume_in_tray_is_never_a_recovery_restart() {
        assert_eq!(resume_in_tray(std::path::Path::new("/tmp")), None);
    }
}
