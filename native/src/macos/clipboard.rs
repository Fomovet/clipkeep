//! macOS clipboard capture and writeback.
//!
//! Keeps the same public surface and the same recording rules as the Windows module: text wins
//! over a picture, own writebacks are suppressed by content hash, oversized or concealed copies
//! are skipped, and a pasted picture is also offered as a file.

use crate::images::{MAX_PIXELS, Rgba};
use arboard::ImageData;
use clipboard_master::{CallbackResult, ClipboardHandler, Master};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[path = "pasteboard.rs"]
mod pb;

pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Guard against a huge encoded picture even when its header looks harmless.
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

fn should_record_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains("cklan1://")
}

const SUPPRESS_TTL: Duration = Duration::from_secs(2);
const MAX_IMAGE_FILE_DROP: usize = 128;

static CAPTURE_IMAGES: AtomicBool = AtomicBool::new(true);
static CAPTURE_PAUSED: AtomicBool = AtomicBool::new(false);

pub fn set_capture_paused(paused: bool) {
    CAPTURE_PAUSED.store(paused, Ordering::SeqCst);
}

pub fn capture_paused() -> bool {
    CAPTURE_PAUSED.load(Ordering::SeqCst)
}

pub fn set_capture_images(enabled: bool) {
    CAPTURE_IMAGES.store(enabled, Ordering::Relaxed);
}

struct PendingWriteback {
    content_hash: String,
    at: Instant,
}

struct WritebackGuard {
    pending: Mutex<Vec<PendingWriteback>>,
}

impl WritebackGuard {
    fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    fn suppress(&self, content_hash: String) {
        let mut pending = self.pending.lock().expect("writeback guard poisoned");
        pending.retain(|p| p.at.elapsed() <= SUPPRESS_TTL);
        pending.push(PendingWriteback {
            content_hash,
            at: Instant::now(),
        });
    }

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

struct Handler<F: FnMut(Captured)> {
    on_change: F,
}

impl<F: FnMut(Captured)> ClipboardHandler for Handler<F> {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        if capture_paused() || is_excluded() || too_large() {
            return CallbackResult::Next;
        }
        if let Some(captured) = read() {
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

impl<F: FnMut(Captured)> ClipboardHandler for &mut Handler<F> {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        (**self).on_clipboard_change()
    }

    fn on_clipboard_error(&mut self, error: std::io::Error) -> CallbackResult {
        (**self).on_clipboard_error(error)
    }
}

/// `clipboard-master` polls `NSPasteboard.changeCount` on macOS; restart the loop if it ever
/// fails so the app never silently stops recording.
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

fn png_dims(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w as u64, h as u64))
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

/// Password managers and oversized copies are rejected before any pixel is decoded.
fn is_excluded() -> bool {
    pb::is_concealed()
}

fn too_large() -> bool {
    if pb::text_byte_len() > MAX_TEXT_BYTES {
        return true;
    }
    if !CAPTURE_IMAGES.load(Ordering::Relaxed) {
        return false;
    }
    let over = |(w, h): (u64, u64)| w == 0 || h == 0 || w.saturating_mul(h) > MAX_PIXELS;
    pb::image_representations()
        .iter()
        .any(|bytes| bytes.len() > MAX_IMAGE_BYTES || png_dims(bytes).is_some_and(over))
}

fn has_bitmap() -> bool {
    pb::has_image()
}

fn format_names() -> String {
    pb::type_names().join(",")
}

const CLIPBOARD_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(15),
    Duration::from_millis(35),
    Duration::from_millis(75),
];

const IMAGE_RETRY_DELAYS: [Duration; 5] = [
    Duration::from_millis(30),
    Duration::from_millis(60),
    Duration::from_millis(120),
    Duration::from_millis(250),
    Duration::from_millis(500),
];

fn read() -> Option<Captured> {
    read_with_retry(&CLIPBOARD_RETRY_DELAYS)
}

fn read_with_retry(retry_delays: &[Duration]) -> Option<Captured> {
    let max_attempts = retry_delays.len() + 1;

    for attempt in 0..max_attempts {
        let paths = pb::file_url_paths();
        if !paths.is_empty() && CAPTURE_IMAGES.load(Ordering::Relaxed) {
            // Our own paste also offers the picture as a file (see write_image).
            if paths.iter().all(|p| is_paste_file(p)) {
                return None;
            }
            if paths.len() <= MAX_IMAGE_FILE_DROP && paths.iter().all(|p| supported_image_file(p)) {
                return Some(Captured::ImageFiles(paths));
            }
            // A mixed selection may still carry the picture itself; fall through.
            crate::win::log_diagnostic("clipboard-file-drop-skipped reading-bitmap");
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
            Ok(text) if text.trim().is_empty() => {}
            Ok(_) => return None,
            Err(arboard::Error::ContentNotAvailable) => {}
            Err(e) => {
                if attempt < retry_delays.len() {
                    thread::sleep(retry_delays[attempt]);
                    continue;
                }
                eprintln!(
                    "clipboard text read failed after {} attempts: {}",
                    max_attempts, e
                );
            }
        }

        if !CAPTURE_IMAGES.load(Ordering::Relaxed) {
            return None;
        }
        drop(cb);
        return read_image();
    }

    None
}

fn read_image() -> Option<Captured> {
    let mut last_error = String::new();
    for attempt in 0..=IMAGE_RETRY_DELAYS.len() {
        match arboard::Clipboard::new().and_then(|mut cb| cb.get_image()) {
            Ok(img) => {
                return Some(Captured::Image(Rgba {
                    width: img.width as u32,
                    height: img.height as u32,
                    pixels: img.bytes.into_owned(),
                }));
            }
            Err(arboard::Error::ConversionFailure) => {
                last_error = "arboard could not decode the bitmap".into();
                break;
            }
            Err(arboard::Error::ContentNotAvailable) => {
                if !has_bitmap() {
                    return None;
                }
                last_error = "arboard found no image representation".into();
                break;
            }
            Err(e) => {
                last_error = e.to_string();
                if let Some(delay) = IMAGE_RETRY_DELAYS.get(attempt) {
                    thread::sleep(*delay);
                }
            }
        }
    }
    if let Some(png) = pb::png_bytes()
        && let Ok(img) = crate::images::decode_png(&png)
    {
        return Some(Captured::Image(img));
    }
    eprintln!("clipboard image not recorded: {last_error}");
    crate::win::log_diagnostic(&format!(
        "clipboard-image-skipped reason=\"{last_error}\" formats=[{}]",
        format_names()
    ));
    None
}

fn suppress_writeback(content_hash: String) {
    writeback_guard().suppress(content_hash);
}

pub fn write_text(text: &str) -> Result<(), arboard::Error> {
    let hash = crate::db::text_hash(text);
    suppress_writeback(hash);
    arboard::Clipboard::new()?.set_text(text.to_owned())
}

fn paste_dir() -> PathBuf {
    std::env::temp_dir().join("clipkeep-paste")
}

fn is_paste_file(path: &Path) -> bool {
    path.starts_with(paste_dir())
}

pub fn write_image(img: &Rgba, file: Option<(&[u8], &str)>) -> Result<(), arboard::Error> {
    suppress_writeback(crate::images::hash(img));
    let path = file.and_then(|(png, name)| match write_paste_file(png, name) {
        Ok(path) => Some(path),
        Err(e) => {
            eprintln!("could not write the paste file: {e}");
            None
        }
    });
    if let Some(path) = &path {
        suppress_writeback(Captured::ImageFiles(vec![path.clone()]).content_hash());
    }
    arboard::Clipboard::new()?.set_image(ImageData {
        width: img.width as usize,
        height: img.height as usize,
        bytes: Cow::Borrowed(&img.pixels),
    })?;
    if let Some(path) = path {
        pb::append_file_url(&path);
    }
    Ok(())
}

fn write_paste_file(png: &[u8], name: &str) -> std::io::Result<PathBuf> {
    let dir = paste_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    std::fs::write(&path, png)?;
    Ok(path)
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
    fn reads_png_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&20000u32.to_be_bytes());
        png.extend_from_slice(&30000u32.to_be_bytes());
        assert_eq!(png_dims(&png), Some((20000, 30000)));
        assert_eq!(png_dims(b"not a png at all, definitely not"), None);
    }

    #[test]
    fn writeback_guard_skips_once_then_resets() {
        let guard = WritebackGuard::new();
        guard.suppress("hash-a".to_owned());
        assert!(guard.should_skip("hash-a"));
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
        assert!(!guard.should_skip("hash-b"));
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
