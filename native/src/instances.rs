//! Taking over from an older copy of clipkeep.
//!
//! Upgrading from the single-file build to the installer leaves the old exe where it was: still
//! running in the tray, and still the target of the logon task. Since only one instance may run,
//! opening the new version then just shows the old one, and every logon starts the old one again.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};

/// Window class of the single-instance plugin's message window; every version registers it.
const INSTANCE_WINDOW_CLASS: &str = "com.clipkeep.clipboard-sic";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Product version of an exe ("0.2.0"), from its version resource.
pub fn file_version(path: &Path) -> Option<String> {
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(name.as_ptr(), null_mut());
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut info = null_mut();
        let mut len = 0u32;
        let root = wide("\\");
        if VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) == 0
            || (len as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*(info as *const VS_FIXEDFILEINFO);
        Some(format!(
            "{}.{}.{}",
            info.dwProductVersionMS >> 16,
            info.dwProductVersionMS & 0xffff,
            info.dwProductVersionLS >> 16
        ))
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.as_os_str().eq_ignore_ascii_case(b.as_os_str()),
    }
}

pub fn is_current_exe(path: &Path) -> bool {
    std::env::current_exe().is_ok_and(|current| same_file(&current, path))
}

/// Whether this exe should replace the copy at `other`: that copy is gone or is an older version.
/// The same version elsewhere, or a newer one, is left alone.
pub fn supersedes(other: &Path) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    if same_file(&current, other) {
        return false;
    }
    match file_version(other) {
        Some(version) => crate::update::newer(env!("CARGO_PKG_VERSION"), &version),
        None => !other.exists(),
    }
}

fn process_path(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// Ends an older copy that is already running, so this one becomes the single instance instead
/// of handing over to it. Runs before the single-instance check.
pub fn retire_older_instance() {
    let class = wide(INSTANCE_WINDOW_CLASS);
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), null());
        if hwnd.is_null() {
            return;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == std::process::id() {
            return;
        }
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            pid,
        );
        if process.is_null() {
            return;
        }
        if let Some(path) = process_path(process)
            && supersedes(&path)
        {
            eprintln!("ending older clipkeep at {}", path.display());
            if TerminateProcess(process, 0) != 0 {
                WaitForSingleObject(process, 3000);
            }
        }
        CloseHandle(process);
    }
}
