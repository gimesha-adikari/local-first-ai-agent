//! EXIF orientation normalization for attached photos (Stage 04-E).
//!
//! Cameras often store pixels sideways or mirrored and record the intended
//! orientation in EXIF metadata. Validation removes that metadata, so an
//! image whose orientation is not "upright" is decoded, transformed and
//! re-encoded first. Decoding happens only after the header dimensions were
//! bounded (at most 24 megapixels), with the decoder's own size limits set
//! to the same bound. Upright images are never decoded here and keep the
//! lossless metadata-stripping path.

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    attachment::MAX_IMAGE_SIDE,
};

/// The EXIF Orientation tag.
const ORIENTATION: u16 = 0x0112;
const SHORT: u16 = 3;
/// More IFD entries than any real camera writes; bounds a malicious count.
const MAX_ENTRIES: usize = 512;
/// Quality of a re-encoded JPEG photo.
const JPEG_QUALITY: u8 = 90;

fn invalid(message: &'static str) -> AgentError {
    AgentError::new(AgentErrorCode::InvalidArguments, message)
}

/// The orientation (2 to 8) recorded in a TIFF-structured EXIF block, or
/// `None` when it is absent, upright (1), malformed or out of range.
pub(crate) fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let big = match tiff.get(0..4)? {
        [b'I', b'I', 42, 0] => false,
        [b'M', b'M', 0, 42] => true,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let ifd = u32_at(4)? as usize;
    let count = usize::from(u16_at(ifd)?);
    if count > MAX_ENTRIES {
        return None;
    }
    (0..count).find_map(|index| {
        let entry = ifd.checked_add(2 + index * 12)?;
        if u16_at(entry)? != ORIENTATION || u16_at(entry + 2)? != SHORT || u32_at(entry + 4)? != 1 {
            return None;
        }
        let value = u16_at(entry + 8)?;
        (2..=8).contains(&value).then_some(value as u8)
    })
}

/// Apply an EXIF orientation to tightly packed pixels of `channels` bytes
/// each. Returns the upright pixels and their width and height.
pub(crate) fn transform(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: usize,
    orientation: u8,
) -> (Vec<u8>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    let swapped = orientation >= 5;
    let (out_w, out_h) = if swapped { (h, w) } else { (w, h) };
    let mut out = vec![0u8; pixels.len()];
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                8 => (w - 1 - y, x),
                _ => (x, y),
            };
            let from = (sy * w + sx) * channels;
            let to = (y * out_w + x) * channels;
            out[to..to + channels].copy_from_slice(&pixels[from..from + channels]);
        }
    }
    (out, out_w as u32, out_h as u32)
}

/// Decode a JPEG whose bounded header was already validated, apply
/// `orientation` and encode the upright pixels as a new baseline JPEG.
pub(crate) fn upright_jpeg(
    bytes: &[u8],
    orientation: u8,
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<Vec<u8>> {
    use zune_core::{colorspace::ColorSpace, options::DecoderOptions};
    let corrupt = || invalid("The JPEG image is damaged or not a JPEG file.");
    let options = DecoderOptions::default()
        .set_max_width(MAX_IMAGE_SIDE as usize)
        .set_max_height(MAX_IMAGE_SIDE as usize)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    decoder.decode_headers().map_err(|_| corrupt())?;
    let (width, height) = decoder.dimensions().ok_or_else(corrupt)?;
    if decoder.get_output_colorspace() != Some(ColorSpace::RGB) {
        return Err(invalid("The JPEG image uses an unsupported colour format."));
    }
    let pixels = decoder.decode().map_err(|_| corrupt())?;
    if pixels.len() != width * height * 3 {
        return Err(corrupt());
    }
    check()?;
    let (upright, out_w, out_h) = transform(&pixels, width as u32, height as u32, 3, orientation);
    drop(pixels);
    check()?;
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
        .encode(
            &upright,
            u16::try_from(out_w).map_err(|_| corrupt())?,
            u16::try_from(out_h).map_err(|_| corrupt())?,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|_| invalid("The photo could not be turned upright."))?;
    Ok(out)
}

/// Decode a PNG, apply `orientation` and encode the upright pixels as a new
/// 8-bit PNG of the same colour type.
pub(crate) fn upright_png(
    bytes: &[u8],
    orientation: u8,
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<Vec<u8>> {
    let corrupt = || invalid("The PNG image is damaged or not a PNG file.");
    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits {
            bytes: 128 * 1024 * 1024,
        },
    );
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_ignore_text_chunk(true);
    let mut reader = decoder.read_info().map_err(|_| corrupt())?;
    let size = reader.output_buffer_size().ok_or_else(corrupt)?;
    let mut pixels = vec![0u8; size];
    let info = reader.next_frame(&mut pixels).map_err(|_| corrupt())?;
    let channels = info.color_type.samples();
    pixels.truncate(info.buffer_size());
    if info.bit_depth != png::BitDepth::Eight
        || pixels.len() != info.width as usize * info.height as usize * channels
    {
        return Err(corrupt());
    }
    check()?;
    let (upright, out_w, out_h) =
        transform(&pixels, info.width, info.height, channels, orientation);
    drop(pixels);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, out_w, out_h);
        encoder.set_color(info.color_type);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| corrupt())?;
        writer.write_image_data(&upright).map_err(|_| corrupt())?;
    }
    check()?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A little-endian TIFF block with one Orientation entry.
    pub(crate) fn tiff(orientation: u16, big: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let u16b = |value: u16| {
            if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        let u32b = |value: u32| {
            if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        out.extend_from_slice(if big { b"MM\0*" } else { b"II*\0" });
        out.extend_from_slice(&u32b(8));
        out.extend_from_slice(&u16b(2));
        // An unrelated entry first (Make, ASCII), then Orientation.
        out.extend_from_slice(&u16b(0x010f));
        out.extend_from_slice(&u16b(2));
        out.extend_from_slice(&u32b(4));
        out.extend_from_slice(b"Cam\0");
        out.extend_from_slice(&u16b(ORIENTATION));
        out.extend_from_slice(&u16b(SHORT));
        out.extend_from_slice(&u32b(1));
        out.extend_from_slice(&u16b(orientation));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&u32b(0));
        out
    }

    #[test]
    fn orientation_is_read_from_either_byte_order_and_bounded() {
        for value in 1..=8u16 {
            let expected = (value != 1).then_some(value as u8);
            assert_eq!(exif_orientation(&tiff(value, false)), expected);
            assert_eq!(exif_orientation(&tiff(value, true)), expected);
        }
        assert_eq!(exif_orientation(&tiff(9, false)), None);
        assert_eq!(exif_orientation(b"II*\0"), None);
        assert_eq!(exif_orientation(b"not tiff"), None);
        let mut huge = tiff(6, false);
        huge[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(exif_orientation(&huge), None);
        let mut truncated = tiff(6, false);
        truncated.truncate(24);
        assert_eq!(exif_orientation(&truncated), None);
    }

    #[test]
    fn every_orientation_maps_pixels_to_the_upright_view() {
        // Stored 3×2 pixels a b c / d e f, one byte each.
        let stored = *b"abcdef";
        let expect = |orientation: u8, w: u32, h: u32, rows: &[u8]| {
            let (out, ow, oh) = transform(&stored, 3, 2, 1, orientation);
            assert_eq!(
                (ow, oh, out.as_slice()),
                (w, h, rows),
                "orientation {orientation}"
            );
        };
        expect(1, 3, 2, b"abcdef");
        expect(2, 3, 2, b"cbafed");
        expect(3, 3, 2, b"fedcba");
        expect(4, 3, 2, b"defabc");
        expect(5, 2, 3, b"adbecf");
        expect(6, 2, 3, b"daebfc");
        expect(7, 2, 3, b"fcebda");
        expect(8, 2, 3, b"cfbead");
        // Multi-byte pixels move as units.
        let (out, _, _) = transform(&[1, 2, 3, 4, 5, 6], 2, 1, 3, 2);
        assert_eq!(out, [4, 5, 6, 1, 2, 3]);
    }
}
