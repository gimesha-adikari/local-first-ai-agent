//! Platform-independent handling of one captured screen frame: pixel
//! conversion with stride validation, region mapping, cropping and frame
//! settling. Pixel buffers are wiped when dropped. Nothing here performs I/O.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Largest accepted frame side in pixels.
pub const MAX_DIMENSION: u32 = 16_384;
/// Largest accepted frame, in converted BGRA bytes (8K x 4K is about 128 MiB).
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
/// Smallest selectable region side in frame pixels.
pub const MIN_REGION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Dimensions are zero or above [`MAX_DIMENSION`], or the frame is too large.
    Size,
    /// The pixel format is not one this build can read.
    Format,
    /// The stride, offset or buffer length do not describe a complete frame.
    Layout,
    /// The region is empty, too small or outside the frame.
    Region,
}

/// Uncompressed formats a capture source may deliver. Names follow the byte
/// order in memory (SPA and GStreamer video format naming).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PixelFormat {
    Rgbx,
    Bgrx,
    Xrgb,
    Xbgr,
    Rgba,
    Bgra,
    Argb,
    Abgr,
    Rgb,
    Bgr,
    /// Packed 4:2:2 YUV (Y0 U Y1 V), the common uncompressed camera format.
    /// Converted with BT.601 limited-range coefficients.
    Yuy2,
}

impl PixelFormat {
    /// The SPA `spa_video_format` identifier.
    pub fn from_spa(id: u32) -> Option<Self> {
        Some(match id {
            7 => Self::Rgbx,
            8 => Self::Bgrx,
            9 => Self::Xrgb,
            10 => Self::Xbgr,
            11 => Self::Rgba,
            12 => Self::Bgra,
            13 => Self::Argb,
            14 => Self::Abgr,
            15 => Self::Rgb,
            16 => Self::Bgr,
            4 => Self::Yuy2,
            _ => return None,
        })
    }

    pub fn spa(self) -> u32 {
        match self {
            Self::Rgbx => 7,
            Self::Bgrx => 8,
            Self::Xrgb => 9,
            Self::Xbgr => 10,
            Self::Rgba => 11,
            Self::Bgra => 12,
            Self::Argb => 13,
            Self::Abgr => 14,
            Self::Rgb => 15,
            Self::Bgr => 16,
            Self::Yuy2 => 4,
        }
    }

    /// Bytes per pixel in a row (YUY2 packs two pixels into four bytes).
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Yuy2 => 2,
            Self::Rgb | Self::Bgr => 3,
            _ => 4,
        }
    }

    /// Byte offsets of red, green and blue in one pixel.
    fn channels(self) -> (usize, usize, usize) {
        match self {
            Self::Rgbx | Self::Rgba | Self::Rgb => (0, 1, 2),
            Self::Bgrx | Self::Bgra | Self::Bgr => (2, 1, 0),
            Self::Xrgb | Self::Argb => (1, 2, 3),
            Self::Xbgr | Self::Abgr => (3, 2, 1),
            // Converted separately; never indexed as RGB channels.
            Self::Yuy2 => (0, 0, 0),
        }
    }
}

/// One BT.601 limited-range YUV sample as BGRA.
fn yuv_to_bgra(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let clamp = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(298 * c + 516 * d),
        clamp(298 * c - 100 * d - 208 * e),
        clamp(298 * c + 409 * e),
        255,
    ]
}

/// Owned pixel bytes that are overwritten with zeroes when dropped.
#[derive(Default)]
pub struct Pixels(Vec<u8>);

impl Pixels {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for Pixels {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Pixels({} bytes)", self.0.len())
    }
}

/// An opaque image in BGRA byte order with alpha 255 (Cairo's ARGB32 on
/// little-endian hosts). Rows are tightly packed.
#[derive(Debug)]
pub struct Frame {
    width: u32,
    height: u32,
    bgra: Pixels,
}

fn checked_size(width: u32, height: u32) -> Result<usize, FrameError> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(FrameError::Size);
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(FrameError::Size)?;
    if bytes > MAX_FRAME_BYTES {
        return Err(FrameError::Size);
    }
    Ok(bytes)
}

impl Frame {
    /// Convert one source buffer. `offset` is where the first row starts and
    /// `stride` the distance between rows; both are validated against `raw`.
    pub fn from_raw(
        raw: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        offset: usize,
        format: PixelFormat,
    ) -> Result<Self, FrameError> {
        let bytes = checked_size(width, height)?;
        let bpp = format.bytes_per_pixel();
        let row = (width as usize)
            .checked_mul(bpp)
            .ok_or(FrameError::Layout)?;
        if stride < row {
            return Err(FrameError::Layout);
        }
        let last = stride
            .checked_mul(height as usize - 1)
            .and_then(|start| start.checked_add(offset))
            .and_then(|start| start.checked_add(row))
            .ok_or(FrameError::Layout)?;
        if last > raw.len() {
            return Err(FrameError::Layout);
        }
        let mut bgra = Vec::with_capacity(bytes);
        if format == PixelFormat::Yuy2 {
            // Each four bytes carry two pixels, so the width must be even.
            if !width.is_multiple_of(2) {
                return Err(FrameError::Layout);
            }
            for y in 0..height as usize {
                let start = offset + y * stride;
                for [y0, u, y1, v] in raw[start..start + row].as_chunks::<4>().0.iter().copied() {
                    bgra.extend_from_slice(&yuv_to_bgra(y0, u, v));
                    bgra.extend_from_slice(&yuv_to_bgra(y1, u, v));
                }
            }
            return Ok(Self {
                width,
                height,
                bgra: Pixels(bgra),
            });
        }
        let (r, g, b) = format.channels();
        for y in 0..height as usize {
            let start = offset + y * stride;
            for pixel in raw[start..start + row].chunks_exact(bpp) {
                bgra.extend_from_slice(&[pixel[b], pixel[g], pixel[r], 255]);
            }
        }
        Ok(Self {
            width,
            height,
            bgra: Pixels(bgra),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bgra(&self) -> &[u8] {
        self.bgra.as_slice()
    }

    /// Hand the pixel buffer to a presenter (for example a Cairo surface
    /// that owns and later drops it, which wipes it).
    pub fn into_pixels(self) -> (u32, u32, Pixels) {
        (self.width, self.height, self.bgra)
    }

    /// Copy one region into a new image.
    pub fn crop(&self, region: Region) -> Result<Frame, FrameError> {
        crop_pixels(self.bgra(), self.width, self.height, region)
    }
}

/// Copy `region` out of a tightly packed BGRA buffer.
pub fn crop_pixels(
    bgra: &[u8],
    width: u32,
    height: u32,
    region: Region,
) -> Result<Frame, FrameError> {
    if bgra.len() != checked_size(width, height)? || !region.within(width, height) {
        return Err(FrameError::Region);
    }
    let mut out = Vec::with_capacity(region.width as usize * region.height as usize * 4);
    for y in region.y..region.y + region.height {
        let start = (y as usize * width as usize + region.x as usize) * 4;
        out.extend_from_slice(&bgra[start..start + region.width as usize * 4]);
    }
    Ok(Frame {
        width: region.width,
        height: region.height,
        bgra: Pixels(out),
    })
}

impl Frame {
    /// RGBA bytes for encoders. The copy is wiped when dropped.
    pub fn rgba(&self) -> Pixels {
        let mut out = Vec::with_capacity(self.bgra.len());
        for pixel in self.bgra().as_chunks::<4>().0 {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
        Pixels(out)
    }
}

/// A rectangle in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn within(&self, width: u32, height: u32) -> bool {
        self.width >= MIN_REGION
            && self.height >= MIN_REGION
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|right| right <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| bottom <= height)
    }
}

/// Map a selection drawn on a view of `view` size (any unit, for example
/// logical overlay pixels) onto a frame of `frame` pixels. The two corners
/// may be in any order. Edges are rounded outward so the region covers the
/// whole selection, then clamped to the frame.
pub fn map_selection(
    corners: ((f64, f64), (f64, f64)),
    view: (f64, f64),
    frame: (u32, u32),
) -> Result<Region, FrameError> {
    let ((x0, y0), (x1, y1)) = corners;
    let (view_w, view_h) = view;
    if !(view_w.is_finite() && view_h.is_finite() && view_w > 0.0 && view_h > 0.0)
        || ![x0, y0, x1, y1].iter().all(|value| value.is_finite())
    {
        return Err(FrameError::Region);
    }
    let scale_x = f64::from(frame.0) / view_w;
    let scale_y = f64::from(frame.1) / view_h;
    let clamp = |value: f64, limit: u32| value.max(0.0).min(f64::from(limit));
    let left = clamp(x0.min(x1) * scale_x, frame.0).floor() as u32;
    let top = clamp(y0.min(y1) * scale_y, frame.1).floor() as u32;
    let right = clamp(x0.max(x1) * scale_x, frame.0).ceil() as u32;
    let bottom = clamp(y0.max(y1) * scale_y, frame.1).ceil() as u32;
    let region = Region {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    };
    if region.within(frame.0, frame.1) {
        Ok(region)
    } else {
        Err(FrameError::Region)
    }
}

/// When to stop waiting for frames after consent. Compositors deliver
/// frames on damage, so once the consent dialog has finished closing the
/// stream goes quiet. The first frame may still show the dialog, so a
/// frame is accepted only after `quiet` without a newer one and at least
/// `minimum` after the first frame. If the screen keeps changing, the
/// latest frame is taken at `maximum`. This reduces the chance that the
/// closing consent dialog appears in the frame; it does not guarantee it
/// (a compositor may animate longer, or the screen may keep changing). The
/// user therefore always previews the frozen frame and confirms the region,
/// which is the mandatory check against any remaining transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlePolicy {
    pub quiet: Duration,
    pub minimum: Duration,
    pub maximum: Duration,
}

impl Default for SettlePolicy {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(200),
            minimum: Duration::from_millis(250),
            maximum: Duration::from_millis(1500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    /// Keep waiting for frames.
    Waiting,
    /// No newer frame arrived for the quiet period.
    Quiescent,
    /// The deadline passed while frames were still arriving.
    Deadline,
}

impl SettlePolicy {
    /// `since_first` and `since_last` are measured from the first and the
    /// latest delivered frame.
    pub fn decide(&self, since_first: Duration, since_last: Duration) -> Settled {
        if since_first >= self.maximum {
            Settled::Deadline
        } else if since_first >= self.minimum && since_last >= self.quiet {
            Settled::Quiescent
        } else {
            Settled::Waiting
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_format_converts_with_stride_padding_and_offset() {
        // Two pixels: pure red then pure blue, one padding byte per row.
        let samples: [(PixelFormat, [u8; 4], [u8; 4]); 8] = [
            (PixelFormat::Rgbx, [255, 0, 0, 0], [0, 0, 255, 0]),
            (PixelFormat::Bgrx, [0, 0, 255, 0], [255, 0, 0, 0]),
            (PixelFormat::Xrgb, [0, 255, 0, 0], [0, 0, 0, 255]),
            (PixelFormat::Xbgr, [0, 0, 0, 255], [0, 255, 0, 0]),
            (PixelFormat::Rgba, [255, 0, 0, 9], [0, 0, 255, 9]),
            (PixelFormat::Bgra, [0, 0, 255, 9], [255, 0, 0, 9]),
            (PixelFormat::Argb, [9, 255, 0, 0], [9, 0, 0, 255]),
            (PixelFormat::Abgr, [9, 0, 0, 255], [9, 255, 0, 0]),
        ];
        for (format, red, blue) in samples {
            assert_eq!(PixelFormat::from_spa(format.spa()), Some(format));
            let mut raw = vec![0xEE; 3]; // offset
            for _ in 0..2 {
                raw.extend(red);
                raw.extend(blue);
                raw.push(0xAA); // stride padding
            }
            let frame = Frame::from_raw(&raw, 2, 2, 9, 3, format).unwrap();
            assert_eq!(
                frame.bgra(),
                [
                    0, 0, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255
                ],
                "{format:?}"
            );
        }
        let rgb = Frame::from_raw(&[255, 0, 0, 0, 0, 255], 2, 1, 6, 0, PixelFormat::Rgb).unwrap();
        assert_eq!(rgb.rgba().as_slice(), [255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(PixelFormat::from_spa(2), None); // I420 is not accepted
    }

    #[test]
    fn yuy2_camera_frames_convert_with_bt601_and_reject_odd_widths() {
        // Limited-range white, black, then BT.601 red (Y 81, U 90, V 240),
        // one padding byte per row.
        let raw = [235, 128, 16, 128, 0xAA, 81, 90, 81, 240, 0xAA];
        let frame = Frame::from_raw(&raw, 2, 2, 5, 0, PixelFormat::Yuy2).unwrap();
        let px = |i: usize| &frame.bgra()[i * 4..i * 4 + 4];
        assert_eq!(px(0), [255, 255, 255, 255]);
        assert_eq!(px(1), [0, 0, 0, 255]);
        let red = px(2);
        assert!(red[2] > 250 && red[1] < 5 && red[0] < 5, "{red:?}");
        assert_eq!(PixelFormat::from_spa(4), Some(PixelFormat::Yuy2));
        assert_eq!(PixelFormat::Yuy2.bytes_per_pixel(), 2);
        assert_eq!(
            Frame::from_raw(&[0; 6], 3, 1, 6, 0, PixelFormat::Yuy2).unwrap_err(),
            FrameError::Layout
        );
        assert_eq!(
            Frame::from_raw(&[0; 6], 2, 2, 4, 0, PixelFormat::Yuy2).unwrap_err(),
            FrameError::Layout
        );
    }

    #[test]
    fn malformed_layouts_and_sizes_are_rejected() {
        let raw = vec![0u8; 64];
        let bgrx = PixelFormat::Bgrx;
        assert_eq!(
            Frame::from_raw(&raw, 0, 1, 4, 0, bgrx).unwrap_err(),
            FrameError::Size
        );
        assert_eq!(
            Frame::from_raw(&raw, MAX_DIMENSION + 1, 1, 4, 0, bgrx).unwrap_err(),
            FrameError::Size
        );
        // Too large in total even though each side is allowed.
        assert_eq!(
            Frame::from_raw(&raw, MAX_DIMENSION, MAX_DIMENSION, 4, 0, bgrx).unwrap_err(),
            FrameError::Size
        );
        assert_eq!(
            Frame::from_raw(&raw, 4, 2, 15, 0, bgrx).unwrap_err(),
            FrameError::Layout
        );
        assert_eq!(
            Frame::from_raw(&raw, 4, 4, 16, 1, bgrx).unwrap_err(),
            FrameError::Layout
        );
        assert_eq!(
            Frame::from_raw(&raw, 4, 2, usize::MAX, 0, bgrx).unwrap_err(),
            FrameError::Layout
        );
        assert!(Frame::from_raw(&raw, 4, 4, 16, 0, bgrx).is_ok());
    }

    #[test]
    fn selections_map_to_frame_pixels_across_scales_and_crop_exactly() {
        // A 2x HiDPI frame shown in a 100x50 logical overlay.
        let region =
            map_selection(((60.5, 40.0), (10.2, 10.0)), (100.0, 50.0), (200, 100)).unwrap();
        assert_eq!(
            region,
            Region {
                x: 20,
                y: 20,
                width: 101,
                height: 60
            }
        );
        // Dragging beyond the overlay clamps to the frame.
        let clamped =
            map_selection(((-20.0, -5.0), (130.0, 70.0)), (100.0, 50.0), (200, 100)).unwrap();
        assert_eq!(
            clamped,
            Region {
                x: 0,
                y: 0,
                width: 200,
                height: 100
            }
        );
        // Fractional scaling: 1.25 frame pixels per logical pixel.
        let fractional =
            map_selection(((8.0, 8.0), (40.0, 24.0)), (800.0, 600.0), (1000, 750)).unwrap();
        assert_eq!(
            fractional,
            Region {
                x: 10,
                y: 10,
                width: 40,
                height: 20
            }
        );
        assert_eq!(
            map_selection(((1.0, 1.0), (2.0, 2.0)), (100.0, 50.0), (100, 50)).unwrap_err(),
            FrameError::Region
        );
        assert!(map_selection(((f64::NAN, 0.0), (9.0, 9.0)), (10.0, 10.0), (10, 10)).is_err());

        let mut raw = Vec::new();
        for y in 0..6u8 {
            for x in 0..6u8 {
                raw.extend([x, y, 0, 255]);
            }
        }
        let frame = Frame::from_raw(&raw, 6, 6, 24, 0, PixelFormat::Bgra).unwrap();
        let crop = frame
            .crop(Region {
                x: 1,
                y: 2,
                width: 4,
                height: 4,
            })
            .unwrap();
        assert_eq!((crop.width(), crop.height()), (4, 4));
        assert_eq!(&crop.bgra()[..4], [1, 2, 0, 255]);
        assert_eq!(&crop.bgra()[crop.bgra().len() - 4..], [4, 5, 0, 255]);
        assert_eq!(
            frame
                .crop(Region {
                    x: 3,
                    y: 3,
                    width: 4,
                    height: 4
                })
                .unwrap_err(),
            FrameError::Region
        );
    }

    #[test]
    fn settling_waits_for_quiet_after_a_minimum_and_stops_at_the_deadline() {
        let policy = SettlePolicy::default();
        let ms = Duration::from_millis;
        assert_eq!(policy.decide(ms(0), ms(0)), Settled::Waiting);
        // Quiet but too soon after the first frame (the dialog may be closing).
        assert_eq!(policy.decide(ms(200), ms(200)), Settled::Waiting);
        // Frames still arriving.
        assert_eq!(policy.decide(ms(600), ms(30)), Settled::Waiting);
        assert_eq!(policy.decide(ms(600), ms(250)), Settled::Quiescent);
        assert_eq!(policy.decide(ms(1500), ms(0)), Settled::Deadline);
    }
}
