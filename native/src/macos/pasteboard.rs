//! Small AppKit pasteboard helpers shared by the macOS clipboard implementation.
//!
//! `arboard` already covers text and images on macOS. These helpers fill the two gaps that the
//! Win32 code covers with `CF_HDROP`: reading a file URL that Finder published, and offering a
//! temporary file alongside a pasted picture.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSString};
use std::path::{Path, PathBuf};

pub fn general() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}

fn png_type() -> &'static NSString {
    unsafe { NSPasteboardTypePNG }
}

fn tiff_type() -> &'static NSString {
    unsafe { NSPasteboardTypeTIFF }
}

fn string_type() -> &'static NSString {
    unsafe { NSPasteboardTypeString }
}

fn file_url_type() -> &'static NSString {
    unsafe { NSPasteboardTypeFileURL }
}

/// Names of every type on the general pasteboard, for diagnostics and exclusions.
pub fn type_names() -> Vec<String> {
    let Some(types) = general().types() else {
        return Vec::new();
    };
    (0..types.count())
        .map(|i| types.objectAtIndex(i).to_string())
        .collect()
}

pub fn has_image() -> bool {
    type_names().iter().any(|t| {
        matches!(
            t.as_str(),
            "public.png"
                | "public.tiff"
                | "public.jpeg"
                | "com.compuserve.gif"
                | "public.webp"
                | "com.microsoft.bmp"
        )
    })
}

/// Password managers mark secrets with these types so history tools skip them.
pub fn is_concealed() -> bool {
    type_names().iter().any(|t| {
        t.contains("ConcealedType")
            || t.contains("org.nspasteboard")
            || t.contains("agilebits")
            || t.contains("ExcludeClipboardContentFromMonitorProcessing")
            || t.contains("Clipboard Viewer Ignore")
            || t.contains("1password")
    })
}

/// UTF-8 byte length of the plain-text representation, without decoding it into a Rust String.
pub fn text_byte_len() -> usize {
    general()
        .stringForType(string_type())
        .map_or(0, |s| s.len())
}

/// Raw image representations, used to check dimensions before handing pixels to the decoder.
pub fn image_representations() -> Vec<Vec<u8>> {
    let pb = general();
    let mut out = Vec::new();
    for ty in [png_type(), tiff_type()] {
        if let Some(data) = pb.dataForType(ty) {
            out.push(NSData::to_vec(&data));
        }
    }
    out
}

/// Raw PNG bytes, for the fallback path when `arboard` cannot decode a picture.
pub fn png_bytes() -> Option<Vec<u8>> {
    general()
        .dataForType(png_type())
        .map(|data| NSData::to_vec(&data))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // "file:///Users/..." or "file://localhost/Users/..."; drop an optional host.
    let path_part = &rest[rest.find('/').unwrap_or(0)..];
    let bytes = path_part.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (hi, lo) = (hex(bytes[i + 1])?, hex(bytes[i + 2])?);
            decoded.push(hi * 16 + lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(
        String::from_utf8_lossy(&decoded).into_owned(),
    ))
}

/// File URLs published as `public.file-url`, one per pasteboard item.
pub fn file_url_paths() -> Vec<PathBuf> {
    let pb = general();
    let ty = file_url_type();
    let mut out = Vec::new();
    if let Some(items) = pb.pasteboardItems() {
        for i in 0..items.count() {
            let item = items.objectAtIndex(i);
            if let Some(url) = item.stringForType(ty)
                && let Some(path) = path_from_file_url(&url.to_string())
            {
                out.push(path);
            }
        }
    }
    if out.is_empty()
        && let Some(url) = pb.stringForType(ty)
        && let Some(path) = path_from_file_url(&url.to_string())
    {
        out.push(path);
    }
    out
}

fn file_url_for_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut url = String::with_capacity(raw.len() + 8);
    url.push_str("file://");
    for byte in raw.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(*byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':' | b'+')
        {
            url.push(*byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// Adds a file URL next to the image data `arboard` just wrote, without clearing the pasteboard.
pub fn append_file_url(path: &Path) {
    let url = NSString::from_str(&file_url_for_path(path));
    general().setString_forType(&url, file_url_type());
}
