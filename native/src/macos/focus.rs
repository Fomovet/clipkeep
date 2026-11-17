//! macOS counterpart of the Win32 focus session tracker (`native/src/focus.rs`).
//!
//! It keeps the same public surface so `lib.rs` can share the v0.2.4 return-to-input flow on
//! every platform. The frontmost application is remembered with `NSWorkspace` and restored with
//! `NSRunningApplication` before Cmd+V is synthesised.
//!
//! When Accessibility permission is already granted (the same TCC right auto-paste needs), the
//! session also snapshots the focused window and UI element via the Accessibility API and, on
//! restore, raises that window and focuses that element. This is best-effort: many browsers and
//! some toolkits do not expose a stable control, an element can go stale if the UI is rebuilt,
//! and GitHub Actions cannot grant TCC, so there are no interactive desktop regression tests.
//! A failed Accessibility restore never cancels paste; the app-level activation still runs.
//!
//! Explorer rename protection is a Windows-only concept, so those predicates are always false
//! here and the no-activate show/hide path is never taken.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const WAIT_LIMIT: Duration = Duration::from_millis(1500);
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);
const INPUT_SETTLE: Duration = Duration::from_millis(80);
/// Bound every Accessibility call so a hung target app cannot stall paste.
const AX_TIMEOUT_SECS: f32 = 0.5;
pub const CANCELLED: &str = "操作已取消";

type AXUIElementRef = CFTypeRef;
type AXError = i32;
const AX_OK: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFTypeRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFTypeRef) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> AXError;
}

/// Owned Accessibility element. Held in the session, not in `Action`, so `Action` stays `Copy`.
struct AxElement(CFType);

// SAFETY: AXUIElement is a CFType that can be messaged from any thread; the session mutex
// is what serialises our use of a given snapshot.
unsafe impl Send for AxElement {}
unsafe impl Sync for AxElement {}

impl AxElement {
    /// Takes ownership of a +1 retained object.
    fn from_create(raw: AXUIElementRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            // SAFETY: `raw` is a newly created or copied CF object the caller owns.
            Some(Self(unsafe { CFType::wrap_under_create_rule(raw) }))
        }
    }

    fn as_ref(&self) -> AXUIElementRef {
        self.0.as_CFTypeRef()
    }
}

struct AxTarget {
    window: AxElement,
    element: Option<AxElement>,
}

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
    ax: Option<AxTarget>,
}

impl Session {
    fn reset(&mut self, target_pid: i32) {
        self.reset_with_ax(target_pid, None);
    }

    fn reset_with_ax(&mut self, target_pid: i32, ax: Option<AxTarget>) {
        self.generation = self.generation.wrapping_add(1);
        self.target_pid = target_pid;
        self.ax = ax;
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
    ax: None,
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

fn cf_string(name: &str) -> CFString {
    CFString::new(name)
}

fn ax_copy(element: AXUIElementRef, attribute: &str) -> Option<AxElement> {
    let attr = cf_string(attribute);
    let mut value: AXUIElementRef = std::ptr::null();
    let status = unsafe { AXUIElementCopyAttributeValue(element, attr.as_CFTypeRef(), &mut value) };
    if status != AX_OK {
        return None;
    }
    AxElement::from_create(value)
}

fn ax_application(pid: i32) -> Option<AxElement> {
    let element = AxElement::from_create(unsafe { AXUIElementCreateApplication(pid) })?;
    unsafe {
        AXUIElementSetMessagingTimeout(element.as_ref(), AX_TIMEOUT_SECS);
    }
    Some(element)
}

/// Snapshot the focused window and control. `None` when Accessibility is not granted, the
/// process has no focused window, or the target does not expose a usable hierarchy.
fn capture_ax(pid: i32) -> Option<AxTarget> {
    if pid <= 0 || pid == own_pid() || !crate::win::accessibility_trusted() {
        return None;
    }
    let app = ax_application(pid)?;
    let window = ax_copy(app.as_ref(), "AXFocusedWindow")?;
    let element = ax_copy(app.as_ref(), "AXFocusedUIElement")
        .or_else(|| ax_copy(window.as_ref(), "AXFocusedUIElement"));
    Some(AxTarget { window, element })
}

fn raise_ax(target: &AxTarget) {
    let action = cf_string("AXRaise");
    let raised =
        unsafe { AXUIElementPerformAction(target.window.as_ref(), action.as_CFTypeRef()) } == AX_OK;
    crate::win::log_diagnostic(&format!("ax-raise ok={raised}"));
    if let Some(element) = &target.element {
        let attr = cf_string("AXFocused");
        let value = CFBoolean::true_value();
        let focused = unsafe {
            AXUIElementSetAttributeValue(
                element.as_ref(),
                attr.as_CFTypeRef(),
                value.as_CFTypeRef(),
            )
        } == AX_OK;
        crate::win::log_diagnostic(&format!("ax-focus ok={focused}"));
    }
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
        if pid == own_pid() {
            let target = session.target_pid;
            let ax = session.ax.take();
            session.reset_with_ax(target, ax);
        } else {
            let target = pid.max(0);
            let ax = capture_ax(target);
            crate::win::log_diagnostic(&format!(
                "focus-capture pid={target} ax_window={} ax_element={}",
                ax.is_some(),
                ax.as_ref().is_some_and(|ax| ax.element.is_some())
            ));
            session.reset_with_ax(target, ax);
        }
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
    let ax = session.ax.take();
    session.reset_with_ax(target, ax);
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
        let ax = capture_ax(pid);
        session.reset_with_ax(pid, ax);
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
        // Already frontmost: still try to put the original control back, then we are done.
        restore_ax(action);
        return Ok(());
    }
    if foreground != own_pid() {
        return Err("已切换到其他窗口，自动粘贴已取消".into());
    }
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return Err("原输入应用已退出".into());
    };
    // ActivateAllWindows is kept so a full-screen or Stage Manager destination can come
    // forward when we have no window-level Accessibility snapshot. When we do, the raise
    // below still aims at the captured window after this activation.
    if !app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows) {
        return Err("无法恢复原输入窗口".into());
    }
    restore_ax(action);
    // Activation is asynchronous. Do not block the main run loop waiting for it: AppKit
    // needs that loop to resign our key window and restore the full-screen input window.
    // The worker's wait_ready below confirms activation before any key is sent.
    Ok(())
}

fn restore_ax(action: Action) {
    if let Ok(session) = SESSION.lock()
        && session.current(action)
        && let Some(ax) = &session.ax
    {
        raise_ax(ax);
    }
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
    let (ax_window, ax_element) = SESSION
        .lock()
        .ok()
        .and_then(|session| session.ax.as_ref().map(|ax| (true, ax.element.is_some())))
        .unwrap_or((false, false));
    format!(
        "session={} target={} frontmost={} valid={} ax_window={} ax_element={} ax_trusted={}",
        action.session,
        action.target_pid,
        frontmost_pid(),
        current(action),
        ax_window,
        ax_element,
        crate::win::accessibility_trusted()
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

    #[test]
    fn reset_without_ax_is_the_permissionless_fallback() {
        let mut session = Session::default();
        session.reset_with_ax(123, None);
        assert!(session.ax.is_none());
        let action = session.claim().unwrap();
        assert_eq!(action.target_pid, 123);
        session.reset(456);
        assert!(session.ax.is_none());
        assert!(!session.current(action));
    }
}
