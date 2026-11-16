use blake3::Hasher;
use image::{
    ExtendedColorType, ImageBuffer, ImageEncoder, ImageFormat, ImageReader, Limits,
    Rgba as RgbaPixel, codecs::png::PngEncoder, imageops,
};
use std::collections::HashSet;
use std::io::Cursor;
use std::io::{Error as IoError, ErrorKind};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

const THUMB_MAX_W: u32 = 640;
const THUMB_MAX_H: u32 = 240;
/// ~40 MP (160 MB as RGBA) covers a triple-4K screenshot; anything bigger isn't worth the RAM spike.
pub const MAX_PIXELS: u64 = 40_000_000;
const MAX_SIDE: u32 = 30_000;
/// Hard ceiling on decoder allocations, so a malformed or hostile PNG fails instead of aborting
/// the process on an out-of-memory allocation.
const MAX_DECODE_ALLOC: u64 = 400 * 1024 * 1024;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct Saved {
    pub hash: String,
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

pub struct ImageStore {
    originals: PathBuf,
    thumbs: PathBuf,
}

/// Hash of the decoded pixels using blake3 (10x faster than sha256), so the same picture matches
/// whether it arrived as raw clipboard pixels or as a PNG from another device.
pub fn hash(img: &Rgba) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"image\0");
    hasher.update(&img.width.to_le_bytes());
    hasher.update(&img.height.to_le_bytes());
    hasher.update(&img.pixels);
    hasher.finalize().to_hex().to_string()
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn file_hash(file: &str) -> Option<&str> {
    file.strip_suffix(".png").filter(|hash| valid_hash(hash))
}

fn temp_image_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".tmp") else {
        return false;
    };
    if let Some(hash) = stem.strip_suffix(".png") {
        return valid_hash(hash);
    }
    let Some((hash, nonce)) = stem.rsplit_once('.') else {
        return false;
    };
    valid_hash(hash)
        && nonce.len() == 32
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

/// A symlink is the Unix equivalent of a reparse point for this guard: never follow one into
/// the image store.
#[cfg(target_os = "macos")]
fn is_reparse_point(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

fn reject_reparse(path: &Path) -> Result<(), String> {
    if is_reparse_point(path) {
        Err("图片存储路径包含重解析点".into())
    } else {
        Ok(())
    }
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(MAX_SIDE);
    l.max_image_height = Some(MAX_SIDE);
    l.max_alloc = Some(MAX_DECODE_ALLOC);
    l
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err(format!("图片尺寸 {width}×{height} 超出限制"));
    }
    Ok(())
}

fn into_rgba(decoded: image::DynamicImage) -> Result<Rgba, String> {
    let (width, height) = (decoded.width(), decoded.height());
    check_size(width, height)?;
    Ok(Rgba {
        width,
        height,
        pixels: decoded.into_rgba8().into_raw(),
    })
}

pub fn decode_png(png: &[u8]) -> Result<Rgba, String> {
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits());
    into_rgba(reader.decode().map_err(|e| e.to_string())?)
}

/// Decode a copied image file with the same pixel and allocation limits used for LAN images.
/// Inspect dimensions before decoding so an oversized file never allocates its full bitmap.
pub fn decode_file(path: &Path) -> Result<Rgba, String> {
    let mut header = ImageReader::open(path).map_err(|e| e.to_string())?;
    header.limits(limits());
    let (width, height) = header.into_dimensions().map_err(|e| e.to_string())?;
    check_size(width, height)?;

    let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    reader.limits(limits());
    into_rgba(reader.decode().map_err(|e| e.to_string())?)
}

impl ImageStore {
    pub fn new(data_dir: &Path) -> std::io::Result<Self> {
        let store = Self {
            originals: data_dir.join("images"),
            thumbs: data_dir.join("thumbs"),
        };
        std::fs::create_dir_all(&store.originals)?;
        std::fs::create_dir_all(&store.thumbs)?;
        if is_reparse_point(&store.originals) || is_reparse_point(&store.thumbs) {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "图片存储目录是重解析点",
            ));
        }
        Ok(store)
    }

    pub fn file_name(hash: &str) -> String {
        format!("{hash}.png")
    }

    /// Returns the shard subdirectory name from hash (first 2 hex chars).
    /// Distributes files across 256 subdirectories to avoid single-directory performance degradation.
    fn shard_dir(hash: &str) -> &str {
        if hash.len() >= 2 { &hash[..2] } else { "00" }
    }

    /// Returns the full path for an original image with sharding.
    fn original_path(&self, hash: &str) -> PathBuf {
        let shard = Self::shard_dir(hash);
        self.originals.join(shard).join(Self::file_name(hash))
    }

    /// Returns the full path for a thumbnail with sharding.
    fn thumb_path_for_hash(&self, hash: &str) -> PathBuf {
        let shard = Self::shard_dir(hash);
        self.thumbs.join(shard).join(Self::file_name(hash))
    }

    /// Older versions kept PNGs directly in the store root. Keep those records readable.
    fn existing_path(&self, base: &Path, hash: &str) -> PathBuf {
        let sharded = base.join(Self::shard_dir(hash)).join(Self::file_name(hash));
        if sharded.exists() {
            sharded
        } else {
            base.join(Self::file_name(hash))
        }
    }

    /// Writes the original and thumbnail if either is missing; cheap when both already exist.
    pub fn write(&self, img: &Rgba, hash: &str) -> Result<Saved, String> {
        check_size(img.width, img.height)?;
        let expected = (img.width as usize)
            .checked_mul(img.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("图片尺寸超出限制")?;
        if img.pixels.len() != expected {
            return Err("图片数据不完整".into());
        }
        if !valid_hash(hash) {
            return Err("图片标识无效".into());
        }
        let file = Self::file_name(hash);
        let original = self.original_path(hash);
        let thumb = self.thumb_path_for_hash(hash);
        for path in [
            self.originals.as_path(),
            self.thumbs.as_path(),
            original.parent().ok_or("图片路径无效")?,
            thumb.parent().ok_or("图片路径无效")?,
            &original,
            &thumb,
        ] {
            reject_reparse(path)?;
        }
        let (w, h) = (img.width, img.height);

        if !original.is_file() {
            // Create shard directory if needed
            if let Some(parent) = original.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                reject_reparse(parent)?;
            }
            reject_reparse(&original)?;
            crate::atomic_file::save_stream(&original, |file| {
                PngEncoder::new(file)
                    .write_image(&img.pixels, w, h, ExtendedColorType::Rgba8)
                    .map_err(IoError::other)
            })
            .map_err(|e| e.to_string())?;
        }

        if !thumb.is_file() {
            // Create shard directory if needed
            if let Some(parent) = thumb.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                reject_reparse(parent)?;
            }
            reject_reparse(&thumb)?;
            let view = ImageBuffer::<RgbaPixel<u8>, &[u8]>::from_raw(w, h, &img.pixels[..])
                .ok_or("图片数据不完整")?;
            let scale = (THUMB_MAX_W as f64 / w as f64)
                .min(THUMB_MAX_H as f64 / h as f64)
                .min(1.0);
            let (tw, th) = (
                ((w as f64 * scale) as u32).max(1),
                ((h as f64 * scale) as u32).max(1),
            );
            let small = imageops::thumbnail(&view, tw, th);
            crate::atomic_file::save_stream(&thumb, |file| {
                PngEncoder::new(file)
                    .write_image(small.as_raw(), tw, th, ExtendedColorType::Rgba8)
                    .map_err(IoError::other)
            })
            .map_err(|e| e.to_string())?;
        }

        let bytes = std::fs::metadata(&original).map(|m| m.len()).unwrap_or(0);
        Ok(Saved {
            hash: hash.to_string(),
            file,
            width: w,
            height: h,
            bytes,
        })
    }

    pub fn load(&self, file: &str) -> Result<Rgba, String> {
        let hash = file_hash(file).ok_or("图片文件名无效")?;
        let path = self.existing_path(&self.originals, hash);
        reject_reparse(&self.originals)?;
        reject_reparse(path.parent().ok_or("图片路径无效")?)?;
        reject_reparse(&path)?;
        let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?;
        reader.set_format(ImageFormat::Png);
        reader.limits(limits());
        into_rgba(reader.decode().map_err(|e| e.to_string())?)
    }

    pub fn png_bytes(&self, file: &str) -> std::io::Result<Vec<u8>> {
        let hash = file_hash(file)
            .ok_or_else(|| IoError::new(ErrorKind::InvalidData, "图片文件名无效"))?;
        let path = self.existing_path(&self.originals, hash);
        for part in [
            &self.originals,
            path.parent().unwrap_or(&self.originals),
            &path,
        ] {
            if is_reparse_point(part) {
                return Err(IoError::new(ErrorKind::InvalidData, "图片路径包含重解析点"));
            }
        }
        std::fs::read(path)
    }

    pub fn thumb_path(&self, file: &str) -> Option<PathBuf> {
        file_hash(file).and_then(|hash| {
            let path = self.existing_path(&self.thumbs, hash);
            (!is_reparse_point(&self.thumbs)
                && !is_reparse_point(path.parent().unwrap_or(&self.thumbs))
                && !is_reparse_point(&path))
            .then_some(path)
        })
    }

    pub fn remove(&self, file: &str) {
        let Some(hash) = file_hash(file) else { return };
        let original = self.original_path(hash);
        let thumb = self.thumb_path_for_hash(hash);
        let legacy_original = self.originals.join(file);
        let legacy_thumb = self.thumbs.join(file);
        if [
            self.originals.as_path(),
            self.thumbs.as_path(),
            original.parent().unwrap_or(&self.originals),
            thumb.parent().unwrap_or(&self.thumbs),
            &original,
            &thumb,
            &legacy_original,
            &legacy_thumb,
        ]
        .into_iter()
        .any(is_reparse_point)
        {
            return;
        }

        let _ = std::fs::remove_file(&original);
        let _ = std::fs::remove_file(&thumb);
        let _ = std::fs::remove_file(&legacy_original);
        let _ = std::fs::remove_file(&legacy_thumb);

        // Try to remove empty shard directories (best effort, ignore errors)
        if let Some(parent) = original.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        if let Some(parent) = thumb.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    /// Deletes files no row points at (left behind by a crash between writing a file and saving
    /// its row) plus stale temp files. Only safe while nothing else is writing images.
    /// Now handles sharded directory structure (recursively scans subdirectories).
    pub fn collect_garbage(&self, referenced: &HashSet<String>) -> usize {
        let mut removed = 0;
        for base_dir in [&self.originals, &self.thumbs] {
            if is_reparse_point(base_dir) {
                continue;
            }
            // Scan both root and shard subdirectories
            if let Ok(entries) = std::fs::read_dir(base_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_reparse_point(&path) {
                        continue;
                    }
                    if path.is_dir() {
                        let shard = entry.file_name().to_string_lossy().into_owned();
                        if shard.len() != 2
                            || !shard
                                .bytes()
                                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                        {
                            continue;
                        }
                        // Scan shard subdirectory
                        if let Ok(shard_entries) = std::fs::read_dir(&path) {
                            for shard_entry in shard_entries.flatten() {
                                if is_reparse_point(&shard_entry.path()) {
                                    continue;
                                }
                                let name = shard_entry.file_name().to_string_lossy().into_owned();
                                let orphan = temp_image_file(&name)
                                    || (file_hash(&name).is_some() && !referenced.contains(&name));
                                if orphan && std::fs::remove_file(shard_entry.path()).is_ok() {
                                    removed += 1;
                                }
                            }
                            // Try to remove empty shard directory
                            let _ = std::fs::remove_dir(&path);
                        }
                    } else {
                        // Handle legacy files in root directory (migration compatibility)
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let orphan = temp_image_file(&name)
                            || (file_hash(&name).is_some() && !referenced.contains(&name));
                        if orphan && std::fs::remove_file(path).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_image_names_cannot_escape_the_image_store() {
        let good = format!("{}.png", "a".repeat(64));
        assert_eq!(file_hash(&good), Some(&good[..64]));
        let bad = [
            "../private.png".to_string(),
            "..\\private.png".to_string(),
            "../../private.png".to_string(),
            "a.png/../private.png".to_string(),
            format!("{}.png", "A".repeat(64)),
            format!("{}.png", "a".repeat(65)),
        ];
        for file in bad {
            assert!(
                file_hash(&file).is_none(),
                "accepted untrusted filename: {file}"
            );
        }
    }

    #[test]
    fn image_store_round_trip_and_temp_names() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "clipkeep-image-test-{}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&dir).unwrap();
        let store = ImageStore::new(&dir).unwrap();
        let image = Rgba {
            width: 1,
            height: 1,
            pixels: vec![9, 17, 25, 255],
        };
        let digest = hash(&image);
        let saved = store.write(&image, &digest).unwrap();
        assert_eq!(store.load(&saved.file).unwrap().pixels, image.pixels);
        assert_eq!(
            decode_png(&std::fs::read(store.thumb_path(&saved.file).unwrap()).unwrap())
                .unwrap()
                .pixels,
            image.pixels
        );
        assert!(temp_image_file(&format!("{digest}.png.tmp")));
        assert!(temp_image_file(&format!("{digest}.{}.tmp", "0".repeat(32))));
        assert!(!temp_image_file("unrelated.tmp"));
        let legacy_original = store.originals.join(&saved.file);
        let legacy_thumb = store.thumbs.join(&saved.file);
        std::fs::rename(store.original_path(&digest), &legacy_original).unwrap();
        std::fs::rename(store.thumb_path_for_hash(&digest), &legacy_thumb).unwrap();
        assert_eq!(store.load(&saved.file).unwrap().pixels, image.pixels);
        assert_eq!(store.thumb_path(&saved.file), Some(legacy_thumb.clone()));
        assert!(!store.png_bytes(&saved.file).unwrap().is_empty());
        store.remove(&saved.file);
        assert!(!store.original_path(&digest).exists());
        assert!(!legacy_original.exists());
        assert!(!legacy_thumb.exists());
        std::fs::remove_dir(store.originals).unwrap();
        std::fs::remove_dir(store.thumbs).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
