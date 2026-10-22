use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem::size_of;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{
    AtomicBool, AtomicIsize, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering,
};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Globalization::{CP_OEMCP, MultiByteToWideChar};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VK_CONTROL, VK_LMENU, VK_LWIN, VK_MENU, VK_OEM_3, VK_RMENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, IsHungAppWindow, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    MSG, SetForegroundWindow, SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
};

const VK_V: u16 = 0x56;
/// Unassigned virtual key. Windows opens the Start menu (Win) or the focused app's menu bar (Alt)
/// when the modifier is released with no other key seen in between; tapping this first prevents it.
const VK_MASK: u16 = 0xE8;
/// Distinguish our SendInput events from keyboard software and remote-input events.
const INJECT_TAG: usize = 0x434B_4550;
/// Windows can silently remove a low-level hook after a timeout. Reinstall it periodically.
const HOOK_REFRESH_MS: u32 = 30 * 1000;
/// A recent physical Win-down event can reach the hook before GetAsyncKeyState reflects it.
/// Keep the fallback short so a missed Win-up cannot make bare V trigger indefinitely.
const WIN_FALLBACK_MS: u32 = 1000;

const MOD_CTRL: u8 = 1;
const MOD_ALT: u8 = 2;
const MOD_SHIFT: u8 = 4;
const MOD_WIN: u8 = 8;

/// Hotkeys offered in settings: (id, modifiers, virtual key, label).
pub const HOTKEYS: &[(&str, u8, u16, &str)] = &[
    ("win+v", MOD_WIN, VK_V, "Win+V"),
    ("ctrl+shift+v", MOD_CTRL | MOD_SHIFT, VK_V, "Ctrl+Shift+V"),
    ("alt+v", MOD_ALT, VK_V, "Alt+V"),
    ("ctrl+alt+v", MOD_CTRL | MOD_ALT, VK_V, "Ctrl+Alt+V"),
    ("ctrl+backquote", MOD_CTRL, VK_OEM_3, "Ctrl+`"),
];

static ON_HOTKEY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static HOTKEY_ENABLED: AtomicBool = AtomicBool::new(true);
static HOTKEY_MODS: AtomicU8 = AtomicU8::new(MOD_WIN);
static HOTKEY_VK: AtomicU16 = AtomicU16::new(VK_V);
/// True from the first intercepted target-key down until its up (or a modifier release).
static KEY_SWALLOWED: AtomicBool = AtomicBool::new(false);
static PREV_FOREGROUND: AtomicIsize = AtomicIsize::new(0);
static WIN_TRACKED: AtomicU8 = AtomicU8::new(0);
static LAST_WIN_DOWN_MS: AtomicU32 = AtomicU32::new(0);

// Optional, count-only diagnostics. The keyboard hook only updates atomics; the dispatcher
// writes snapshots on its own thread when hotkey-diagnostics.enabled exists in app data.
static DIAG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
static HOOK_EVENT_COUNT: AtomicU64 = AtomicU64::new(0);
static WIN_DOWN_COUNT: AtomicU64 = AtomicU64::new(0);
static WIN_UP_COUNT: AtomicU64 = AtomicU64::new(0);
static TARGET_DOWN_COUNT: AtomicU64 = AtomicU64::new(0);
static TARGET_UP_COUNT: AtomicU64 = AtomicU64::new(0);
static TARGET_MATCH_COUNT: AtomicU64 = AtomicU64::new(0);
static DISPATCH_COUNT: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_INJECTED_DOWN_COUNT: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_INJECTED_UP_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_ASYNC_MODS: AtomicU8 = AtomicU8::new(0);
static LAST_EFFECTIVE_MODS: AtomicU8 = AtomicU8::new(0);
static LAST_TARGET_WAS_HELD: AtomicBool = AtomicBool::new(false);

/// Monotonically-increasing counter. The keyboard hook bumps this instead of calling the
/// callback directly — Windows may silently remove a hook that blocks too long, so we
/// cannot do any UI work (run_on_main_thread, etc.) inside the hook. A dedicated dispatcher
/// thread watches this counter and calls the actual callback outside the hook.
static HOTKEY_FIRE_COUNT: AtomicU64 = AtomicU64::new(0);
/// The dispatcher thread, unparked by the hook so a press is handled without polling delay.
static DISPATCHER: OnceLock<thread::Thread> = OnceLock::new();

pub fn start_hotkey_diagnostics(data_dir: &Path) {
    // Require an explicit opt-in so an empty marker cannot keep a release log growing forever.
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

fn diagnostic_snapshot() -> String {
    format!(
        "hook-events={} win-down={} win-up={} target-down={} target-up={} injected-down={} injected-up={} async-mods={} effective-mods={} target-was-held={} tracked-win={} matches={} fires={} dispatches={}",
        HOOK_EVENT_COUNT.load(Ordering::Relaxed),
        WIN_DOWN_COUNT.load(Ordering::Relaxed),
        WIN_UP_COUNT.load(Ordering::Relaxed),
        TARGET_DOWN_COUNT.load(Ordering::Relaxed),
        TARGET_UP_COUNT.load(Ordering::Relaxed),
        EXTERNAL_INJECTED_DOWN_COUNT.load(Ordering::Relaxed),
        EXTERNAL_INJECTED_UP_COUNT.load(Ordering::Relaxed),
        LAST_ASYNC_MODS.load(Ordering::Relaxed),
        LAST_EFFECTIVE_MODS.load(Ordering::Relaxed),
        LAST_TARGET_WAS_HELD.load(Ordering::Relaxed),
        WIN_TRACKED.load(Ordering::Relaxed),
        TARGET_MATCH_COUNT.load(Ordering::Relaxed),
        HOTKEY_FIRE_COUNT.load(Ordering::Relaxed),
        DISPATCH_COUNT.load(Ordering::Relaxed),
    )
}

pub fn hotkey_label(id: &str) -> Option<&'static str> {
    HOTKEYS.iter().find(|h| h.0 == id).map(|h| h.3)
}

/// Returns false for an unknown hotkey id.
pub fn set_hotkey(enabled: bool, id: &str) -> bool {
    let Some(&(_, mods, vk, _)) = HOTKEYS.iter().find(|h| h.0 == id) else {
        return false;
    };
    HOTKEY_MODS.store(mods, Ordering::Relaxed);
    HOTKEY_VK.store(vk, Ordering::Relaxed);
    HOTKEY_ENABLED.store(enabled, Ordering::Relaxed);
    KEY_SWALLOWED.store(false, Ordering::Relaxed);
    true
}

fn held(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

/// The current event is the target key, so the modifier keys have already changed state.
/// GetAsyncKeyState cannot describe the key *currently* entering a low-level hook.
fn current_mods() -> u8 {
    let mut m = 0u8;
    if held(VK_CONTROL) {
        m |= MOD_CTRL;
    }
    if held(VK_MENU) {
        m |= MOD_ALT;
    }
    if held(VK_SHIFT) {
        m |= MOD_SHIFT;
    }
    if held(VK_LWIN) || held(VK_RWIN) {
        m |= MOD_WIN;
    }
    m
}

fn effective_mods(async_mods: u8, tracked_win: u8, elapsed_since_win_down: u32) -> u8 {
    if tracked_win != 0 && elapsed_since_win_down <= WIN_FALLBACK_MS {
        async_mods | MOD_WIN
    } else {
        async_mods
    }
}

fn install_hook() -> HHOOK {
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), null_mut(), 0) }
}

/// Install the keyboard hook and start the dispatcher thread.
///
/// The hook itself never calls the callback — it only increments HOTKEY_FIRE_COUNT, unparks the
/// dispatcher thread and returns immediately. The dispatcher sees the counter change and calls
/// the callback there (outside the hook's timeout).
pub fn install_hotkey_hook(callback: impl Fn() + Send + Sync + 'static) {
    if ON_HOTKEY.set(Box::new(callback)).is_err() {
        return;
    }

    // Dispatcher thread: calls once for each physical press, outside the hook deadline.
    let dispatcher = thread::spawn(|| {
        let mut last_seen: u64 = 0;
        let mut last_snapshot = String::new();
        let mut next_snapshot = Instant::now();
        loop {
            // The timeout only paces diagnostic snapshots; presses unpark the thread at once.
            thread::park_timeout(Duration::from_secs(1));
            let cur = HOTKEY_FIRE_COUNT.load(Ordering::Relaxed);
            while last_seen < cur {
                last_seen += 1;
                DISPATCH_COUNT.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = ON_HOTKEY.get() {
                    cb();
                }
            }
            if DIAG_FILE.get().is_some() && Instant::now() >= next_snapshot {
                let snapshot = diagnostic_snapshot();
                if snapshot != last_snapshot {
                    log_diagnostic(&snapshot);
                    last_snapshot = snapshot;
                }
                next_snapshot = Instant::now() + Duration::from_secs(1);
            }
        }
    });
    let _ = DISPATCHER.set(dispatcher.thread().clone());

    // Hook thread: owns the message pump required for WH_KEYBOARD_LL.
    thread::spawn(|| unsafe {
        let mut hook = install_hook();
        if hook.is_null() {
            log_diagnostic(&format!("hook-install-failed error={}", GetLastError()));
            eprintln!("failed to install hotkey keyboard hook");
        } else {
            log_diagnostic("hook-installed");
        }
        if SetTimer(null_mut(), 0, HOOK_REFRESH_MS, None) == 0 {
            log_diagnostic(&format!(
                "hook-refresh-timer-failed error={}",
                GetLastError()
            ));
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER && msg.hwnd.is_null() {
                let fresh = install_hook();
                if !fresh.is_null() {
                    let old_hook = hook;
                    hook = fresh;
                    // Windows can silently remove an expired hook, then reuse its handle for the
                    // new one. Never unhook that reused handle or we remove the new hook instead.
                    if !old_hook.is_null()
                        && old_hook != fresh
                        && UnhookWindowsHookEx(old_hook) == 0
                    {
                        log_diagnostic(&format!("old-hook-unhook-failed error={}", GetLastError()));
                    }
                    log_diagnostic("hook-refreshed");
                } else {
                    log_diagnostic(&format!("hook-refresh-failed error={}", GetLastError()));
                }
                // A previous hook may have timed out and missed the key-up event.
                if !held(HOTKEY_VK.load(Ordering::Relaxed)) {
                    KEY_SWALLOWED.store(false, Ordering::Relaxed);
                }
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

fn modifier_bit(vk: u16) -> u8 {
    match vk {
        VK_CONTROL | 0xA2 | 0xA3 => MOD_CTRL,
        VK_SHIFT | 0xA0 | 0xA1 => MOD_SHIFT,
        VK_LWIN | VK_RWIN => MOD_WIN,
        VK_LMENU | VK_RMENU | VK_MENU => MOD_ALT,
        _ => 0,
    }
}

#[derive(Debug, PartialEq)]
enum TargetAction {
    Pass,
    Swallow,
    Fire,
}

fn target_key_action(down: bool, matches: bool) -> TargetAction {
    if down {
        if !matches {
            KEY_SWALLOWED.store(false, Ordering::Relaxed);
            return TargetAction::Pass;
        }
        if KEY_SWALLOWED.swap(true, Ordering::Relaxed) {
            TargetAction::Swallow // key repeat
        } else {
            TargetAction::Fire
        }
    } else if KEY_SWALLOWED.swap(false, Ordering::Relaxed) {
        TargetAction::Swallow
    } else {
        TargetAction::Pass
    }
}

/// Keep this callback short: Windows may silently remove a low-level hook that blocks.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) };
    }
    HOOK_EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
    let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    if kb.flags & LLKHF_INJECTED != 0 && kb.dwExtraInfo == INJECT_TAG {
        return unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) };
    }
    let down = match wparam as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) },
    };
    let vk = kb.vkCode as u16;
    let win_side = match vk {
        VK_LWIN => 1,
        VK_RWIN => 2,
        _ => 0,
    };
    if kb.flags & LLKHF_INJECTED != 0 && (win_side != 0 || vk == HOTKEY_VK.load(Ordering::Relaxed))
    {
        if down {
            EXTERNAL_INJECTED_DOWN_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            EXTERNAL_INJECTED_UP_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    if win_side != 0 {
        if down {
            WIN_DOWN_COUNT.fetch_add(1, Ordering::Relaxed);
            WIN_TRACKED.fetch_or(win_side, Ordering::Relaxed);
            LAST_WIN_DOWN_MS.store(kb.time, Ordering::Relaxed);
        } else {
            WIN_UP_COUNT.fetch_add(1, Ordering::Relaxed);
            WIN_TRACKED.fetch_and(!win_side, Ordering::Relaxed);
        }
    }
    let modifier = modifier_bit(vk);
    if modifier != 0 {
        if down {
            // Win passes through the hook, so its async state distinguishes a new chord from
            // key repeat. Recover if an older hook missed the previous V/Win key-up events.
            if modifier == MOD_WIN && !held(vk) {
                KEY_SWALLOWED.store(false, Ordering::Relaxed);
            }
        } else {
            if HOTKEY_MODS.load(Ordering::Relaxed) & modifier != 0 {
                KEY_SWALLOWED.store(false, Ordering::Relaxed);
            }
        }
    }

    if vk == HOTKEY_VK.load(Ordering::Relaxed) {
        if down {
            TARGET_DOWN_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            TARGET_UP_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        let async_mods = if down { current_mods() } else { 0 };
        let target_was_held = down && held(vk);
        let mods = effective_mods(
            async_mods,
            WIN_TRACKED.load(Ordering::Relaxed),
            kb.time
                .wrapping_sub(LAST_WIN_DOWN_MS.load(Ordering::Relaxed)),
        );
        if down {
            LAST_ASYNC_MODS.store(async_mods, Ordering::Relaxed);
            LAST_EFFECTIVE_MODS.store(mods, Ordering::Relaxed);
            LAST_TARGET_WAS_HELD.store(target_was_held, Ordering::Relaxed);
        }
        let matches = down
            && HOTKEY_ENABLED.load(Ordering::Relaxed)
            && mods == HOTKEY_MODS.load(Ordering::Relaxed);
        if matches {
            TARGET_MATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        match target_key_action(down, matches) {
            TargetAction::Pass => {}
            TargetAction::Swallow => return 1,
            TargetAction::Fire => {
                // EcoPaste masks Win on the first V-down. Doing this before changing focus
                // avoids turning the swallowed chord into a lone Win key on release.
                if HOTKEY_MODS.load(Ordering::Relaxed) & (MOD_WIN | MOD_ALT) != 0 {
                    send_keys(&[(VK_MASK, false), (VK_MASK, true)]);
                }
                HOTKEY_FIRE_COUNT.fetch_add(1, Ordering::Relaxed);
                if let Some(dispatcher) = DISPATCHER.get() {
                    dispatcher.unpark();
                }
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// Console tools such as schtasks write in the OEM code page (GBK on Chinese Windows), not UTF-8.
pub fn oem_to_string(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    unsafe {
        let wide_len = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, null_mut(), 0);
        if wide_len <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; wide_len as usize];
        MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            len,
            wide.as_mut_ptr(),
            wide_len,
        );
        String::from_utf16_lossy(&wide)
    }
}

/// Windows plays its open/close animation for captioned windows, which Tauri keeps even without
/// decorations. A popup panel should appear and disappear at once.
pub fn disable_transitions(hwnd: HWND) {
    let disabled: i32 = 1;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            (&disabled as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
}

/// Call right before showing our window, so a paste can go back to where the user was typing.
pub fn remember_foreground() {
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if !hwnd.is_null() && pid != std::process::id() {
            PREV_FOREGROUND.store(hwnd as isize, Ordering::Relaxed);
        }
    }
}

/// Windows only lets the process that owns the foreground window hand focus away, so borrow its
/// input queue for the switch. Skipped for a hung app, whose queue could stall ours. UI thread only.
pub fn force_foreground(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, null_mut());
        let me = GetCurrentThreadId();
        let attached = !fg.is_null()
            && IsHungAppWindow(fg) == 0
            && fg_thread != 0
            && fg_thread != me
            && AttachThreadInput(me, fg_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(me, fg_thread, 0);
        }
    }
}

pub fn paste_to_previous_window() {
    let prev = PREV_FOREGROUND.load(Ordering::Relaxed) as HWND;
    if !prev.is_null() {
        unsafe { SetForegroundWindow(prev) };
    }
    thread::sleep(Duration::from_millis(80));
    send_keys(&[
        (VK_CONTROL, false),
        (VK_V, false),
        (VK_V, true),
        (VK_CONTROL, true),
    ]);
}

fn send_keys<const N: usize>(keys: &[(u16, bool); N]) -> bool {
    let inputs: [INPUT; N] = std::array::from_fn(|i| {
        let (vk, up) = keys[i];
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: INJECT_TAG,
                },
            },
        }
    });
    unsafe { SendInput(N as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) == N as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_fire_per_press_with_repeat_and_fast_repress() {
        KEY_SWALLOWED.store(false, Ordering::Relaxed);
        assert_eq!(target_key_action(true, true), TargetAction::Fire);
        for _ in 0..30 {
            assert_eq!(target_key_action(true, true), TargetAction::Swallow);
        }
        assert_eq!(target_key_action(false, false), TargetAction::Swallow);
        assert_eq!(target_key_action(true, true), TargetAction::Fire);
        assert_eq!(target_key_action(false, false), TargetAction::Swallow);
        assert_eq!(target_key_action(true, false), TargetAction::Pass);
        assert_eq!(target_key_action(true, true), TargetAction::Fire);
        assert_eq!(target_key_action(true, true), TargetAction::Swallow);
        KEY_SWALLOWED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn recent_win_press_covers_stale_async_state_without_leaking_to_later_v() {
        assert_eq!(effective_mods(0, 1, 20), MOD_WIN);
        assert_eq!(effective_mods(0, 1, WIN_FALLBACK_MS + 1), 0);
        assert_eq!(effective_mods(MOD_WIN, 0, 5000), MOD_WIN);
    }
}
