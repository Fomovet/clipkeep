//! Decoder for the uncompressed clipboard DIBs (`CF_DIBV5` / `CF_DIB`) that arboard rejects.
//!
//! arboard rewrites a 32-bit `BI_RGB` DIBV5 header to `BI_BITFIELDS` and hands it to the `image`
//! BMP decoder, which then expects three extra mask DWORDs after a V4/V5 header and starts the
//! pixels 12 bytes late. Bitmaps without those bytes (PixPin screenshots, for one) run short and
//! fail with `ConversionFailure`. Only the layouts clipboard producers actually use are handled.

use crate::images::{MAX_PIXELS, Rgba};

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const INFO_HEADER: usize = 40;
/// BITMAPV3INFOHEADER: the smallest header that carries an alpha mask.
const V3_HEADER: usize = 56;
const MAX_HEADER: usize = 124;
const MAX_SIDE: u32 = 30_000;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// One colour channel described by a bit mask, scaled to 8 bits on read.
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    max: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        let shift = if mask == 0 { 0 } else { mask.trailing_zeros() };
        Self {
            mask,
            shift,
            max: mask >> shift,
        }
    }

    fn read(self, px: u32) -> u8 {
        if self.max == 0 {
            return 0;
        }
        let value = (px & self.mask) >> self.shift;
        ((u64::from(value) * 255 + u64::from(self.max) / 2) / u64::from(self.max)) as u8
    }
}

/// Decodes a packed DIB (header, optional masks/colour table, pixels) to RGBA.
/// Returns `None` for anything it doesn't understand, so the caller keeps its original error.
pub fn decode(dib: &[u8]) -> Option<Rgba> {
    let header = u32_at(dib, 0)? as usize;
    if !(INFO_HEADER..=MAX_HEADER).contains(&header) || dib.len() < header {
        return None;
    }
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let raw_height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    if width <= 0 || raw_height == 0 || raw_height == i32::MIN || u16_at(dib, 12)? != 1 {
        return None;
    }
    let (width, height) = (width as u32, raw_height.unsigned_abs());
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let bits = u16_at(dib, 14)?;
    let compression = u32_at(dib, 16)?;
    let colors_used = u32_at(dib, 32)? as usize;

    let mut offset = header;
    let (red, green, blue, alpha) = match (bits, compression) {
        (24, BI_RGB) => (0xff_0000, 0xff00, 0xff, 0),
        // The high byte is officially unused, but Chromium, PixPin and others put alpha there.
        (32, BI_RGB) => (0xff_0000, 0xff00, 0xff, 0xff00_0000),
        (32, BI_BITFIELDS) if header == INFO_HEADER => {
            offset += 12;
            (u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?, 0)
        }
        (32, BI_BITFIELDS) => {
            let masks = (u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?);
            let alpha = if header >= V3_HEADER {
                u32_at(dib, 52)?
            } else {
                0
            };
            (masks.0, masks.1, masks.2, alpha)
        }
        _ => return None,
    };
    if red == 0 || green == 0 || blue == 0 {
        return None;
    }
    // Colour tables are optional for true-colour bitmaps and rarely present; skip one if declared.
    offset = offset.checked_add(colors_used.checked_mul(4)?)?;

    let bytes_per_px = usize::from(bits / 8);
    let stride = (width as usize * usize::from(bits)).div_ceil(32) * 4;
    let needed = stride.checked_mul(height as usize)?;
    // Some writers repeat the masks after a V4/V5 header, as a BITMAPINFOHEADER would carry them.
    // Recognise that by the copy itself rather than guessing from the buffer size.
    if compression == BI_BITFIELDS
        && header > INFO_HEADER
        && dib.len() >= offset + 12 + needed
        && (
            u32_at(dib, offset)?,
            u32_at(dib, offset + 4)?,
            u32_at(dib, offset + 8)?,
        ) == (red, green, blue)
    {
        offset += 12;
    }
    let data = dib.get(offset..offset.checked_add(needed)?)?;

    let (r, g, b, a) = (
        Channel::new(red),
        Channel::new(green),
        Channel::new(blue),
        Channel::new(alpha),
    );
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let mut any_alpha = false;
    for y in 0..height as usize {
        // Positive heights are stored bottom-up.
        let src_row = if raw_height > 0 {
            height as usize - 1 - y
        } else {
            y
        };
        let row = &data[src_row * stride..][..width as usize * bytes_per_px];
        let out = &mut pixels[y * width as usize * 4..][..width as usize * 4];
        for (src, dst) in row
            .chunks_exact(bytes_per_px)
            .zip(out.as_chunks_mut::<4>().0)
        {
            let px = match bytes_per_px {
                3 => u32::from(src[0]) | u32::from(src[1]) << 8 | u32::from(src[2]) << 16,
                _ => u32::from_le_bytes([src[0], src[1], src[2], src[3]]),
            };
            dst[0] = r.read(px);
            dst[1] = g.read(px);
            dst[2] = b.read(px);
            dst[3] = a.read(px);
            any_alpha |= dst[3] != 0;
        }
    }
    // No alpha channel, or one left all zero by a writer that doesn't use it: the image is opaque.
    if !any_alpha {
        pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|px| px[3] = 255);
    }
    Some(Rgba {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixels as (r, g, b, a) in top-down order.
    const PIXELS: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 128],
        [10, 20, 30, 255],
        [40, 50, 60, 255],
        [70, 80, 90, 0],
    ];
    const W: i32 = 3;
    const H: i32 = 2;

    fn header(
        size: u32,
        height: i32,
        bits: u16,
        compression: u32,
        masks: Option<[u32; 4]>,
    ) -> Vec<u8> {
        let mut h = vec![0u8; size as usize];
        h[0..4].copy_from_slice(&size.to_le_bytes());
        h[4..8].copy_from_slice(&W.to_le_bytes());
        h[8..12].copy_from_slice(&height.to_le_bytes());
        h[12..14].copy_from_slice(&1u16.to_le_bytes());
        h[14..16].copy_from_slice(&bits.to_le_bytes());
        h[16..20].copy_from_slice(&compression.to_le_bytes());
        if let Some(masks) = masks {
            for (i, mask) in masks.iter().enumerate().take((size as usize - 40) / 4) {
                h[40 + i * 4..44 + i * 4].copy_from_slice(&mask.to_le_bytes());
            }
        }
        h
    }

    /// 32-bit BGRA rows, bottom-up unless `top_down`.
    fn bgra(top_down: bool) -> Vec<u8> {
        let mut rows: Vec<&[[u8; 4]]> = PIXELS.chunks(W as usize).collect();
        if !top_down {
            rows.reverse();
        }
        rows.concat()
            .iter()
            .flat_map(|[r, g, b, a]| [*b, *g, *r, *a])
            .collect()
    }

    fn expected() -> Vec<u8> {
        PIXELS.concat()
    }

    const ARGB: [u32; 4] = [0xff_0000, 0xff00, 0xff, 0xff00_0000];

    /// The header PixPin puts in CF_DIBV5: BI_RGB, an alpha mask, and the pixels right after it.
    fn pixpin_dibv5() -> Vec<u8> {
        let mut dib = header(124, H, 32, BI_RGB, Some([0, 0, 0, 0xff00_0000]));
        dib.extend(bgra(false));
        dib
    }

    #[test]
    fn decodes_pixpin_dibv5() {
        let img = decode(&pixpin_dibv5()).unwrap();
        assert_eq!((img.width, img.height), (W as u32, H as u32));
        assert_eq!(img.pixels, expected());
    }

    /// Pins the root cause: arboard's header rewrite plus `image` starting 12 bytes late. If this
    /// starts passing after a dependency update, the fallback may no longer be needed.
    #[test]
    fn image_crate_rejects_the_rewritten_pixpin_header() {
        let mut dib = pixpin_dibv5();
        dib[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        for (i, mask) in ARGB.iter().enumerate() {
            dib[40 + i * 4..44 + i * 4].copy_from_slice(&mask.to_le_bytes());
        }
        let decoded =
            image::codecs::bmp::BmpDecoder::new_without_file_header(std::io::Cursor::new(&dib))
                .and_then(image::DynamicImage::from_decoder);
        assert!(decoded.is_err());
        assert_eq!(decode(&dib).unwrap().pixels, expected());
    }

    #[test]
    fn decodes_info_header_with_masks_after_it() {
        // PixPin's CF_DIB: 40-byte header, BI_BITFIELDS, three masks, then pixels. No alpha mask.
        let mut dib = header(40, H, 32, BI_BITFIELDS, None);
        for mask in &ARGB[..3] {
            dib.extend(mask.to_le_bytes());
        }
        dib.extend(bgra(false));
        let img = decode(&dib).unwrap();
        let opaque: Vec<u8> = PIXELS
            .iter()
            .flat_map(|[r, g, b, _]| [*r, *g, *b, 255])
            .collect();
        assert_eq!(img.pixels, opaque);
    }

    #[test]
    fn skips_masks_repeated_after_a_v5_header() {
        let mut dib = header(124, H, 32, BI_BITFIELDS, Some(ARGB));
        for mask in &ARGB[..3] {
            dib.extend(mask.to_le_bytes());
        }
        dib.extend(bgra(false));
        assert_eq!(decode(&dib).unwrap().pixels, expected());
    }

    #[test]
    fn v5_bitfields_without_repeated_masks_start_right_after_the_header() {
        let mut dib = header(124, H, 32, BI_BITFIELDS, Some(ARGB));
        dib.extend(bgra(false));
        assert_eq!(decode(&dib).unwrap().pixels, expected());
    }

    #[test]
    fn decodes_top_down_rows() {
        let mut dib = header(124, -H, 32, BI_RGB, Some([0, 0, 0, 0xff00_0000]));
        dib.extend(bgra(true));
        assert_eq!(decode(&dib).unwrap().pixels, expected());
    }

    #[test]
    fn treats_all_zero_alpha_as_opaque() {
        let mut dib = header(40, H, 32, BI_RGB, None);
        dib.extend(
            bgra(false)
                .chunks(4)
                .flat_map(|px| [px[0], px[1], px[2], 0]),
        );
        let img = decode(&dib).unwrap();
        assert!(img.pixels.chunks(4).all(|px| px[3] == 255));
        assert_eq!(&img.pixels[..3], &[255, 0, 0]);
    }

    #[test]
    fn decodes_padded_24_bit_rows() {
        // 3 pixels * 3 bytes = 9, padded to a 12-byte stride.
        let mut dib = header(40, H, 24, BI_RGB, None);
        for row in PIXELS.chunks(W as usize).rev() {
            dib.extend(row.iter().flat_map(|[r, g, b, _]| [*b, *g, *r]));
            dib.extend([0, 0, 0]);
        }
        let img = decode(&dib).unwrap();
        let opaque: Vec<u8> = PIXELS
            .iter()
            .flat_map(|[r, g, b, _]| [*r, *g, *b, 255])
            .collect();
        assert_eq!(img.pixels, opaque);
    }

    #[test]
    fn rejects_truncated_and_unsupported_data() {
        let dib = pixpin_dibv5();
        assert!(decode(&dib[..dib.len() - 1]).is_none());
        assert!(decode(&dib[..30]).is_none());
        let mut paletted = header(40, H, 8, BI_RGB, None);
        paletted.extend(vec![0u8; 1024 + 8]);
        assert!(decode(&paletted).is_none());
        let mut negative_width = pixpin_dibv5();
        negative_width[4..8].copy_from_slice(&(-3i32).to_le_bytes());
        assert!(decode(&negative_width).is_none());
    }
}
