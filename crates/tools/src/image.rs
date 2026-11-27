//! Validation and metadata removal for attached PNG and JPEG images.
//!
//! Dimensions are read from the header and bounded before any pixel data is
//! decoded, which rules out decompression bombs. PNG images are then fully
//! decoded row by row (one row in memory) to prove the compressed stream is
//! intact. JPEG images are checked structurally here and decoded only inside
//! the OCR sandbox or by the WebView preview, both after the bound. Every
//! kept image is rebuilt from an allowlist of chunks or segments, so text,
//! EXIF, XMP, ICC, IPTC and comment metadata never leave this module.

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    attachment::{AttachmentKind, MAX_IMAGE_FILE_BYTES, MAX_IMAGE_PIXELS, MAX_IMAGE_SIDE},
};

/// A validated image with metadata removed.
#[derive(Clone)]
pub struct ValidatedImage {
    pub kind: AttachmentKind,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for ValidatedImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedImage")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

fn invalid(message: &'static str) -> AgentError {
    AgentError::new(AgentErrorCode::InvalidArguments, message)
}

fn too_large() -> AgentError {
    AgentError::new(
        AgentErrorCode::KnowledgeLimit,
        "The image is larger than 8192 pixels on a side or 24 megapixels.",
    )
}

fn check_dimensions(width: u32, height: u32) -> AgentResult<()> {
    if width == 0 || height == 0 {
        return Err(invalid("The image has no pixels."));
    }
    if width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(too_large());
    }
    Ok(())
}

/// Validate an attached image of `kind` and strip its metadata.
pub fn validate(
    kind: AttachmentKind,
    bytes: &[u8],
    check: &dyn Fn() -> AgentResult<()>,
) -> AgentResult<ValidatedImage> {
    if bytes.len() > MAX_IMAGE_FILE_BYTES {
        return Err(AgentError::new(
            AgentErrorCode::KnowledgeLimit,
            "The image file is larger than 10 MB.",
        ));
    }
    match kind {
        AttachmentKind::Png => png(bytes, check),
        AttachmentKind::Jpeg => jpeg(bytes, check),
        _ => Err(invalid("The file is not a PNG or JPEG image.")),
    }
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Chunks kept in a normalized PNG: pixel data and what is needed to show
/// the colours correctly. Text, EXIF, ICC profiles, timestamps and
/// animation chunks are dropped.
const PNG_KEPT: [&[u8; 4]; 8] = [
    b"IHDR", b"PLTE", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"IDAT", b"IEND",
];

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for part in parts {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
            }
        }
    }
    !crc
}

fn png(bytes: &[u8], check: &dyn Fn() -> AgentResult<()>) -> AgentResult<ValidatedImage> {
    let corrupt = || invalid("The PNG image is damaged or not a PNG file.");
    let rest = bytes.strip_prefix(PNG_SIGNATURE).ok_or_else(corrupt)?;
    let mut offset = 0;
    let mut output = PNG_SIGNATURE.to_vec();
    let mut header = None;
    let mut seen_idat = false;
    let mut ended = false;
    let mut chunks = 0u32;
    while offset < rest.len() {
        chunks += 1;
        if chunks > 100_000 || ended {
            return Err(corrupt());
        }
        let length = u32::from_be_bytes(
            rest.get(offset..offset + 4)
                .ok_or_else(corrupt)?
                .try_into()
                .map_err(|_| corrupt())?,
        ) as usize;
        let kind: &[u8; 4] = rest
            .get(offset + 4..offset + 8)
            .ok_or_else(corrupt)?
            .try_into()
            .map_err(|_| corrupt())?;
        let data = rest
            .get(offset + 8..offset + 8 + length)
            .ok_or_else(corrupt)?;
        let stored = rest
            .get(offset + 8 + length..offset + 12 + length)
            .ok_or_else(corrupt)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) || crc32(&[kind, data]).to_be_bytes() != stored
        {
            return Err(corrupt());
        }
        if header.is_none() && kind != b"IHDR" {
            return Err(corrupt());
        }
        match kind {
            b"IHDR" => {
                if header.is_some() || data.len() != 13 {
                    return Err(corrupt());
                }
                let width = u32::from_be_bytes(data[0..4].try_into().map_err(|_| corrupt())?);
                let height = u32::from_be_bytes(data[4..8].try_into().map_err(|_| corrupt())?);
                check_dimensions(width, height)?;
                header = Some((width, height));
            }
            b"IDAT" => seen_idat = true,
            b"IEND" => ended = true,
            _ => {}
        }
        let critical = kind[0].is_ascii_uppercase();
        if PNG_KEPT.contains(&kind) {
            output.extend_from_slice(&rest[offset..offset + 12 + length]);
        } else if critical {
            return Err(invalid("The PNG image uses an unsupported feature."));
        }
        offset += 12 + length;
        if chunks.is_multiple_of(64) {
            check()?;
        }
    }
    let (width, height) = header.ok_or_else(corrupt)?;
    if !seen_idat || !ended {
        return Err(corrupt());
    }
    decode_png(&output, check)?;
    Ok(ValidatedImage {
        kind: AttachmentKind::Png,
        bytes: output,
        width,
        height,
    })
}

/// Decode every row to prove the compressed data is complete and bounded.
fn decode_png(bytes: &[u8], check: &dyn Fn() -> AgentResult<()>) -> AgentResult<()> {
    let corrupt = || invalid("The PNG image is damaged or not a PNG file.");
    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits {
            // Header bounds keep a row far below this; it caps metadata.
            bytes: 64 * 1024 * 1024,
        },
    );
    decoder.set_ignore_text_chunk(true);
    let mut reader = decoder.read_info().map_err(|_| corrupt())?;
    let mut rows = 0u32;
    while reader.next_row().map_err(|_| corrupt())?.is_some() {
        rows += 1;
        if rows.is_multiple_of(256) {
            check()?;
        }
    }
    reader.finish().map_err(|_| corrupt())?;
    Ok(())
}

fn jpeg(bytes: &[u8], check: &dyn Fn() -> AgentResult<()>) -> AgentResult<ValidatedImage> {
    let corrupt = || invalid("The JPEG image is damaged or not a JPEG file.");
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(corrupt());
    }
    let mut output = vec![0xff, 0xd8];
    let mut offset = 2;
    let mut frame = None;
    let mut scans = 0;
    let mut segments = 0u32;
    loop {
        segments += 1;
        if segments > 8192 {
            return Err(corrupt());
        }
        if segments.is_multiple_of(64) {
            check()?;
        }
        // Markers may be preceded by fill bytes.
        if *bytes.get(offset).ok_or_else(corrupt)? != 0xff {
            return Err(corrupt());
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset).ok_or_else(corrupt)?;
        offset += 1;
        match marker {
            0xd9 => {
                // End of image; anything after it is discarded.
                output.extend_from_slice(&[0xff, 0xd9]);
                break;
            }
            0x00 | 0xd8 | 0xd0..=0xd7 | 0x01 => return Err(corrupt()),
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes(
            bytes
                .get(offset..offset + 2)
                .ok_or_else(corrupt)?
                .try_into()
                .map_err(|_| corrupt())?,
        ));
        if length < 2 {
            return Err(corrupt());
        }
        let segment = bytes.get(offset - 2..offset + length).ok_or_else(corrupt)?;
        let payload = &segment[4..];
        offset += length;
        match marker {
            // Baseline, extended and progressive Huffman frames.
            0xc0..=0xc2 => {
                if frame.is_some() || payload.len() < 6 {
                    return Err(corrupt());
                }
                let height = u32::from(u16::from_be_bytes([payload[1], payload[2]]));
                let width = u32::from(u16::from_be_bytes([payload[3], payload[4]]));
                let components = usize::from(payload[5]);
                if height == 0 {
                    // A height defined later by DNL is not supported.
                    return Err(invalid("The JPEG image uses an unsupported encoding."));
                }
                if !matches!(components, 1 | 3 | 4) || payload.len() != 6 + 3 * components {
                    return Err(corrupt());
                }
                check_dimensions(width, height)?;
                frame = Some((width, height));
                output.extend_from_slice(segment);
            }
            // Lossless, hierarchical and arithmetic-coded frames.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xde => {
                return Err(invalid("The JPEG image uses an unsupported encoding."));
            }
            // Start of scan: copy the entropy-coded data up to the next marker.
            0xda => {
                if frame.is_none() {
                    return Err(corrupt());
                }
                scans += 1;
                if scans > 256 {
                    return Err(corrupt());
                }
                output.extend_from_slice(segment);
                let start = offset;
                loop {
                    let byte = *bytes.get(offset).ok_or_else(corrupt)?;
                    if byte == 0xff {
                        match bytes.get(offset + 1) {
                            Some(0x00 | 0xd0..=0xd7) => offset += 2,
                            Some(0xff) => offset += 1,
                            Some(_) => break,
                            None => return Err(corrupt()),
                        }
                    } else {
                        offset += 1;
                    }
                }
                output.extend_from_slice(&bytes[start..offset]);
            }
            // JFIF and Adobe colour information are kept; every other
            // application segment (EXIF, XMP, ICC, IPTC) and comments go.
            0xe0 | 0xee => output.extend_from_slice(segment),
            0xe1..=0xed | 0xef | 0xfe => {}
            // Tables, restart intervals and other structural segments.
            0xc4 | 0xdb | 0xdd | 0xcc => output.extend_from_slice(segment),
            _ => return Err(corrupt()),
        }
    }
    let (width, height) = frame.ok_or_else(corrupt)?;
    if scans == 0 {
        return Err(corrupt());
    }
    Ok(ValidatedImage {
        kind: AttachmentKind::Jpeg,
        bytes: output,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> AgentResult<()> {
        Ok(())
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        out
    }

    pub(crate) fn synthetic_png(width: u32, height: u32, text: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            if let Some(text) = text {
                encoder
                    .add_text_chunk("Comment".into(), text.into())
                    .unwrap();
            }
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![200; (width * height) as usize])
                .unwrap();
        }
        out
    }

    #[test]
    fn png_metadata_is_removed_and_damage_or_bombs_are_rejected() {
        let original = synthetic_png(16, 8, Some("GPS 51.5N private comment"));
        assert!(original.windows(7).any(|w| w == b"Comment"));
        let image = validate(AttachmentKind::Png, &original, &ok).unwrap();
        assert_eq!((image.width, image.height), (16, 8));
        assert!(!image.bytes.windows(7).any(|w| w == b"Comment"));
        assert!(!image.bytes.windows(7).any(|w| w == b"private"));
        // The normalized image is still a decodable PNG.
        decode_png(&image.bytes, &ok).unwrap();

        // A flipped bit in the pixel data fails the chunk checksum.
        let mut damaged = original.clone();
        let last = damaged.len() - 20;
        damaged[last] ^= 0x40;
        assert!(validate(AttachmentKind::Png, &damaged, &ok).is_err());

        // A header claiming enormous dimensions is refused before decoding.
        let mut bomb = PNG_SIGNATURE.to_vec();
        let mut header = 60_000u32.to_be_bytes().to_vec();
        header.extend_from_slice(&60_000u32.to_be_bytes());
        header.extend_from_slice(&[8, 0, 0, 0, 0]);
        bomb.extend(chunk(b"IHDR", &header));
        bomb.extend(chunk(
            b"IDAT",
            &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        ));
        bomb.extend(chunk(b"IEND", &[]));
        let error = validate(AttachmentKind::Png, &bomb, &ok).unwrap_err();
        assert_eq!(error.code, AgentErrorCode::KnowledgeLimit);

        // Truncated compressed data passes the chunk scan but not decoding.
        let mut header = 64u32.to_be_bytes().to_vec();
        header.extend_from_slice(&64u32.to_be_bytes());
        header.extend_from_slice(&[8, 0, 0, 0, 0]);
        let mut short = PNG_SIGNATURE.to_vec();
        short.extend(chunk(b"IHDR", &header));
        short.extend(chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00]));
        short.extend(chunk(b"IEND", &[]));
        assert!(validate(AttachmentKind::Png, &short, &ok).is_err());

        assert!(validate(AttachmentKind::Png, b"not a png", &ok).is_err());
        assert!(validate(AttachmentKind::Jpeg, &original, &ok).is_err());
    }

    pub(crate) fn synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        out.extend_from_slice(&[
            0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0,
        ]);
        // EXIF with a fake location and a comment, both to be removed.
        let exif = b"Exif\0\0GPS-SECRET";
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(exif);
        let comment = b"private comment";
        out.extend_from_slice(&[0xff, 0xfe]);
        out.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(comment);
        out.extend_from_slice(&[0xff, 0xdb, 0x00, 0x43, 0x00]);
        out.extend_from_slice(&[1; 64]);
        out.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 0x08]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        out.extend_from_slice(&[0xff, 0xc4, 0x00, 0x14, 0x00, 1]);
        out.extend_from_slice(&[0; 15]);
        out.extend_from_slice(&[0x00]);
        out.extend_from_slice(&[0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00]);
        out.extend_from_slice(&[0x12, 0x34, 0xff, 0x00, 0x56]);
        out.extend_from_slice(&[0xff, 0xd9]);
        out.extend_from_slice(b"trailing bytes");
        out
    }

    #[test]
    fn jpeg_metadata_is_removed_and_structure_and_size_are_enforced() {
        let original = synthetic_jpeg(40, 30);
        let image = validate(AttachmentKind::Jpeg, &original, &ok).unwrap();
        assert_eq!((image.width, image.height), (40, 30));
        for secret in [&b"GPS-SECRET"[..], b"private comment", b"trailing"] {
            assert!(original.windows(secret.len()).any(|w| w == secret));
            assert!(!image.bytes.windows(secret.len()).any(|w| w == secret));
        }
        assert!(image.bytes.ends_with(&[0xff, 0xd9]));
        assert!(image.bytes.windows(4).any(|w| w == b"JFIF"));

        let huge = synthetic_jpeg(9000, 100);
        assert_eq!(
            validate(AttachmentKind::Jpeg, &huge, &ok).unwrap_err().code,
            AgentErrorCode::KnowledgeLimit
        );
        let mut truncated = original.clone();
        truncated.truncate(original.len() - 20);
        assert!(validate(AttachmentKind::Jpeg, &truncated, &ok).is_err());
        let mut arithmetic = original.clone();
        let sof = arithmetic
            .windows(2)
            .position(|w| w == [0xff, 0xc0])
            .unwrap();
        arithmetic[sof + 1] = 0xc9;
        assert!(validate(AttachmentKind::Jpeg, &arithmetic, &ok).is_err());
        assert!(validate(AttachmentKind::Jpeg, &[0xff, 0xd8, 0xff], &ok).is_err());
    }
}
