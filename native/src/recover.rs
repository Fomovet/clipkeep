//! Recovery when WebView2 goes away underneath the panel.
//!
//! The main window is a transparent, borderless, always-on-top shell; everything visible in it is
//! drawn by WebView2. If the WebView2 browser process exits while clipkeep keeps running, the
//! shell stays on screen with nothing painted in it: invisible, but still taking every click on
//! that part of the screen. A lost render process instead leaves WebView2's own error page, which
//! has no way to close the window.
//!
//! Either way the window is hidden at once. A lost render process is reloaded in place. A lost
//! browser process cannot be: clipkeep restarts itself into the tray, the path every logon takes.

use crate::{Core, db, hide_main, hide_tray_menu_window, win};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewWindow};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED, ICoreWebView2,
};
use webview2_com::ProcessFailedEventHandler;
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// Written just before a recovery restart, so the next start can tell it is one. Holds the
/// time of the restart and what the new process should do; the time is in the content because
/// rewriting a file with the same bytes need not change its modification time.
const MARKER: &str = "webview-recovery";
/// A start this soon after the marker was written is the recovery restart itself.
const RESUME_WINDOW: Duration = Duration::from_secs(15);
/// WebView2 lost again this soon after a recovery restart means restarting doesn't help (the
/// runtime is missing or blocked). Stay hidden rather than restart in a loop.
const RESTART_COOLDOWN: Duration = Duration::from_secs(60);

/// The browser process is gone; the main window must not be shown again in this process.
static WEBVIEW_LOST: AtomicBool = AtomicBool::new(false);
/// clipkeep is quitting on purpose. WebView2 exits along with it, which is not a failure.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Call before quitting on purpose, so WebView2 going away isn't answered with a restart.
pub fn quitting() {
    QUITTING.store(true, Ordering::Relaxed);
}

/// What the process started by a recovery restart does with the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// WebView2 went away by itself: come back quietly.
    Tray,
    /// The user asked for the panel and got a restart instead: show it.
    Panel,
}

impl Resume {
    fn as_str(self) -> &'static str {
        match self {
            Resume::Tray => "tray",
            Resume::Panel => "panel",
        }
    }
}

struct Marker {
    age: Duration,
    resume: Resume,
}

fn marker_path(app: &AppHandle) -> Option<PathBuf> {
    app.try_state::<Core>()
        .map(|core| core.data_dir.join(MARKER))
}

fn marker_text(written_ms: i64, resume: Resume) -> String {
    format!("{written_ms} {}", resume.as_str())
}

/// `None` for anything unreadable, or written in the future (the clock was set back).
fn parse_marker(text: &str, now_ms: i64) -> Option<Marker> {
    let (written_ms, resume) = text.trim().split_once(' ')?;
    let age_ms = now_ms.checked_sub(written_ms.parse::<i64>().ok()?)?;
    let resume = match resume {
        "tray" => Resume::Tray,
        "panel" => Resume::Panel,
        _ => return None,
    };
    Some(Marker {
        age: Duration::from_millis(u64::try_from(age_ms).ok()?),
        resume,
    })
}

fn read_marker(path: &Path) -> Option<Marker> {
    parse_marker(&std::fs::read_to_string(path).ok()?, db::now_ms())
}

/// `Some` when this start is the restart after a lost WebView2.
pub fn resuming(data_dir: &Path) -> Option<Resume> {
    read_marker(&data_dir.join(MARKER))
        .filter(|marker| marker.age < RESUME_WINDOW)
        .map(|marker| marker.resume)
}

/// Whether the panel may not be shown because WebView2 is gone. Each attempt to show it retries
/// the restart, which goes through once the cooldown has passed.
pub fn blocks_show(app: &AppHandle) -> bool {
    if !WEBVIEW_LOST.load(Ordering::Relaxed) {
        return false;
    }
    win::log_diagnostic("show-main-refused webview-lost");
    restart(app, Resume::Panel);
    true
}

/// Starts watching the main window's WebView2. Call once, after `Core` is managed.
pub fn watch(app: &AppHandle, window: &WebviewWindow) {
    let app = app.clone();
    let registered = window.with_webview(move |webview| {
        // SAFETY: the controller and the interfaces obtained from it are live COM objects
        // owned by this window, and this closure runs on the thread that created them.
        let core = match unsafe { webview.controller().CoreWebView2() } {
            Ok(core) => core,
            Err(e) => {
                win::log_diagnostic(&format!("webview-watch-failed {e}"));
                return;
            }
        };
        watch_process_failures(&app, &core);
        watch_browser_process(&app, &core);
    });
    if let Err(e) = registered {
        win::log_diagnostic(&format!("webview-watch-failed {e}"));
    }
}

/// WebView2 reports a failed process here, on the UI thread.
fn watch_process_failures(app: &AppHandle, core: &ICoreWebView2) {
    let app = app.clone();
    let handler = ProcessFailedEventHandler::create(Box::new(move |sender, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
        // SAFETY: `args` is the live event object WebView2 passed to this callback and
        // `kind` is a valid out-pointer for the duration of the call.
        unsafe { args.ProcessFailedKind(&mut kind)? };
        win::log_diagnostic(&format!("webview-process-failed kind={}", kind.0));
        if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
            browser_lost(&app);
        } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
            // Don't leave WebView2's error page up: it has no close button and ignores Esc.
            hide_main(&app);
            if let Some(sender) = sender {
                // SAFETY: `sender` is the live WebView that raised the event.
                if let Err(e) = unsafe { sender.Reload() } {
                    win::log_diagnostic(&format!("webview-reload-failed {e}"));
                }
            }
        }
        // GPU and utility processes are restarted by WebView2 itself.
        Ok(())
    }));
    let mut token = 0i64;
    // SAFETY: `core` is live, `handler` is a valid COM handler and `token` a valid out-pointer.
    if let Err(e) = unsafe { core.add_ProcessFailed(&handler, &mut token) } {
        win::log_diagnostic(&format!("webview-watch-failed {e}"));
    }
}

/// Waits on the browser process itself. The ProcessFailed event has to travel through the very
/// process that died, so this is the signal that cannot be missed.
fn watch_browser_process(app: &AppHandle, core: &ICoreWebView2) {
    let mut pid = 0u32;
    // SAFETY: `core` is live and `pid` is a valid out-pointer.
    if let Err(e) = unsafe { core.BrowserProcessId(&mut pid) } {
        win::log_diagnostic(&format!("webview-browser-pid-failed {e}"));
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // SAFETY: plain Win32 calls; the handle is owned by this thread, checked for null,
        // and closed exactly once after the wait.
        let exited = unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                win::log_diagnostic(&format!("webview-browser-open-failed pid={pid}"));
                return;
            }
            let wait = WaitForSingleObject(process, INFINITE);
            CloseHandle(process);
            wait == WAIT_OBJECT_0
        };
        if !exited {
            return;
        }
        win::log_diagnostic(&format!("webview-browser-exited pid={pid}"));
        let handle = app.clone();
        // During a normal quit the event loop is already gone and this simply fails.
        let _ = app.run_on_main_thread(move || browser_lost(&handle));
    });
}

/// Main thread. Takes the empty shell off the screen first, then brings the app back.
fn browser_lost(app: &AppHandle) {
    if QUITTING.load(Ordering::Relaxed) || WEBVIEW_LOST.swap(true, Ordering::Relaxed) {
        return;
    }
    hide_main(app);
    hide_tray_menu_window(app);
    restart(app, Resume::Tray);
}

/// Restarts clipkeep, unless a recovery restart just happened.
fn restart(app: &AppHandle, resume: Resume) {
    let Some(marker) = marker_path(app) else {
        return;
    };
    if read_marker(&marker).is_some_and(|last| last.age < RESTART_COOLDOWN) {
        win::log_diagnostic("webview-restart-skipped cooldown");
        return;
    }
    if let Err(e) = std::fs::write(&marker, marker_text(db::now_ms(), resume)) {
        // Without the marker the next start couldn't stop a restart loop.
        win::log_diagnostic(&format!("webview-restart-skipped marker {e}"));
        return;
    }
    win::log_diagnostic("webview-restart");
    let app = app.clone();
    // Off the main thread, Tauri delivers the exit events first; the single-instance plugin
    // releases its lock there, so the new process doesn't hand over to this one and quit.
    std::thread::spawn(move || app.restart());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_round_trips_with_its_age() {
        let text = marker_text(1_000_000, Resume::Panel);
        let marker = parse_marker(&text, 1_004_000).expect("valid marker");
        assert_eq!(marker.age, Duration::from_secs(4));
        assert_eq!(marker.resume, Resume::Panel);
        let marker = parse_marker(&marker_text(1_000_000, Resume::Tray), 1_000_000);
        assert_eq!(marker.expect("valid marker").resume, Resume::Tray);
    }

    #[test]
    fn unreadable_or_future_markers_are_ignored() {
        for text in ["", "tray", "123", "abc tray", "123 later", "123 tray extra"] {
            assert!(parse_marker(text, 1_000).is_none(), "{text:?}");
        }
        // Written "after" now: the clock was set back, so its age means nothing.
        assert!(parse_marker(&marker_text(2_000, Resume::Tray), 1_000).is_none());
    }

    #[test]
    fn resuming_follows_the_marker_file() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).expect("system random source");
        let dir = std::env::temp_dir().join(format!(
            "clipkeep-recover-test-{}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&dir).expect("create temp dir");
        let marker = dir.join(MARKER);
        assert_eq!(resuming(&dir), None, "no marker yet");

        std::fs::write(&marker, marker_text(db::now_ms(), Resume::Tray)).expect("write marker");
        assert_eq!(resuming(&dir), Some(Resume::Tray), "fresh marker");

        // Rewritten for a later restart, it must count from the new time.
        let stale = db::now_ms() - 3_600_000;
        std::fs::write(&marker, marker_text(stale, Resume::Tray)).expect("write marker");
        assert_eq!(resuming(&dir), None, "an hour old");
        assert!(
            read_marker(&marker).is_some_and(|last| last.age >= RESTART_COOLDOWN),
            "an old restart must not hold back a new one"
        );
        std::fs::write(&marker, marker_text(db::now_ms(), Resume::Panel)).expect("write marker");
        assert_eq!(resuming(&dir), Some(Resume::Panel), "rewritten marker");
        assert!(read_marker(&marker).is_some_and(|last| last.age < RESTART_COOLDOWN));

        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }
}
