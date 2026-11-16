//! macOS equivalents of the Win32 helpers in `win.rs`.
//!
//! Global hotkeys use Carbon's `RegisterEventHotKey` (via `global-hotkey`), which works without
//! Accessibility permission. Synthesising Cmd+V with `CGEvent` *does* require Accessibility
//! permission, so the first paste asks macOS to show the standard consent dialog.

#[path = "position.rs"]
mod position;
pub use position::{PopupArea, cursor_point, popup_area};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2_app_kit::{
    NSApplicationActivationOptions, NSRunningApplication, NSWindow, NSWindowCollectionBehavior,
    NSWorkspace,
};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// macOS virtual key code for the V key (kVK_ANSI_V).
const KVK_ANSI_V: u16 = 9;
/// macOS virtual key code for the ` key (kVK_ANSI_Grave).
const KVK_ANSI_GRAVE: u16 = 10;
/// Flag bits, kept in the tuple so `HOTKEYS` has the same shape on every platform.
const MOD_CTRL: u8 = 1;
const MOD_ALT: u8 = 2;
const MOD_SHIFT: u8 = 4;
const MOD_CMD: u8 = 8;

/// The ids are the Windows ones on purpose: an existing settings file stores an id, not a key
/// code, so a Mac can read (and a Windows machine can read back) the same settings unchanged.
/// Only the displayed labels use macOS symbols.
pub const HOTKEYS: &[(&str, u8, u16, &str)] = &[
    ("win+v", MOD_CMD | MOD_SHIFT, KVK_ANSI_V, "⌘⇧V"),
    ("ctrl+shift+v", MOD_CTRL | MOD_SHIFT, KVK_ANSI_V, "⌃⇧V"),
    ("alt+v", MOD_ALT, KVK_ANSI_V, "⌥V"),
    ("ctrl+alt+v", MOD_CTRL | MOD_ALT, KVK_ANSI_V, "⌃⌥V"),
    ("ctrl+backquote", MOD_CTRL, KVK_ANSI_GRAVE, "⌃`"),
];

fn hotkey_for(id: &str) -> Option<HotKey> {
    let (mods, key) = match id {
        "win+v" => (Modifiers::SUPER | Modifiers::SHIFT, Code::KeyV),
        "ctrl+shift+v" => (Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyV),
        "alt+v" => (Modifiers::ALT, Code::KeyV),
        "ctrl+alt+v" => (Modifiers::CONTROL | Modifiers::ALT, Code::KeyV),
        "ctrl+backquote" => (Modifiers::CONTROL, Code::Backquote),
        _ => return None,
    };
    Some(HotKey::new(Some(mods), key))
}

pub fn hotkey_label(id: &str) -> Option<&'static str> {
    HOTKEYS.iter().find(|h| h.0 == id).map(|h| h.3)
}

/// Current selection, shared between the settings command and the hotkey runtime.
static CONFIG: Mutex<(bool, Option<HotKey>)> = Mutex::new((true, None));

struct Runtime {
    manager: GlobalHotKeyManager,
    active: Option<HotKey>,
}

impl Runtime {
    fn reconfigure(&mut self) {
        if let Some(old) = self.active.take() {
            let _ = self.manager.unregister(old);
        }
        let (enabled, selected) = *CONFIG.lock().expect("hotkey config poisoned");
        if enabled && let Some(hotkey) = selected {
            match self.manager.register(hotkey) {
                Ok(()) => {
                    self.active = Some(hotkey);
                    log_diagnostic("hotkey-registered");
                }
                Err(e) => log_diagnostic(&format!("hotkey-register-failed {e}")),
            }
        }
    }
}

static HOTKEY_RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();

/// Returns false for an unknown hotkey id, matching the Windows implementation.
pub fn set_hotkey(enabled: bool, id: &str) -> bool {
    let Some(hotkey) = hotkey_for(id) else {
        return false;
    };
    *CONFIG.lock().expect("hotkey config poisoned") = (enabled, Some(hotkey));
    // Before setup finishes there is no manager yet; install_hotkey_hook registers the
    // selected hotkey when it creates one.
    if let Some(runtime) = HOTKEY_RUNTIME.get() {
        runtime
            .lock()
            .expect("hotkey runtime poisoned")
            .reconfigure();
    }
    true
}

/// Registers the Carbon event handler on the main thread (called from Tauri's `setup`) and
/// dispatches presses on a dedicated thread, mirroring the Windows hook dispatcher.
pub fn install_hotkey_hook(callback: impl Fn() + Send + Sync + 'static) {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("failed to install global hotkey handler: {e}");
            log_diagnostic(&format!("hotkey-handler-failed {e}"));
            return;
        }
    };
    let mut runtime = Runtime {
        manager,
        active: None,
    };
    runtime.reconfigure();
    let _ = HOTKEY_RUNTIME.set(Mutex::new(runtime));

    thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            match receiver.recv() {
                Ok(event) if event.state() == HotKeyState::Pressed => callback(),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
}

// Optional, count-only diagnostics: same opt-in file and format as the Windows build.
static DIAG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

pub fn start_hotkey_diagnostics(data_dir: &Path) {
    if !std::fs::read_to_string(data_dir.join("hotkey-diagnostics.enabled"))
        .map(|value| value.trim() == "enabled")
        .unwrap_or(false)
    {
        return;
    }
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(data_dir.join("hotkey-diagnostics.log"))
    {
        let _ = DIAG_FILE.set(Mutex::new(file));
        log_diagnostic("started");
    }
}

pub fn log_diagnostic(message: &str) {
    let Some(file) = DIAG_FILE.get() else { return };
    let Ok(mut file) = file.lock() else { return };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = writeln!(file, "{now} {message}");
}

/// On macOS console tools already emit UTF-8; there is no OEM code page to convert.
/// Kept so every platform module exposes the same helper names.
#[allow(dead_code)]
pub fn oem_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// WKWebView has no native caption or DWM transition to disable.
#[allow(dead_code)]
pub fn disable_transitions(_hwnd: ()) {}

static PREV_FOREGROUND_PID: AtomicI32 = AtomicI32::new(0);
static ACCESSIBILITY_ASKED: AtomicBool = AtomicBool::new(false);

/// Remember the app that was frontmost before our panel appears, so a paste can go back to it.
pub fn remember_foreground() {
    let workspace = NSWorkspace::sharedWorkspace();
    if let Some(app) = workspace.frontmostApplication() {
        let pid = app.processIdentifier();
        if pid > 0 && pid != std::process::id() as i32 {
            PREV_FOREGROUND_PID.store(pid, Ordering::Relaxed);
        }
    }
}

/// Bring our own panel forward. `WebviewWindow::show()` already activates the app on macOS;
/// this makes the activation explicit (and does not need a HWND).
pub fn force_foreground() {
    NSRunningApplication::currentApplication()
        .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
}

/// Called on the event-loop thread before showing either popup. Always-on-top alone only
/// raises a window inside its own Space; full-screen applications live in separate Spaces.
pub fn prepare_popup(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let ptr = window.ns_window()?;
    // SAFETY: Tauri owns this NSWindow, and both popup show paths run on the main thread.
    let native = unsafe { &*ptr.cast::<NSWindow>() };
    let mut behavior = native.collectionBehavior();
    behavior.remove(
        NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenPrimary
            | NSWindowCollectionBehavior::FullScreenNone,
    );
    behavior.insert(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    if objc2::available!(macos = 13.0) {
        // Stage Manager/full-screen membership flags are mutually exclusive.
        behavior
            .remove(NSWindowCollectionBehavior::Primary | NSWindowCollectionBehavior::Auxiliary);
        behavior.insert(NSWindowCollectionBehavior::CanJoinAllApplications);
    }
    native.setCollectionBehavior(behavior);
    log_diagnostic(&format!(
        "popup-spaces label={} behavior={:#x}",
        window.label(),
        behavior.bits()
    ));
    Ok(())
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // macOS `Boolean` is an unsigned char, not Rust's `bool`.
    fn AXIsProcessTrusted() -> u8;
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
}

/// Shows the system consent dialog once, the first time a synthetic key event is needed.
fn request_accessibility() {
    if unsafe { AXIsProcessTrusted() } != 0 {
        return;
    }
    if ACCESSIBILITY_ASKED.swap(true, Ordering::Relaxed) {
        return;
    }
    let key = CFString::new("AXTrustedCheckOptionPrompt");
    let value = CFBoolean::true_value();
    let pairs = [(key.as_CFType(), value.as_CFType())];
    let options = CFDictionary::from_CFType_pairs(&pairs);
    unsafe {
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast());
    }
    log_diagnostic("accessibility-permission-required");
    eprintln!(
        "clipkeep needs Accessibility permission to paste with Cmd+V: \
         System Settings > Privacy & Security > Accessibility"
    );
}

/// No Win32 foreground hook on macOS; the focus session tracker remembers the frontmost app.
#[allow(dead_code)]
pub fn start_foreground_tracking() {
    remember_foreground();
}

/// Explorer rename protection is Windows-only, so the no-activate show/hide path is unreachable
/// on macOS. These stubs keep the shared v0.2.4 flow compiling on Apple Silicon.
#[allow(dead_code)]
pub fn show_no_activate(_hwnd: ()) -> bool {
    false
}

#[allow(dead_code)]
pub fn hide_no_activate(_hwnd: ()) {}

/// Synthesise Cmd+V. `focus::restore` has already put the original application back in front.
pub fn can_send_paste() -> bool {
    if unsafe { AXIsProcessTrusted() } == 0 {
        log_diagnostic("paste-accessibility-denied");
        request_accessibility();
        return false;
    }
    true
}

pub fn send_paste() -> bool {
    if !can_send_paste() {
        return false;
    }
    log_diagnostic("paste-key-send");
    send_cmd_v()
}

/// Kept for parity with the Windows helper name; the v0.2.4 flow uses `focus::restore` first.
#[allow(dead_code)]
pub fn paste_to_previous_window() {
    let pid = PREV_FOREGROUND_PID.load(Ordering::Relaxed);
    if pid > 0
        && let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
    {
        app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
    thread::sleep(Duration::from_millis(80));
    request_accessibility();
    send_cmd_v();
}

pub fn modifiers_released() -> bool {
    let flags =
        unsafe { CGEventSourceFlagsState(CGEventSourceStateID::CombinedSessionState as i32) };
    let modifiers = CGEventFlags::CGEventFlagCommand
        | CGEventFlags::CGEventFlagControl
        | CGEventFlags::CGEventFlagAlternate
        | CGEventFlags::CGEventFlagShift;
    flags & modifiers.bits() == 0
}

fn send_cmd_v() -> bool {
    let source = match CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
        Ok(source) => source,
        Err(_) => return false,
    };
    let down = CGEvent::new_keyboard_event(source.clone(), KVK_ANSI_V, true);
    let up = CGEvent::new_keyboard_event(source, KVK_ANSI_V, false);
    if let (Ok(down), Ok(up)) = (down, up) {
        down.set_flags(CGEventFlags::CGEventFlagCommand);
        up.set_flags(CGEventFlags::CGEventFlagCommand);
        down.post(CGEventTapLocation::HID);
        up.post(CGEventTapLocation::HID);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkey_does_not_capture_system_paste() {
        let hotkey = hotkey_for("win+v").unwrap();
        assert_eq!(
            hotkey,
            HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyV)
        );
        assert_ne!(hotkey, HotKey::new(Some(Modifiers::SUPER), Code::KeyV));
        assert_eq!(hotkey_label("win+v"), Some("⌘⇧V"));
    }
}
