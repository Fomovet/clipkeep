use crate::images::{MAX_PIXELS, Rgba};
use arboard::ImageData;
use clipboard_master::{CallbackResult, ClipboardHandler, Master};
use std::borrow::Cow;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::UI::Shell::DragQueryFileW;

pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;

/// Pairing codes grant send permission and should not become searchable history on either PC.
fn should_record_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains("cklan1://")
}

/// Writeback guard suppression TTL. Writeback events typically arrive within milliseconds,
/// but we allow extra headroom without risking false positives from later real copies.
const SUPPRESS_TTL: Duration = Duration::from_secs(2);

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;
const MAX_IMAGE_FILE_DROP: u32 = 128;

static CAPTURE_IMAGES: AtomicBool = AtomicBool::new(true);
static CAPTURE_PAUSED: AtomicBool = AtomicBool::new(false);

pub fn set_capture_paused(paused: bool) {
    CAPTURE_PAUSED.store(paused, Ordering::SeqCst);
}

pub fn capture_paused() -> bool {
    CAPTURE_PAUSED.load(Ordering::SeqCst)
}

/// Writeback echo suppression guard.
///
/// Our app writing to the clipboard triggers OS change notifications, which would be captured
/// as a new copy, forming a loop. Before writing, we register the `content_hash` of what we're
/// about to write; when the change callback reads content, it checks this guard and skips if matched.
///
/// Uses `content_hash` comparison rather than a simple boolean flag to avoid false positives:
/// if a real user copy arrives before our writeback event, it won't be mistakenly suppressed.
/// Each entry has a TTL to prevent stale hashes from blocking later legitimate copies of the same content.
struct WritebackGuard {
    pending: Mutex<Vec<PendingWriteback>>,
}

struct PendingWriteback {
    content_hash: String,
    at: Instant,
}

impl WritebackGuard {
    fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Register a writeback: before writing to clipboard, call this with the content hash.
    fn suppress(&self, content_hash: String) {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);
        pending.push(PendingWriteback {
            content_hash,
            at: Instant::now(),
        });
    }

    /// Check if this content hash is from our own writeback. If matched (and still valid),
    /// consumes the entry and returns true. Expired entries are cleaned up automatically.
    fn should_skip(&self, content_hash: &str) -> bool {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);

        if let Some(index) = pending.iter().position(|p| p.content_hash == content_hash) {
            pending.remove(index);
            return true;
        }
        false
    }
}

static WRITEBACK_GUARD: std::sync::OnceLock<WritebackGuard> = std::sync::OnceLock::new();

fn writeback_guard() -> &'static WritebackGuard {
    WRITEBACK_GUARD.get_or_init(WritebackGuard::new)
}

pub enum Captured {
    Text(String),
    Image(Rgba),
    ImageFiles(Vec<PathBuf>),
}

impl Captured {
    /// Compute content hash for writeback suppression.
    fn content_hash(&self) -> String {
        match self {
            Captured::Text(text) => crate::db::text_hash(text),
            Captured::Image(img) => crate::images::hash(img),
            Captured::ImageFiles(paths) => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"image-files\0");
                for path in paths {
                    hasher.update(path.to_string_lossy().as_bytes());
                    hasher.update(&[0]);
                }
                hasher.finalize().to_hex().to_string()
            }
        }
    }
}

pub fn set_capture_images(enabled: bool) {
    CAPTURE_IMAGES.store(enabled, Ordering::Relaxed);
}

struct Handler<F: FnMut(Captured)> {
    on_change: F,
}

impl<F: FnMut(Captured)> ClipboardHandler for Handler<F> {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        if capture_paused() || is_excluded() || too_large() {
            return CallbackResult::Next;
        }
        if let Some(captured) = read() {
            // Check if this is our own writeback echo
            let content_hash = captured.content_hash();
            if writeback_guard().should_skip(&content_hash) {
                return CallbackResult::Next;
            }
            if !capture_paused() {
                (self.on_change)(captured);
            }
        }
        CallbackResult::Next
    }

    fn on_clipboard_error(&mut self, _error: std::io::Error) -> CallbackResult {
        CallbackResult::Next
    }
}

/// Runs for the life of the app; if the listener ever fails it is restarted rather than leaving
/// the app silently not recording.
pub fn start_monitor(on_change: impl FnMut(Captured) + Send + 'static) {
    thread::spawn(move || {
        let mut handler = Handler { on_change };
        let mut backoff = Duration::from_millis(500);
        loop {
            match Master::new(&mut handler) {
                Ok(mut master) => {
                    if let Err(e) = master.run() {
                        eprintln!("clipboard monitor stopped: {e}");
                    }
                }
                Err(e) => eprintln!("clipboard monitor failed to start: {e}"),
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}

impl<F: FnMut(Captured)> ClipboardHandler for &mut Handler<F> {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        (**self).on_clipboard_change()
    }

    fn on_clipboard_error(&mut self, error: std::io::Error) -> CallbackResult {
        (**self).on_clipboard_error(error)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Password managers mark secrets with these formats so history tools skip them.
fn is_excluded() -> bool {
    [
        "ExcludeClipboardContentFromMonitorProcessing",
        "Clipboard Viewer Ignore",
    ]
    .iter()
    .any(|name| unsafe {
        let fmt = RegisterClipboardFormatW(wide(name).as_ptr());
        fmt != 0 && IsClipboardFormatAvailable(fmt) != 0
    })
}

struct OpenedClipboard;

impl OpenedClipboard {
    fn open() -> Option<Self> {
        for _ in 0..5 {
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Some(Self);
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// Runs `f` over the first bytes (the header) of one clipboard format without copying the data.
    fn with_header<T>(&self, format: u32, f: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let len = GlobalSize(handle).min(64);
            let ptr = GlobalLock(handle) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let result = f(std::slice::from_raw_parts(ptr, len));
            GlobalUnlock(handle);
            result
        }
    }

    /// Copies one clipboard format's bytes.
    fn read(&self, format: u32) -> Option<Vec<u8>> {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let len = GlobalSize(handle);
            let ptr = GlobalLock(handle) as *const u8;
            if ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            GlobalUnlock(handle);
            Some(bytes)
        }
    }

    fn size_of(&self, format: u32) -> usize {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                0
            } else {
                GlobalSize(handle)
            }
        }
    }
}

impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn png_dims(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w as u64, h as u64))
}

fn dib_dims(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 12 {
        return None;
    }
    let w = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let h = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
    Some((w.unsigned_abs() as u64, h.unsigned_abs() as u64))
}

fn supported_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

enum ImageFileDrop {
    Busy,
    Ignored,
    Paths(Vec<PathBuf>),
}

/// Explorer puts file paths in CF_HDROP instead of publishing the image pixels.
/// Read only the paths while the clipboard is open; file decoding happens later.
fn read_image_file_drop() -> ImageFileDrop {
    let Some(_opened) = OpenedClipboard::open() else {
        return ImageFileDrop::Busy;
    };
    unsafe {
        let handle = GetClipboardData(CF_HDROP);
        if handle.is_null() {
            return ImageFileDrop::Busy;
        }
        let count = DragQueryFileW(handle, u32::MAX, null_mut(), 0);
        if count == 0 || count > MAX_IMAGE_FILE_DROP {
            return ImageFileDrop::Ignored;
        }
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = DragQueryFileW(handle, index, null_mut(), 0);
            if len == 0 || len > 32_767 {
                return ImageFileDrop::Ignored;
            }
            let mut name = vec![0u16; len as usize + 1];
            if DragQueryFileW(handle, index, name.as_mut_ptr(), name.len() as u32) != len {
                return ImageFileDrop::Busy;
            }
            let path = PathBuf::from(OsString::from_wide(&name[..len as usize]));
            // A mixed file selection cannot be represented as a single image copy.
            if !supported_image_file(&path) {
                return ImageFileDrop::Ignored;
            }
            paths.push(path);
        }
        ImageFileDrop::Paths(paths)
    }
}

/// arboard reads the whole payload (and decodes PNGs with no size limit) before we could check it,
/// so a huge copy would exhaust memory and abort the app. Peek at sizes and headers first.
fn too_large() -> bool {
    let Some(cb) = OpenedClipboard::open() else {
        return false;
    };
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
            // UTF-16 is at most twice the UTF-8 byte length of the same text.
            return cb.size_of(CF_UNICODETEXT) > MAX_TEXT_BYTES * 2 + 16;
        }
        if !CAPTURE_IMAGES.load(Ordering::Relaxed) {
            return false;
        }
        let over = |(w, h): (u64, u64)| w == 0 || h == 0 || w * h > MAX_PIXELS;
        let png = RegisterClipboardFormatW(wide("PNG").as_ptr());
        if png != 0
            && IsClipboardFormatAvailable(png) != 0
            && let Some(dims) = cb.with_header(png, png_dims)
        {
            return over(dims);
        }
        for fmt in [CF_DIBV5, CF_DIB] {
            if IsClipboardFormatAvailable(fmt) != 0
                && let Some(dims) = cb.with_header(fmt, dib_dims)
            {
                return over(dims);
            }
        }
    }
    false
}

/// Decodes the clipboard bitmap ourselves when arboard can't (see `dib`). DIBV5 first, since it
/// may carry alpha; CF_DIB is what Windows synthesizes it from, or vice versa.
fn read_dib_fallback() -> Option<Rgba> {
    let cb = OpenedClipboard::open()?;
    [CF_DIBV5, CF_DIB].into_iter().find_map(|format| unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return None;
        }
        cb.read(format).and_then(|bytes| crate::dib::decode(&bytes))
    })
}

/// Retry delays for clipboard read operations (exponential backoff).
/// Other clipboard listeners can briefly hold the clipboard open; we retry with increasing delays.
const CLIPBOARD_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(15),
    Duration::from_millis(35),
    Duration::from_millis(75),
];

/// Text wins when both exist (e.g. spreadsheet cells also carry a bitmap).
fn read() -> Option<Captured> {
    read_with_retry(&CLIPBOARD_RETRY_DELAYS)
}

/// Retry clipboard read with exponential backoff to handle concurrent access.
fn read_with_retry(retry_delays: &[Duration]) -> Option<Captured> {
    // The source app may still hold the clipboard open right after the change notification.
    let max_attempts = retry_delays.len() + 1;

    for attempt in 0..max_attempts {
        if unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 } {
            if !CAPTURE_IMAGES.load(Ordering::Relaxed) {
                return None;
            }
            match read_image_file_drop() {
                ImageFileDrop::Paths(paths) => return Some(Captured::ImageFiles(paths)),
                ImageFileDrop::Ignored => return None,
                ImageFileDrop::Busy if attempt < retry_delays.len() => {
                    thread::sleep(retry_delays[attempt]);
                    continue;
                }
                ImageFileDrop::Busy => return None,
            }
        }
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                if attempt < retry_delays.len() {
                    thread::sleep(retry_delays[attempt]);
                    continue;
                }
                eprintln!(
                    "clipboard read failed after {} attempts: {}",
                    max_attempts, e
                );
                return None;
            }
        };

        match cb.get_text() {
            Ok(text) if should_record_text(&text) => {
                return Some(Captured::Text(text));
            }
            Ok(_) => return None, // Empty or too large
            Err(arboard::Error::ContentNotAvailable) => {
                // No text, try image below
            }
            Err(e) => {
                if attempt < retry_delays.len() {
                    thread::sleep(retry_delays[attempt]);
                    continue;
                }
                eprintln!(
                    "clipboard text read failed after {} attempts: {}",
                    max_attempts, e
                );
                // Fall through to try image
            }
        }

        if !CAPTURE_IMAGES.load(Ordering::Relaxed) {
            return None;
        }

        return match cb.get_image() {
            Ok(img) => Some(Captured::Image(Rgba {
                width: img.width as u32,
                height: img.height as u32,
                pixels: img.bytes.into_owned(),
            })),
            // A decode failure won't go away on retry; try our own DIB decoder instead.
            Err(arboard::Error::ConversionFailure) => {
                let img = read_dib_fallback();
                if img.is_none() {
                    eprintln!("clipboard image could not be decoded");
                }
                img.map(Captured::Image)
            }
            Err(e) => {
                if attempt < retry_delays.len() {
                    thread::sleep(retry_delays[attempt]);
                    continue;
                }
                eprintln!(
                    "clipboard image read failed after {} attempts: {}",
                    max_attempts, e
                );
                None
            }
        };
    }

    None
}

/// Register a writeback suppression before writing to clipboard, to prevent echo loops.
fn suppress_writeback(content_hash: String) {
    writeback_guard().suppress(content_hash);
}

pub fn write_text(text: &str) -> Result<(), arboard::Error> {
    let hash = crate::db::text_hash(text);
    suppress_writeback(hash);
    arboard::Clipboard::new()?.set_text(text.to_owned())
}

pub fn write_image(img: &Rgba) -> Result<(), arboard::Error> {
    let hash = crate::images::hash(img);
    suppress_writeback(hash);
    arboard::Clipboard::new()?.set_image(ImageData {
        width: img.width as usize,
        height: img.height as usize,
        bytes: Cow::Borrowed(&img.pixels),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_credentials_never_enter_clipboard_history() {
        assert!(should_record_text("ordinary text"));
        assert!(!should_record_text("cklan1://192.168.1.2:45788/secret"));
        assert!(!should_record_text(
            "Message: cklan1://192.168.1.2:45788/secret"
        ));
    }

    #[test]
    fn reads_png_and_dib_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&20000u32.to_be_bytes());
        png.extend_from_slice(&30000u32.to_be_bytes());
        assert_eq!(png_dims(&png), Some((20000, 30000)));
        assert_eq!(png_dims(b"not a png at all, definitely not"), None);

        let mut dib = 124u32.to_le_bytes().to_vec();
        dib.extend_from_slice(&1920i32.to_le_bytes());
        dib.extend_from_slice(&(-1080i32).to_le_bytes()); // top-down DIBs use a negative height
        assert_eq!(dib_dims(&dib), Some((1920, 1080)));
    }

    #[test]
    fn writeback_guard_skips_once_then_resets() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());

        assert!(guard.should_skip("hash-a"));
        // Entry consumed, subsequent copy of same content should not be suppressed
        assert!(!guard.should_skip("hash-a"));
    }

    #[test]
    fn writeback_guard_supports_multiple_pending_hashes() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());
        guard.suppress("hash-b".to_owned());

        assert!(guard.should_skip("hash-a"));
        assert!(guard.should_skip("hash-b"));
        assert!(!guard.should_skip("hash-c"));
    }

    #[test]
    fn writeback_guard_does_not_skip_unrelated_content() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());

        // Writeback event hasn't arrived yet, but a different real copy comes in
        assert!(!guard.should_skip("hash-b"));
        // Original writeback event arrives later
        assert!(guard.should_skip("hash-a"));
    }

    #[test]
    fn writeback_guard_expired_suppression_is_ignored() {
        let guard = WritebackGuard::new();
        guard.pending.lock().unwrap().push(PendingWriteback {
            content_hash: "hash-expired".to_owned(),
            at: Instant::now() - SUPPRESS_TTL - Duration::from_millis(100),
        });

        assert!(!guard.should_skip("hash-expired"));
    }
}
