//! Interactive-desktop regression fixture for the production Windows focus code.
//! Run two copies (`--target` and default), drive JSON lines through stdin/stdout.
//! Only creates disposable test windows; never attaches to a user document.
#![allow(dead_code)]
#[path = "../src/focus.rs"]
mod focus;
#[path = "../src/win.rs"]
mod win;

use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{self, BufRead, Write};
use std::ptr::null_mut;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const REQUEST: u32 = WM_APP + 71;
struct Request {
    value: Value,
    reply: SyncSender<Value>,
}
#[derive(Default)]
struct State {
    window: usize,
    fields: Vec<usize>,
    action: Option<focus::Action>,
    clipboard: Option<Vec<(u32, Vec<u8>)>>,
}
struct ClipboardOpen;
impl ClipboardOpen {
    fn new(window: HWND) -> Result<Self, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while unsafe { OpenClipboard(window) } == 0 {
            if Instant::now() >= deadline {
                return Err("cannot open clipboard".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Self)
    }
}
impl Drop for ClipboardOpen {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
fn set_clipboard_format(format: u32, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if memory.is_null() {
            return Err("cannot allocate clipboard memory".into());
        }
        let buffer = GlobalLock(memory);
        if buffer.is_null() {
            windows_sys::Win32::Foundation::GlobalFree(memory);
            return Err("cannot lock clipboard memory".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        GlobalUnlock(memory);
        if SetClipboardData(format, memory).is_null() {
            windows_sys::Win32::Foundation::GlobalFree(memory);
            return Err("cannot write clipboard format".into());
        }
    }
    Ok(())
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn text(window: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(window) };
    let mut buffer = vec![0; len as usize + 1];
    let len = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..len as usize])
}

fn execute(value: Value) -> Result<Value, String> {
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let window = state.window as HWND;
        let command = value["cmd"].as_str().ok_or("missing command")?;
        match command {
            "focus" => {
                let index = value["index"].as_u64().unwrap_or(0) as usize;
                let field = *state.fields.get(index).ok_or("missing field")? as HWND;
                unsafe {
                    ShowWindow(window, SW_SHOW);
                }
                win::force_foreground(window);
                unsafe {
                    SetWindowTextW(
                        field,
                        wide(value["text"].as_str().unwrap_or("左右")).as_ptr(),
                    );
                    SetFocus(field);
                    let caret = value["caret"].as_u64().unwrap_or(1) as usize;
                    SendMessageW(field, 0x00B1, caret, caret as isize); // EM_SETSEL
                }
                Ok(json!(unsafe { GetForegroundWindow() } == window))
            }
            "capture" => {
                focus::begin_session();
                state.action = Some(focus::claim()?);
                Ok(json!(true))
            }
            "show" => {
                unsafe {
                    ShowWindow(window, SW_SHOW);
                }
                win::force_foreground(window);
                Ok(json!(focus::owns_foreground()))
            }
            "hotkey" => {
                let keys = [(0x11, false), (0x12, false), (0x56, false), (0x56, true), (0x12, true), (0x11, true)];
                let inputs = keys.map(|(vk, up)| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, ..Default::default() } },
                });
                Ok(json!(unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) } == inputs.len() as u32))
            }
            "submit" => {
                let keys: &[u16] = match value["method"].as_str() {
                    Some("enter") => &[13],
                    Some("escape") => &[27],
                    Some("copy-only") => &[0x10, 13],
                    Some("ctrl-number") => &[0x11, 0x31],
                    _ => return Err("unsupported fixture input".into()),
                };
                let inputs: Vec<INPUT> = keys.iter().map(|vk| (*vk, false))
                    .chain(keys.iter().rev().map(|vk| (*vk, true)))
                    .map(|(vk, up)| INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, ..Default::default() } },
                    }).collect();
                Ok(json!(unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) } == inputs.len() as u32))
            }
            "foreground-process" => {
                let mut process = 0;
                unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process); }
                Ok(json!(process))
            }
            "modifier" => {
                let input = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT {
                        wVk: 0x11,
                        dwFlags: if value["down"].as_bool().unwrap_or(false) { 0 } else { KEYEVENTF_KEYUP },
                        ..Default::default()
                    } },
                };
                Ok(json!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) } == 1))
            }
            "status" => {
                let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
                unsafe { GetGUIThreadInfo(0, &mut info); }
                Ok(json!({"foreground": unsafe { GetForegroundWindow() } == window,
                    "field": state.fields.iter().position(|field| *field == info.hwndFocus as usize)}))
            }
            "return" => {
                let action = state.action.ok_or("missing action")?;
                let result = {
                    let _returning = focus::Returning::new();
                    focus::restore(action)
                };
                if result.is_ok() {
                    unsafe {
                        ShowWindow(window, SW_HIDE);
                    }
                    focus::wait_ready(action, value["paste"].as_bool().unwrap_or(false))
                        .map_err(|error| format!("{error}: {}", focus::diagnostic(action)))?;
                    if value["paste"].as_bool().unwrap_or(false) && !win::send_paste() {
                        return Err("SendInput failed".into());
                    }
                }
                focus::finish(action);
                result.map_err(|error| format!("{error}: {}", focus::diagnostic(action)))?;
                Ok(json!(true))
            }
            "clipboard" => {
                let _clipboard = ClipboardOpen::new(window)?;
                if state.clipboard.is_none() {
                    let mut saved = Vec::new();
                    let mut format = 0;
                    let mut bitmap = false;
                    loop {
                        format = unsafe { EnumClipboardFormats(format) };
                        if format == 0 {
                            break;
                        }
                        if format == 2 {
                            bitmap = true;
                            continue;
                        } // Synthesized from DIB on restore.
                        if [3, 9, 14].contains(&format) {
                            return Err(
                                "non-memory clipboard format; test will not replace it".into()
                            );
                        }
                        let memory = unsafe { GetClipboardData(format) };
                        let len = unsafe { GlobalSize(memory) };
                        if len == 0 || len > 256 * 1024 * 1024 {
                            return Err(
                                "unsupported clipboard data; test will not replace it".into()
                            );
                        }
                        let buffer = unsafe { GlobalLock(memory) };
                        if buffer.is_null() {
                            return Err("cannot snapshot clipboard".into());
                        }
                        let bytes = unsafe {
                            std::slice::from_raw_parts(buffer as *const u8, len).to_vec()
                        };
                        unsafe {
                            GlobalUnlock(memory);
                        }
                        saved.push((format, bytes));
                    }
                    if bitmap && !saved.iter().any(|(format, _)| [8, 17].contains(format)) {
                        return Err("bitmap without DIB; test will not replace it".into());
                    }
                    state.clipboard = Some(saved);
                }
                if unsafe { EmptyClipboard() } == 0 {
                    return Err("cannot clear clipboard".into());
                }
                let words = wide(value["text"].as_str().unwrap_or("焦点验收"));
                let bytes = words
                    .iter()
                    .flat_map(|word| word.to_le_bytes())
                    .collect::<Vec<_>>();
                set_clipboard_format(13, &bytes)?;
                Ok(json!(true))
            }
            "restore-clipboard" => {
                if let Some(saved) = state.clipboard.as_ref() {
                    let _clipboard = ClipboardOpen::new(window)?;
                    if unsafe { EmptyClipboard() } == 0 {
                        return Err("cannot clear clipboard".into());
                    }
                    for (format, bytes) in saved {
                        set_clipboard_format(*format, bytes)?;
                    }
                    state.clipboard = None;
                }
                Ok(json!(true))
            }
            "image" => {
                if state.clipboard.is_none() { return Err("snapshot clipboard first".into()); }
                let side = 4096u32;
                let mut dib = vec![255u8; 40 + (side * side * 4) as usize];
                dib[..40].fill(0);
                dib[..4].copy_from_slice(&40u32.to_le_bytes());
                dib[4..8].copy_from_slice(&side.to_le_bytes());
                dib[8..12].copy_from_slice(&side.to_le_bytes());
                dib[12..14].copy_from_slice(&1u16.to_le_bytes());
                dib[14..16].copy_from_slice(&32u16.to_le_bytes());
                let _clipboard = ClipboardOpen::new(window)?;
                if unsafe { EmptyClipboard() } == 0 { return Err("cannot clear clipboard".into()); }
                set_clipboard_format(8, &dib)?;
                Ok(json!(true))
            }
            "text" => Ok(json!(
                state
                    .fields
                    .iter()
                    .map(|handle| text(*handle as HWND))
                    .collect::<Vec<_>>()
            )),
            "busy" => Ok(json!(focus::claim().is_err())),
            "stale" => {
                focus::begin_session();
                Ok(json!(
                    focus::restore(state.action.ok_or("missing action")?).is_err()
                ))
            }
            "cancel" => {
                focus::cancel();
                unsafe {
                    ShowWindow(window, SW_HIDE);
                }
                Ok(json!(true))
            }
            "destroy-field" => {
                let index = value["index"].as_u64().unwrap_or(0) as usize;
                let field = *state.fields.get(index).ok_or("missing field")? as HWND;
                unsafe {
                    DestroyWindow(field);
                }
                Ok(json!(true))
            }
            "quit" => {
                unsafe {
                    PostQuitMessage(0);
                }
                Ok(json!(true))
            }
            _ => Err("unknown command".into()),
        }
    })
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == REQUEST {
        let request = unsafe { Box::from_raw(l as *mut Request) };
        let result = execute(request.value);
        let _ = request.reply.send(match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        });
        0
    } else {
        unsafe { DefWindowProcW(window, message, w, l) }
    }
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let explorer_target = args.iter().any(|arg| arg == "--explorer-target");
    let target = explorer_target || args.iter().any(|arg| arg == "--target");
    let class = if explorer_target {
        "CabinetWClass"
    } else {
        "STATIC"
    };
    let class_wide = wide(class);
    if explorer_target {
        let mut info: WNDCLASSW = unsafe { std::mem::zeroed() };
        info.lpfnWndProc = Some(procedure);
        info.hInstance = unsafe { GetModuleHandleW(null_mut()) };
        info.lpszClassName = class_wide.as_ptr();
        assert_ne!(
            unsafe { RegisterClassW(&info) },
            0,
            "register Explorer fixture class"
        );
    }
    let window = unsafe {
        CreateWindowExW(
            0,
            class_wide.as_ptr(),
            wide(if explorer_target {
                "Disposable Explorer rename fixture"
            } else if target {
                "Clipkeep native input fixture"
            } else {
                "Clipkeep focus probe"
            })
            .as_ptr(),
            WS_OVERLAPPEDWINDOW,
            if target { 50 } else { 650 },
            100,
            480,
            220,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    assert!(!window.is_null());
    if !explorer_target {
        unsafe {
            SetWindowLongPtrW(window, GWLP_WNDPROC, procedure as *const () as isize);
        }
    }
    let mut fields = Vec::new();
    for i in 0..2 {
        let field = unsafe {
            CreateWindowExW(
                WS_EX_CLIENTEDGE,
                wide("EDIT").as_ptr(),
                wide("左右").as_ptr(),
                WS_VISIBLE | WS_CHILD | WS_TABSTOP | 0x80,
                20,
                20 + i * 55,
                410,
                36,
                window,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        assert!(!field.is_null());
        fields.push(field as usize);
    }
    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            window: window as usize,
            fields,
            action: None,
            clipboard: None,
        }
    });
    if target {
        unsafe {
            ShowWindow(window, SW_SHOW);
        }
    }
    println!("QA:{}", json!({"ready":true,"window":window as usize}));
    io::stdout().flush().unwrap();
    let handle = window as usize;
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (reply, result) = sync_channel(1);
            let request = Box::into_raw(Box::new(Request { value, reply }));
            if unsafe { PostMessageW(handle as HWND, REQUEST, 0, request as isize) } == 0 {
                break;
            }
            let Ok(value) = result.recv() else { break };
            println!("QA:{value}");
            let _ = io::stdout().flush();
        }
    });
    let mut message: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DestroyWindow(window);
    }
}
