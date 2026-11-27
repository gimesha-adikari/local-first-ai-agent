//! The few SPA POD structures needed to negotiate one raw video format with
//! a PipeWire producer, built and parsed by hand so no PipeWire headers are
//! needed at build time. Layouts follow `spa/pod/pod.h`: every POD is an
//! 8-byte header (`u32` body size, `u32` type) and a body padded to 8 bytes.

use agent_core::screen::PixelFormat;

const ID: u32 = 3;
const INT: u32 = 4;
const LONG: u32 = 5;
const RECTANGLE: u32 = 10;
const FRACTION: u32 = 11;
const OBJECT: u32 = 15;
const CHOICE: u32 = 19;

const CHOICE_RANGE: u32 = 1;
const CHOICE_ENUM: u32 = 3;
const CHOICE_FLAGS: u32 = 4;

const OBJECT_FORMAT: u32 = 0x40003;
const OBJECT_PARAM_BUFFERS: u32 = 0x40004;
pub(super) const PARAM_ENUM_FORMAT: u32 = 3;
pub(super) const PARAM_FORMAT: u32 = 4;
const PARAM_BUFFERS: u32 = 5;

const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const FORMAT_VIDEO_MODIFIER: u32 = 0x20002;
const FORMAT_VIDEO_SIZE: u32 = 0x20003;
const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const MEDIA_TYPE_VIDEO: u32 = 2;
const MEDIA_SUBTYPE_RAW: u32 = 1;
const BUFFERS_DATA_TYPE: u32 = 6;

/// `spa_data_type`: memory the consumer can read directly.
pub(super) const DATA_MEM_PTR: u32 = 1;
pub(super) const DATA_MEM_FD: u32 = 2;
pub(super) const DATA_DMA_BUF: u32 = 3;

/// Screen formats offered, most preferred first (the first is the default).
pub(crate) const SCREEN_FORMATS: [PixelFormat; 10] = [
    PixelFormat::Bgrx,
    PixelFormat::Bgra,
    PixelFormat::Rgbx,
    PixelFormat::Rgba,
    PixelFormat::Xrgb,
    PixelFormat::Xbgr,
    PixelFormat::Argb,
    PixelFormat::Abgr,
    PixelFormat::Rgb,
    PixelFormat::Bgr,
];

/// Camera formats: packed YUV, which most cameras deliver uncompressed, and
/// the packed RGB formats virtual and converted sources may offer. Planar
/// and compressed (MJPEG) formats are not negotiated.
pub(crate) const CAMERA_FORMATS: [PixelFormat; 7] = [
    PixelFormat::Yuy2,
    PixelFormat::Bgrx,
    PixelFormat::Rgbx,
    PixelFormat::Bgra,
    PixelFormat::Rgba,
    PixelFormat::Rgb,
    PixelFormat::Bgr,
];

#[derive(Default)]
struct Builder(Vec<u8>);

impl Builder {
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_ne_bytes());
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }

    /// A POD whose body is written by `body`; the size is patched afterwards.
    fn pod(&mut self, kind: u32, body: impl FnOnce(&mut Self)) {
        let start = self.0.len();
        self.u32(0);
        self.u32(kind);
        body(self);
        let size = (self.0.len() - start - 8) as u32;
        self.0[start..start + 4].copy_from_slice(&size.to_ne_bytes());
        self.pad();
    }

    fn id(&mut self, value: u32) {
        self.pod(ID, |b| b.u32(value));
    }

    /// A choice of `child` values that are each `size` bytes.
    fn choice(&mut self, choice: u32, child: u32, size: u32, values: &[&[u32]]) {
        self.pod(CHOICE, |b| {
            b.u32(choice);
            b.u32(0);
            b.u32(size);
            b.u32(child);
            for value in values {
                for word in *value {
                    b.u32(*word);
                }
            }
        });
    }

    fn property(&mut self, key: u32, value: impl FnOnce(&mut Self)) {
        self.u32(key);
        self.u32(0);
        value(self);
    }
}

/// The EnumFormat parameter: raw video in any of `formats` (the first is
/// the default) and any size, preferring `size`. No DMA-BUF modifier is
/// offered, so producers send memory the CPU can read.
pub(super) fn enum_format(formats: &[PixelFormat], size: (u32, u32)) -> Vec<u8> {
    let mut builder = Builder::default();
    builder.pod(OBJECT, |b| {
        b.u32(OBJECT_FORMAT);
        b.u32(PARAM_ENUM_FORMAT);
        b.property(FORMAT_MEDIA_TYPE, |b| b.id(MEDIA_TYPE_VIDEO));
        b.property(FORMAT_MEDIA_SUBTYPE, |b| b.id(MEDIA_SUBTYPE_RAW));
        b.property(FORMAT_VIDEO_FORMAT, |b| {
            let mut values = vec![[formats[0].spa()]];
            values.extend(formats.iter().map(|format| [format.spa()]));
            let values: Vec<&[u32]> = values.iter().map(|value| value.as_slice()).collect();
            b.choice(CHOICE_ENUM, ID, 4, &values);
        });
        b.property(FORMAT_VIDEO_SIZE, |b| {
            b.choice(
                CHOICE_RANGE,
                RECTANGLE,
                8,
                &[&[size.0, size.1], &[1, 1], &[16_384, 16_384]],
            );
        });
        b.property(FORMAT_VIDEO_FRAMERATE, |b| {
            b.choice(CHOICE_RANGE, FRACTION, 8, &[&[30, 1], &[0, 1], &[1000, 1]]);
        });
    });
    builder.0
}

/// The Buffers parameter: accept only CPU-addressable memory.
pub(super) fn buffers() -> Vec<u8> {
    let mut builder = Builder::default();
    builder.pod(OBJECT, |b| {
        b.u32(OBJECT_PARAM_BUFFERS);
        b.u32(PARAM_BUFFERS);
        b.property(BUFFERS_DATA_TYPE, |b| {
            let mask = (1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD);
            b.choice(CHOICE_FLAGS, INT, 4, &[&[mask]]);
        });
    });
    builder.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoFormat {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// The producer chose a DMA-BUF modifier (GPU memory).
    pub modifier: bool,
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn padded(size: usize) -> usize {
    size.div_ceil(8) * 8
}

/// The value of a property POD, unwrapping a choice to its first value.
fn value(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let size = word(bytes, 0)? as usize;
    let kind = word(bytes, 4)?;
    let body = bytes.get(8..8 + size)?;
    if kind != CHOICE {
        return Some((kind, body));
    }
    let child_size = word(body, 8)? as usize;
    let child_kind = word(body, 12)?;
    Some((child_kind, body.get(16..16 + child_size)?))
}

/// Parse a fixated Format parameter. Returns `None` for anything malformed,
/// non-video, non-raw, or in a format this build cannot read.
pub(super) fn parse_format(pod: &[u8]) -> Option<VideoFormat> {
    let size = word(pod, 0)? as usize;
    if word(pod, 4)? != OBJECT || !(8..=65_536).contains(&size) {
        return None;
    }
    let body = pod.get(8..8 + size)?;
    if word(body, 0)? != OBJECT_FORMAT {
        return None;
    }
    let (mut media, mut subtype, mut format, mut dimensions, mut modifier) =
        (None, None, None, None, false);
    let mut at = 8;
    while at + 16 <= body.len() {
        let key = word(body, at)?;
        let pod_size = word(body, at + 8)? as usize;
        let value_bytes = body.get(at + 8..at + 16 + pod_size)?;
        let (kind, data) = value(value_bytes)?;
        match (key, kind) {
            (FORMAT_MEDIA_TYPE, ID) => media = word(data, 0),
            (FORMAT_MEDIA_SUBTYPE, ID) => subtype = word(data, 0),
            (FORMAT_VIDEO_FORMAT, ID) => format = word(data, 0),
            (FORMAT_VIDEO_SIZE, RECTANGLE) => dimensions = Some((word(data, 0)?, word(data, 4)?)),
            (FORMAT_VIDEO_MODIFIER, LONG) => modifier = true,
            _ => {}
        }
        at += 8 + 8 + padded(pod_size);
    }
    if media? != MEDIA_TYPE_VIDEO || subtype? != MEDIA_SUBTYPE_RAW {
        return None;
    }
    let (width, height) = dimensions?;
    Some(VideoFormat {
        format: PixelFormat::from_spa(format?)?,
        width,
        height,
        modifier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixated Format the way a producer sends it.
    fn fixated(format: u32, width: u32, height: u32, modifier: bool) -> Vec<u8> {
        let mut builder = Builder::default();
        builder.pod(OBJECT, |b| {
            b.u32(OBJECT_FORMAT);
            b.u32(PARAM_FORMAT);
            b.property(FORMAT_MEDIA_TYPE, |b| b.id(MEDIA_TYPE_VIDEO));
            b.property(FORMAT_MEDIA_SUBTYPE, |b| b.id(MEDIA_SUBTYPE_RAW));
            b.property(FORMAT_VIDEO_FORMAT, |b| b.id(format));
            if modifier {
                b.property(FORMAT_VIDEO_MODIFIER, |b| {
                    b.pod(LONG, |b| {
                        b.u32(0);
                        b.u32(0);
                    })
                });
            }
            b.property(FORMAT_VIDEO_SIZE, |b| {
                b.pod(RECTANGLE, |b| {
                    b.u32(width);
                    b.u32(height);
                })
            });
            b.property(FORMAT_VIDEO_FRAMERATE, |b| {
                b.pod(FRACTION, |b| {
                    b.u32(0);
                    b.u32(1);
                })
            });
        });
        builder.0
    }

    #[test]
    fn offered_format_object_has_the_documented_layout() {
        let pod = enum_format(&SCREEN_FORMATS, (1920, 1080));
        assert_eq!(pod.len() % 8, 0);
        assert_eq!(word(&pod, 0).unwrap() as usize, pod.len() - 8);
        assert_eq!(word(&pod, 4), Some(OBJECT));
        assert_eq!(word(&pod, 8), Some(OBJECT_FORMAT));
        assert_eq!(word(&pod, 12), Some(PARAM_ENUM_FORMAT));
        // mediaType: key, flags, then an Id POD of body size 4.
        assert_eq!(word(&pod, 16), Some(FORMAT_MEDIA_TYPE));
        assert_eq!(
            (word(&pod, 24), word(&pod, 28), word(&pod, 32)),
            (Some(4), Some(ID), Some(2))
        );
        let buffers = buffers();
        assert_eq!(word(&buffers, 8), Some(OBJECT_PARAM_BUFFERS));
        assert_eq!(buffers.len() % 8, 0);
    }

    #[test]
    fn producer_formats_parse_and_unsupported_or_malformed_ones_do_not() {
        let parsed = parse_format(&fixated(8, 2560, 1600, false)).unwrap();
        assert_eq!(
            parsed,
            VideoFormat {
                format: PixelFormat::Bgrx,
                width: 2560,
                height: 1600,
                modifier: false
            }
        );
        assert!(parse_format(&fixated(12, 10, 10, true)).unwrap().modifier);
        assert!(parse_format(&fixated(2, 10, 10, false)).is_none()); // I420
        assert_eq!(
            parse_format(&fixated(4, 640, 480, false)).unwrap().format,
            PixelFormat::Yuy2
        );
        let mut truncated = fixated(8, 10, 10, false);
        truncated.truncate(40);
        assert!(parse_format(&truncated).is_none());
        assert!(parse_format(&[0; 4]).is_none());
        // The offered EnumFormat has choices; its first value is the default.
        let offered = parse_format(&enum_format(&SCREEN_FORMATS, (1920, 1080))).unwrap();
        assert_eq!(
            (offered.format, offered.width, offered.height),
            (PixelFormat::Bgrx, 1920, 1080)
        );
        let camera = parse_format(&enum_format(&CAMERA_FORMATS, (1280, 720))).unwrap();
        assert_eq!(
            (camera.format, camera.width, camera.height),
            (PixelFormat::Yuy2, 1280, 720)
        );
    }
}
