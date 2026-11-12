//! macOS counterpart of the Win32 focus session tracker (`native/src/focus.rs`).
//!
//! It keeps the same public surface so `lib.rs` can share the v0.2.4 return-to-input flow on
//! every platform. Instead of HWNDs and GUI thread info it remembers the frontmost application
//! with `NSWorkspace` and restores it with `NSRunningApplication` before Cmd+V is synthesised.
//! Explorer rename protection is a Windows-only concept, so those predicates are always false
//! here and the no-activate show/hide path is never taken.

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const WAIT_LIMIT: Duration = Duration::from_millis(1500);
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);
const INPUT_SETTLE: Duration = Duration::from_millis(80);
pub const CANCELLED: &str = "操作已取消";

/// One popup session. `Copy` because the shared flow passes it to several predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    session: u64,
    target_pid: i32,
}

#[derive(Default)]
struct Session {
    generation: u64,
    target_pid: i32,
    busy: bool,
}

impl Session {
    fn reset(&mut self, target_pid: i32) {
        self.generation = self.generation.wrapping_add(1);
        self.target_pid = target_pid;
        self.busy = false;
    }

    fn claim(&mut self) -> Result<Action, String> {
        if self.busy {
            return Err("正在处理上一条记录".into());
        }
        self.busy = true;
        Ok(Action {
            session: self.generation,
            target_pid: self.target_pid,
        })
    }

    fn current(&self, action: Action) -> bool {
        self.generation == action.session && self.target_pid == action.target_pid
    }

    fn finish(&mut self, action: Action) {
        if self.current(action) {
            self.busy = false;
        }
    }
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    generation: 0,
    target_pid: 0,
    busy: false,
});
static RETURNING: AtomicU32 = AtomicU32::new(0);

fn frontmost_pid() -> i32 {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
        .unwrap_or(0)
}

fn own_pid() -> i32 {
    std::process::id() as i32
}

pub fn owns_foreground() -> bool {
    frontmost_pid() == own_pid()
}

pub fn external_foreground() -> bool {
    let pid = frontmost_pid();
    pid > 0 && pid != own_pid()
}

/// Record the application that owned the keyboard before our panel became visible.
/// Returns false: macOS has no Explorer rename editor to preserve.
pub fn begin_session() -> bool {
    let pid = frontmost_pid();
    if let Ok(mut session) = SESSION.lock() {
        let target = if pid == own_pid() {
            session.target_pid
        } else {
            pid.max(0)
        };
        session.reset(target);
    }
    false
}

pub fn preserved_editor_active() -> bool {
    false
}

pub fn cancel() {
    if let Ok(mut session) = SESSION.lock() {
        session.reset(0);
    }
}

pub fn claim() -> Result<Action, String> {
    SESSION.lock().map_err(|e| e.to_string())?.claim()
}

pub fn dismiss() -> Result<Action, String> {
    let mut session = SESSION.lock().map_err(|e| e.to_string())?;
    let target = session.target_pid;
    session.reset(target);
    session.claim()
}

pub fn current(action: Action) -> bool {
    SESSION.lock().is_ok_and(|session| session.current(action))
}

pub fn preserves_editor(_action: Action) -> bool {
    false
}

pub fn foreground_changed() -> bool {
    if is_returning() || owns_foreground() {
        return false;
    }
    let pid = frontmost_pid().max(0);
    if let Ok(mut session) = SESSION.lock()
        && !is_returning()
        && session.target_pid != pid
    {
        session.reset(pid);
    }
    // The caller uses true specifically for Explorer's non-activating editor mode.
    false
}

pub fn finish(action: Action) {
    if let Ok(mut session) = SESSION.lock() {
        session.finish(action);
    }
}

pub fn is_returning() -> bool {
    RETURNING.load(Ordering::Acquire) != 0
}

pub struct Returning;

#[allow(clippy::new_without_default)]
impl Returning {
    pub fn new() -> Self {
        RETURNING.fetch_add(1, Ordering::AcqRel);
        Returning
    }
}

impl Drop for Returning {
    fn drop(&mut self) {
        RETURNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Put the original application back in front. Called on Tauri's main thread.
pub fn restore(action: Action) -> Result<(), String> {
    if !current(action) {
        return Err(CANCELLED.into());
    }
    let pid = action.target_pid;
    if pid <= 0 {
        return Err("没有可恢复的原输入窗口".into());
    }
    let foreground = frontmost_pid();
    if foreground == pid {
        return Ok(());
    }
    if foreground != own_pid() {
        return Err("已切换到其他窗口，自动粘贴已取消".into());
    }
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return Err("原输入应用已退出".into());
    };
    if !app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows) {
        return Err("无法恢复原输入窗口".into());
    }
    // Activation is asynchronous. Do not block the main run loop waiting for it: AppKit
    // needs that loop to resign our key window and restore the full-screen input window.
    // The worker's wait_ready below confirms activation before any key is sent.
    Ok(())
}

pub fn target_focused(action: Action) -> Result<bool, String> {
    if !current(action) {
        return Err(CANCELLED.into());
    }
    if action.target_pid <= 0 {
        return Err("没有可恢复的原输入窗口".into());
    }
    Ok(frontmost_pid() == action.target_pid)
}

pub fn diagnostic(action: Action) -> String {
    format!(
        "session={} target={} frontmost={} valid={}",
        action.session,
        action.target_pid,
        frontmost_pid(),
        current(action)
    )
}

/// Wait for held modifiers to be released before injecting Cmd+V.
pub fn wait_ready(action: Action, paste: bool) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_LIMIT;
    let mut focused_since = None;
    loop {
        if !current(action) {
            return Err(CANCELLED.into());
        }
        if target_focused(action)? && (!paste || crate::win::modifiers_released()) {
            let since = focused_since.get_or_insert_with(Instant::now);
            // NSWorkspace can report the new PID before its key window/first responder
            // accepts input. Require stable focus before synthesising Command-V.
            if !paste || since.elapsed() >= INPUT_SETTLE {
                return Ok(());
            }
        } else {
            focused_since = None;
        }
        if Instant::now() >= deadline {
            return Err("请松开修饰键并重新选择输入位置，内容已复制".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_session_allows_repeated_actions_but_not_concurrent_ones() {
        let mut session = Session::default();
        session.reset(123);
        let first = session.claim().unwrap();
        assert!(session.claim().is_err());
        session.finish(first);
        assert_eq!(session.claim().unwrap(), first);
    }

    #[test]
    fn old_worker_cannot_unlock_or_validate_a_new_session() {
        let mut session = Session::default();
        session.reset(123);
        let old = session.claim().unwrap();
        session.reset(456);
        let new = session.claim().unwrap();
        session.finish(old);
        assert!(!session.current(old));
        assert!(session.current(new));
        assert!(session.claim().is_err());
        session.finish(new);
        assert!(session.claim().is_ok());
    }
}
