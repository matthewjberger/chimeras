//! Frame analysis helpers.
//!
//! Pure functions over [`Frame`] for common image-quality measurements, plus a
//! small [`Ring`] for post-hoc "pick the sharpest" selection.
//!
//! Sharpness scores returned by the blur functions are relative scalars, not
//! absolute quality measurements; calibrate thresholds per camera and lighting
//! condition.

use std::collections::VecDeque;

use crate::convert::to_luma8;
use crate::types::{Frame, Rect};

/// Measure frame sharpness as the variance of the 3×3 Laplacian response.
///
/// Returns a relative scalar; higher means sharper. Thresholds must be
/// calibrated per camera and lighting condition.
///
/// ```
/// use cameras::analysis::blur_variance;
/// use cameras::{Frame, FrameQuality, PixelFormat};
/// use bytes::Bytes;
/// use std::time::Duration;
///
/// fn rgb_frame(pixels: Vec<u8>) -> Frame {
///     Frame {
///         width: 8,
///         height: 8,
///         stride: 0,
///         timestamp: Duration::ZERO,
///         pixel_format: PixelFormat::Rgb8,
///         quality: FrameQuality::Intact,
///         plane_primary: Bytes::from(pixels),
///         plane_secondary: Bytes::new(),
///     }
/// }
///
/// let flat = rgb_frame(vec![128u8; 8 * 8 * 3]);
///
/// let mut checkerboard = Vec::with_capacity(8 * 8 * 3);
/// for row in 0..8 {
///     for col in 0..8 {
///         let value = if (row + col) % 2 == 0 { 255 } else { 0 };
///         checkerboard.extend_from_slice(&[value, value, value]);
///     }
/// }
/// let sharp = rgb_frame(checkerboard);
///
/// assert!(blur_variance(&sharp) > blur_variance(&flat));
/// ```
pub fn blur_variance(frame: &Frame) -> f32 {
    let luma = to_luma8(frame);
    let width = frame.width as usize;
    if width == 0 {
        return 0.0;
    }
    let height = luma.len() / width;
    laplacian_variance(&luma, width, height)
}

/// Like [`blur_variance`] but restricted to a region of interest.
///
/// Returns `0.0` for degenerate or out-of-frame regions. Sharpness is relative;
/// calibrate thresholds per camera and lighting condition.
pub fn blur_variance_in(frame: &Frame, region: Rect) -> f32 {
    let luma = to_luma8(frame);
    let width = frame.width as usize;
    if width == 0 {
        return 0.0;
    }
    let height = luma.len() / width;
    let left = (region.x as usize).min(width);
    let top = (region.y as usize).min(height);
    let right = (region.x as usize + region.width as usize).min(width);
    let bottom = (region.y as usize + region.height as usize).min(height);
    if left >= right || top >= bottom {
        return 0.0;
    }
    let cropped_width = right - left;
    let cropped_height = bottom - top;
    let mut cropped = Vec::with_capacity(cropped_width * cropped_height);
    for row in top..bottom {
        let start = row * width + left;
        let end = start + cropped_width;
        cropped.extend_from_slice(&luma[start..end]);
    }
    laplacian_variance(&cropped, cropped_width, cropped_height)
}

/// Faster [`blur_variance`] that samples every `stride`-th pixel in each axis
/// before convolving.
///
/// A `stride` of 0 or 1 behaves exactly like [`blur_variance`]. Larger values
/// trade accuracy for speed and are useful for real-time gating. Sharpness is
/// relative; calibrate thresholds per camera and lighting condition.
pub fn blur_variance_subsampled(frame: &Frame, stride: u32) -> f32 {
    let step = (stride.max(1)) as usize;
    if step == 1 {
        return blur_variance(frame);
    }
    let luma = to_luma8(frame);
    let source_width = frame.width as usize;
    if source_width == 0 {
        return 0.0;
    }
    let source_height = luma.len() / source_width;
    let target_width = source_width.div_ceil(step);
    let target_height = source_height.div_ceil(step);
    if target_width < 3 || target_height < 3 {
        return 0.0;
    }
    let mut downsampled = Vec::with_capacity(target_width * target_height);
    for row in 0..target_height {
        let source_row = row * step;
        if source_row >= source_height {
            break;
        }
        let source_row_start = source_row * source_width;
        for col in 0..target_width {
            let source_col = col * step;
            if source_col >= source_width {
                break;
            }
            downsampled.push(luma[source_row_start + source_col]);
        }
    }
    laplacian_variance(&downsampled, target_width, target_height)
}

/// Fixed-capacity buffer of recent [`Frame`]s.
///
/// Plain data: push with [`ring_push`], scan with [`take_sharpest`], or iterate
/// `frames` directly.
#[derive(Clone, Debug, Default)]
pub struct Ring {
    /// Maximum number of retained frames. `0` disables storage.
    pub capacity: usize,
    /// Retained frames, oldest first.
    pub frames: VecDeque<Frame>,
}

/// Build an empty [`Ring`] with the given capacity.
pub fn ring_new(capacity: usize) -> Ring {
    Ring {
        capacity,
        frames: VecDeque::with_capacity(capacity),
    }
}

/// Append `frame` to `ring`, evicting the oldest frame when at capacity.
pub fn ring_push(ring: &mut Ring, frame: Frame) {
    if ring.capacity == 0 {
        return;
    }
    if ring.frames.len() >= ring.capacity {
        ring.frames.pop_front();
    }
    ring.frames.push_back(frame);
}

/// Return the frame in `ring` with the highest [`blur_variance`] score.
///
/// Scores the full frame for accuracy. Callers who need a faster scan can
/// iterate `ring.frames` and apply [`blur_variance_subsampled`] themselves.
/// Sharpness is relative; this finds the sharpest within the ring, not across
/// time.
pub fn take_sharpest(ring: &Ring) -> Option<Frame> {
    ring.frames
        .iter()
        .map(|frame| (blur_variance(frame), frame))
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, frame)| frame.clone())
}

fn laplacian_variance(luma: &[u8], width: usize, height: usize) -> f32 {
    if width < 3 || height < 3 {
        return 0.0;
    }
    let pixel_count = width * height;
    if luma.len() < pixel_count {
        return 0.0;
    }
    let mut sum: f64 = 0.0;
    let mut sum_squared: f64 = 0.0;
    let mut count: u64 = 0;
    for row in 0..height {
        let row_above = row.saturating_sub(1);
        let row_below = (row + 1).min(height - 1);
        let row_offset = row * width;
        let above_offset = row_above * width;
        let below_offset = row_below * width;
        for col in 0..width {
            let col_left = col.saturating_sub(1);
            let col_right = (col + 1).min(width - 1);
            let center = luma[row_offset + col] as i32;
            let above = luma[above_offset + col] as i32;
            let below = luma[below_offset + col] as i32;
            let neighbor_left = luma[row_offset + col_left] as i32;
            let neighbor_right = luma[row_offset + col_right] as i32;
            let response = above + below + neighbor_left + neighbor_right - 4 * center;
            let response_f = response as f64;
            sum += response_f;
            sum_squared += response_f * response_f;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let inverse_count = 1.0 / count as f64;
    let mean = sum * inverse_count;
    let variance = sum_squared * inverse_count - mean * mean;
    variance.max(0.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Frame, FrameQuality, PixelFormat};
    use bytes::Bytes;
    use std::time::Duration;

    fn rgb_frame(width: u32, height: u32, pixels: Vec<u8>) -> Frame {
        Frame {
            width,
            height,
            stride: 0,
            timestamp: Duration::ZERO,
            pixel_format: PixelFormat::Rgb8,
            quality: FrameQuality::Intact,
            plane_primary: Bytes::from(pixels),
            plane_secondary: Bytes::new(),
        }
    }

    fn checkerboard(width: u32, height: u32) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 3) as usize);
        for row in 0..height {
            for col in 0..width {
                let value = if (row + col) % 2 == 0 { 255 } else { 0 };
                pixels.extend_from_slice(&[value, value, value]);
            }
        }
        rgb_frame(width, height, pixels)
    }

    #[test]
    fn sharp_beats_flat() {
        let flat = rgb_frame(16, 16, vec![128u8; 16 * 16 * 3]);
        let sharp = checkerboard(16, 16);
        assert!(blur_variance(&sharp) > blur_variance(&flat));
    }

    #[test]
    fn roi_within_bounds() {
        let sharp = checkerboard(16, 16);
        let center = Rect {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
        };
        assert!(blur_variance_in(&sharp, center) > 0.0);
    }

    #[test]
    fn roi_out_of_bounds_returns_zero() {
        let sharp = checkerboard(16, 16);
        let offscreen = Rect {
            x: 100,
            y: 100,
            width: 10,
            height: 10,
        };
        assert_eq!(blur_variance_in(&sharp, offscreen), 0.0);
    }

    #[test]
    fn roi_degenerate_returns_zero() {
        let sharp = checkerboard(16, 16);
        let empty = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        assert_eq!(blur_variance_in(&sharp, empty), 0.0);
    }

    #[test]
    fn subsample_stride_one_matches_full() {
        let sharp = checkerboard(16, 16);
        let full = blur_variance(&sharp);
        let strided = blur_variance_subsampled(&sharp, 1);
        assert!((full - strided).abs() < 1e-3);
    }

    #[test]
    fn subsample_stride_zero_matches_full() {
        let sharp = checkerboard(16, 16);
        let full = blur_variance(&sharp);
        let strided = blur_variance_subsampled(&sharp, 0);
        assert!((full - strided).abs() < 1e-3);
    }

    #[test]
    fn ring_push_evicts_oldest() {
        let mut ring = ring_new(2);
        ring_push(&mut ring, rgb_frame(4, 4, vec![0u8; 48]));
        ring_push(&mut ring, rgb_frame(4, 4, vec![64u8; 48]));
        ring_push(&mut ring, rgb_frame(4, 4, vec![128u8; 48]));
        assert_eq!(ring.frames.len(), 2);
        assert_eq!(ring.frames[0].plane_primary[0], 64);
        assert_eq!(ring.frames[1].plane_primary[0], 128);
    }

    #[test]
    fn take_sharpest_picks_highest_variance() {
        let mut ring = ring_new(3);
        ring_push(&mut ring, rgb_frame(16, 16, vec![128u8; 16 * 16 * 3]));
        ring_push(&mut ring, checkerboard(16, 16));
        ring_push(&mut ring, rgb_frame(16, 16, vec![64u8; 16 * 16 * 3]));
        let sharpest = take_sharpest(&ring).expect("ring has frames");
        let sharp_variance = blur_variance(&sharpest);
        let flat_variance = blur_variance(&ring.frames[0]);
        assert!(sharp_variance > flat_variance);
    }

    #[test]
    fn take_sharpest_empty_returns_none() {
        let ring = ring_new(4);
        assert!(take_sharpest(&ring).is_none());
    }

    #[test]
    fn ring_zero_capacity_rejects_pushes() {
        let mut ring = ring_new(0);
        ring_push(&mut ring, rgb_frame(4, 4, vec![0u8; 48]));
        assert!(ring.frames.is_empty());
    }
}
