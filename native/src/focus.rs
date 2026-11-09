//! One original input target per popup session. Never infer a paste destination after hiding.
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, IsWindowEnabled, SetFocus, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GUITHREADINFO, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetParent, GetWindowThreadProcessId, IsHungAppWindow, IsWindow, SetForegroundWindow,
};

pub const WAIT_LIMIT: Duration = Duration::from_millis(500);
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);
pub const CANCELLED: &str = "操作已取消";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    window: usize,
    process: u32,
    thread: u32,
    control: usize,
    control_process: u32,
    control_thread: u32,
    preserve_editor: bool,
}

#[derive(Default)]
struct Session {
    generation: u64,
    target: Option<Target>,
    busy: bool,
}

impl Session {
    fn reset(&mut self, target: Option<Target>) {
        self.generation = self.generation.wrapping_add(1);
        self.target = target;
        self.busy = false;
    }

    fn refresh(&mut self, target: Option<Target>) {
        // Activation and child-focus events often describe the same destination. Such
        // duplicates must not release the busy guard or cancel an in-flight transfer.
        if self.target != target {
            self.reset(target);
        }
    }

    fn claim(&mut self) -> Result<Action, String> {
        if self.busy {
            return Err("正在处理上一条记录".into());
        }
        self.busy = true;
        Ok(Action {
            generation: self.generation,
            target: self.target,
        })
    }
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    generation: 0,
    target: None,
    busy: false,
});
static RETURNING: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug)]
pub struct Action {
    generation: u64,
    target: Option<Target>,
}

fn identity(window: HWND) -> (u32, u32) {
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(window, &mut process) };
    (process, thread)
}

fn gui_info(thread: u32) -> Option<GUITHREADINFO> {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    (unsafe { GetGUIThreadInfo(thread, &mut info) } != 0).then_some(info)
}

fn class_name(window: HWND) -> String {
    let mut name = [0u16; 128];
    let count = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..count.max(0) as usize])
}

fn is_explorer_edit(root_class: &str, control_class: &str, parent_classes: &[&str]) -> bool {
    matches!(
        root_class,
        "CabinetWClass" | "ExploreWClass" | "ExplorerWClass"
    ) && control_class == "Edit"
        && !parent_classes.iter().any(|class| {
            matches!(
                *class,
                "ComboBox" | "ComboBoxEx32" | "ReBarWindow32" | "ToolbarWindow32"
            ) || class.starts_with("Search")
        })
}

fn is_explorer_rename_editor(window: HWND, control: HWND) -> bool {
    let root_class = class_name(window);
    let control_class = class_name(control);
    if !matches!(
        root_class.as_str(),
        "CabinetWClass" | "ExploreWClass" | "ExplorerWClass"
    ) || control_class != "Edit"
    {
        return false;
    }

    // Explorer's address bar is also an Edit control, hosted by a ComboBox. Exclude that
    // family; a focused Edit elsewhere in an Explorer window is the inline file-name editor.
    let mut parent_classes = Vec::with_capacity(8);
    let mut parent = unsafe { GetParent(control) };
    for _ in 0..12 {
        if parent.is_null() {
            return false;
        }
        if parent == window {
            let parent_classes = parent_classes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            return is_explorer_edit(&root_class, &control_class, &parent_classes);
        }
        parent_classes.push(class_name(parent));
        parent = unsafe { GetParent(parent) };
    }
    false
}

pub fn owns_foreground() -> bool {
    let window = unsafe { GetForegroundWindow() };
    !window.is_null() && identity(window).0 == std::process::id()
}

pub fn external_foreground() -> bool {
    let window = unsafe { GetForegroundWindow() };
    !window.is_null() && identity(window).0 != std::process::id()
}

fn capture() -> Option<Target> {
    let window = unsafe { GetForegroundWindow() };
    let (process, thread) = identity(window);
    if window.is_null() || thread == 0 || process == std::process::id() {
        return None;
    }
    let control = gui_info(thread)
        .map(|info| info.hwndFocus)
        .unwrap_or(null_mut());
    let control = if !control.is_null() && unsafe { GetAncestor(control, GA_ROOT) } == window {
        control
    } else {
        null_mut()
    };
    let preserve_editor = is_explorer_rename_editor(window, control);
    let (control_process, control_thread) = identity(control);
    Some(Target {
        window: window as usize,
        process,
        thread,
        control: control as usize,
        control_process,
        control_thread,
        preserve_editor,
    })
}

/// Called before either our tray menu or main window activates. Own windows preserve the
/// captured tray target; an unavailable external foreground must clear any stale destination.
pub fn begin_session() -> bool {
    let own = owns_foreground();
    let captured = capture();
    if let Ok(mut session) = SESSION.lock() {
        let target = if own { session.target } else { captured };
        session.reset(target);
        return target.is_some_and(|target| target.preserve_editor);
    }
    false
}

/// True only while the captured Explorer edit control remains the active foreground target.
pub fn preserved_editor_active() -> bool {
    SESSION
        .lock()
        .ok()
        .and_then(|session| session.target)
        .filter(|target| target.preserve_editor)
        .is_some_and(target_is_focused)
}

pub fn cancel() {
    if let Ok(mut session) = SESSION.lock() {
        session.reset(None);
    }
}

pub fn claim() -> Result<Action, String> {
    SESSION.lock().map_err(|e| e.to_string())?.claim()
}

pub fn dismiss() -> Result<Action, String> {
    let mut session = SESSION.lock().map_err(|e| e.to_string())?;
    let target = session.target;
    session.reset(target);
    session.claim()
}

pub fn current(action: Action) -> bool {
    SESSION
        .lock()
        .is_ok_and(|session| session.generation == action.generation)
}

pub fn preserves_editor(action: Action) -> bool {
    action.target.is_some_and(|target| target.preserve_editor)
}

/// Refresh the destination when the user activates another app or changes its focused control.
/// The paste worker owns focus while it restores and injects input, so ignore those transitions.
pub fn foreground_changed() -> bool {
    if is_returning() || owns_foreground() {
        return false;
    }
    let target = capture();
    let preserve_editor = target.is_some_and(|target| target.preserve_editor);
    if let Ok(mut session) = SESSION.lock()
        && !is_returning()
    {
        session.refresh(target);
    }
    preserve_editor
}

pub fn finish(action: Action) {
    if let Ok(mut session) = SESSION.lock()
        && session.generation == action.generation
    {
        session.busy = false;
    }
}

pub fn is_returning() -> bool {
    RETURNING.load(Ordering::Acquire) != 0
}

pub struct Returning;
impl Returning {
    pub fn new() -> Self {
        RETURNING.fetch_add(1, Ordering::AcqRel);
        Self
    }
}
impl Drop for Returning {
    fn drop(&mut self) {
        RETURNING.fetch_sub(1, Ordering::AcqRel);
    }
}

struct InputAttachment {
    from: u32,
    to: u32,
}
impl InputAttachment {
    fn new(to: u32) -> Result<Option<Self>, String> {
        let from = unsafe { GetCurrentThreadId() };
        if from == to {
            return Ok(None);
        }
        if unsafe { AttachThreadInput(from, to, 1) } == 0 {
            return Err("无法连接原输入框的焦点线程".into());
        }
        Ok(Some(Self { from, to }))
    }
}
impl Drop for InputAttachment {
    fn drop(&mut self) {
        unsafe {
            AttachThreadInput(self.from, self.to, 0);
        }
    }
}

fn validate(target: Target) -> Result<(), String> {
    let window = target.window as HWND;
    if unsafe { IsWindow(window) } == 0 || identity(window) != (target.process, target.thread) {
        return Err("原输入窗口已关闭".into());
    }
    if unsafe { IsHungAppWindow(window) } != 0 {
        return Err("原输入窗口暂时无响应".into());
    }
    if target.control != 0 {
        let control = target.control as HWND;
        if unsafe { IsWindow(control) } == 0
            || unsafe { IsWindowEnabled(control) } == 0
            || identity(control) != (target.control_process, target.control_thread)
            || unsafe { GetAncestor(control, GA_ROOT) } != window
        {
            return Err("原输入框已失效，请重新选择输入位置".into());
        }
    }
    Ok(())
}

fn target_is_focused(target: Target) -> bool {
    if validate(target).is_err() || unsafe { GetForegroundWindow() } as usize != target.window {
        return false;
    }
    let thread = if target.control_thread != 0 {
        target.control_thread
    } else {
        target.thread
    };
    gui_info(thread).is_some_and(|info| {
        if target.control != 0 {
            info.hwndFocus as usize == target.control
        } else {
            !info.hwndFocus.is_null()
                && unsafe { GetAncestor(info.hwndFocus, GA_ROOT) } as usize == target.window
        }
    })
}

/// UI thread only. Hand focus away while we still own the foreground, before hiding.
pub fn restore(action: Action) -> Result<(), String> {
    if !current(action) {
        return Err(CANCELLED.into());
    }
    let target = action.target.ok_or("没有可恢复的原输入窗口")?;
    validate(target)?;
    if target.preserve_editor {
        return target_is_focused(target)
            .then_some(())
            .ok_or_else(|| "资源管理器重命名输入框已失去焦点，自动粘贴已取消".into());
    }
    let window = target.window as HWND;
    let foreground = unsafe { GetForegroundWindow() };
    if foreground != window && !owns_foreground() {
        return Err("已切换到其他窗口，自动粘贴已取消".into());
    }
    // Attach before activation, so the destination processes activation before SetFocus.
    // Attaching after SetForegroundWindow lets a delayed WM_SETFOCUS overwrite the child.
    let _attachment = InputAttachment::new(if target.control_thread != 0 {
        target.control_thread
    } else {
        target.thread
    })?;
    if unsafe { SetForegroundWindow(window) } == 0 && unsafe { GetForegroundWindow() } != window {
        return Err("无法激活原输入窗口，请手动粘贴".into());
    }
    if target.control != 0 {
        unsafe {
            SetFocus(target.control as HWND);
        }
    }
    Ok(())
}

pub fn target_focused(action: Action) -> Result<bool, String> {
    if !current(action) {
        return Err(CANCELLED.into());
    }
    let target = action.target.ok_or("没有可恢复的原输入窗口")?;
    validate(target)?;
    if target.preserve_editor {
        return Ok(target_is_focused(target));
    }
    let window = target.window as HWND;
    let foreground = unsafe { GetForegroundWindow() };
    if foreground != window {
        if !foreground.is_null() && !owns_foreground() {
            return Err("已切换到其他窗口，自动粘贴已取消".into());
        }
        return Ok(false);
    }
    let info = gui_info(if target.control_thread != 0 {
        target.control_thread
    } else {
        target.thread
    });
    Ok(info.is_some_and(|info| {
        if target.control != 0 {
            info.hwndFocus as usize == target.control
        } else {
            !info.hwndFocus.is_null() && unsafe { GetAncestor(info.hwndFocus, GA_ROOT) } == window
        }
    }))
}

pub fn diagnostic(action: Action) -> String {
    let foreground = unsafe { GetForegroundWindow() } as usize;
    let focused = action
        .target
        .and_then(|target| {
            gui_info(if target.control_thread != 0 {
                target.control_thread
            } else {
                target.thread
            })
        })
        .map(|info| info.hwndFocus as usize)
        .unwrap_or(0);
    format!(
        "generation={} current={} target={:?} foreground={foreground} focused={focused}",
        action.generation,
        current(action),
        action.target
    )
}

pub fn wait_ready(action: Action, paste: bool) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_LIMIT;
    let mut stable = false;
    loop {
        if target_focused(action)? && (!paste || modifiers_released()) {
            if stable {
                return Ok(());
            }
            stable = true;
        } else {
            stable = false;
        }
        if Instant::now() >= deadline {
            return Err("原输入框未能及时恢复焦点，内容已复制，请手动粘贴".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn modifiers_released() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .all(|key| unsafe { GetAsyncKeyState(*key as i32) } >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Target {
        Target {
            window: 1,
            process: 2,
            thread: 3,
            control: 4,
            control_process: 2,
            control_thread: 3,
            preserve_editor: false,
        }
    }
    #[test]
    fn one_action_per_session_and_new_session_cancels_old_action() {
        let mut session = Session::default();
        session.reset(Some(target()));
        let old = session.claim().unwrap();
        assert!(session.claim().is_err());
        session.reset(Some(Target {
            window: 9,
            ..target()
        }));
        let new = session.claim().unwrap();
        assert_ne!(old.generation, new.generation);
        assert_eq!(old.target.unwrap().window, 1);
        assert_eq!(new.target.unwrap().window, 9);
    }
    #[test]
    fn missing_new_target_cannot_reuse_previous_destination() {
        let mut session = Session::default();
        session.reset(Some(target()));
        session.reset(None);
        assert!(session.claim().unwrap().target.is_none());
    }

    #[test]
    fn duplicate_focus_events_preserve_busy_action_but_new_control_invalidates_it() {
        let mut session = Session::default();
        session.reset(Some(target()));
        let old = session.claim().unwrap();
        session.refresh(Some(target()));
        assert_eq!(session.generation, old.generation);
        assert!(session.claim().is_err());

        let next_control = Target {
            control: 5,
            ..target()
        };
        session.refresh(Some(next_control));
        let next = session.claim().unwrap();
        assert_ne!(next.generation, old.generation);
        assert_eq!(next.target, Some(next_control));
    }

    #[test]
    fn only_explorer_edit_controls_outside_navigation_bars_preserve_focus() {
        assert!(is_explorer_edit(
            "CabinetWClass",
            "Edit",
            &["DirectUIHWND", "SHELLDLL_DefView"]
        ));
        assert!(is_explorer_edit(
            "ExploreWClass",
            "Edit",
            &["SysListView32"]
        ));
        assert!(is_explorer_edit(
            "ExplorerWClass",
            "Edit",
            &["SysListView32"]
        ));
        assert!(!is_explorer_edit(
            "CabinetWClass",
            "Edit",
            &["ComboBox", "ComboBoxEx32"]
        ));
        assert!(!is_explorer_edit("CabinetWClass", "Static", &["Edit"]));
        assert!(!is_explorer_edit("Notepad", "Edit", &[]));
    }
}
