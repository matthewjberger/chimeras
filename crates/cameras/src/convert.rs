//! Pixel format conversion helpers.
//!
//! Frames come out of the library in whatever native format the platform delivered.
//! These functions convert them to the two formats most consumers want: packed RGB8
//! (24 bits per pixel) and packed RGBA8 (32 bits per pixel). Stride is honored, so the
//! output is always tightly packed regardless of how padded the source buffer was.

use crate::error::Error;
use crate::types::{Frame, PixelFormat};

/// Convert a frame to packed 24-bit RGB (3 bytes per pixel).
///
/// Supports every [`PixelFormat`]. MJPEG is decoded via `zune-jpeg`.
pub fn to_rgb8(frame: &Frame) -> Result<Vec<u8>, Error> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    match frame.pixel_format {
        PixelFormat::Rgb8 => Ok(frame.plane_primary.to_vec()),
        PixelFormat::Rgba8 => Ok(rgba_to_rgb(&frame.plane_primary)),
        PixelFormat::Bgra8 => Ok(bgra_to_rgb(&frame.plane_primary, width, height, stride)),
        PixelFormat::Yuyv => Ok(yuyv_to_rgb(&frame.plane_primary, width, height, stride)),
        PixelFormat::Nv12 => Ok(nv12_to_rgb(
            &frame.plane_primary,
            &frame.plane_secondary,
            width,
            height,
            stride,
        )),
        PixelFormat::Mjpeg => mjpeg_to_rgb(&frame.plane_primary),
    }
}

/// Convert a frame to packed 32-bit RGBA (4 bytes per pixel).
///
/// For formats without an alpha channel (RGB8, YUYV, NV12, MJPEG), the alpha byte is
/// filled with 0xFF. For BGRA8, the channel order is swapped in place.
pub fn to_rgba8(frame: &Frame) -> Result<Vec<u8>, Error> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    match frame.pixel_format {
        PixelFormat::Rgba8 => Ok(frame.plane_primary.to_vec()),
        PixelFormat::Bgra8 => Ok(bgra_to_rgba(&frame.plane_primary, width, height, stride)),
        _ => {
            let rgb = to_rgb8(frame)?;
            Ok(rgb_to_rgba(&rgb))
        }
    }
}

fn rgba_to_rgb(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity((data.len() / 4) * 3);
    for chunk in data.chunks_exact(4) {
        output.push(chunk[0]);
        output.push(chunk[1]);
        output.push(chunk[2]);
    }
    output
}

fn bgra_to_rgb(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let effective_stride = if stride == 0 { width * 4 } else { stride };
    let rows_available = data.len().checked_div(effective_stride).unwrap_or(0);
    let rows = height.min(rows_available);
    let row_bytes_wanted = width * 4;
    let mut output = Vec::with_capacity(rows * width * 3);
    for row in 0..rows {
        let offset = row * effective_stride;
        let end = offset.saturating_add(row_bytes_wanted).min(data.len());
        let row_bytes = &data[offset..end];
        for pixel in row_bytes.chunks_exact(4) {
            output.push(pixel[2]);
            output.push(pixel[1]);
            output.push(pixel[0]);
        }
    }
    output
}

fn bgra_to_rgba(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let effective_stride = if stride == 0 { width * 4 } else { stride };
    let rows_available = data.len().checked_div(effective_stride).unwrap_or(0);
    let rows = height.min(rows_available);
    let row_bytes_wanted = width * 4;
    let mut output = Vec::with_capacity(width * height * 4);
    for row in 0..rows {
        let offset = row * effective_stride;
        let end = offset.saturating_add(row_bytes_wanted).min(data.len());
        let row_bytes = &data[offset..end];
        for chunk in row_bytes.chunks_exact(4) {
            output.push(chunk[2]);
            output.push(chunk[1]);
            output.push(chunk[0]);
            // Force opaque. Windows Media Foundation delivers BGRA with
            // alpha byte set to 0 (XRGB semantics), which makes the frame
            // render fully transparent in consumers that respect alpha.
            output.push(0xFF);
        }
    }
    output
}

fn rgb_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity((data.len() / 3) * 4);
    for chunk in data.chunks_exact(3) {
        output.push(chunk[0]);
        output.push(chunk[1]);
        output.push(chunk[2]);
        output.push(255);
    }
    output
}

fn yuyv_to_rgb(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let row_bytes = width * 2;
    let effective_stride = if stride == 0 { row_bytes } else { stride };
    let mut output = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let start = row * effective_stride;
        let row_slice = &data[start..start + row_bytes];
        for chunk in row_slice.chunks_exact(4) {
            let y0 = chunk[0] as i32;
            let u = chunk[1] as i32 - 128;
            let y1 = chunk[2] as i32;
            let v = chunk[3] as i32 - 128;
            push_yuv_rgb(&mut output, y0, u, v);
            push_yuv_rgb(&mut output, y1, u, v);
        }
    }
    output
}

fn nv12_to_rgb(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Vec<u8> {
    let y_stride = if stride == 0 { width } else { stride };
    let uv_stride = y_stride;
    let mut output = vec![0u8; width * height * 3];
    for row in 0..height {
        for col in 0..width {
            let y = y_plane[row * y_stride + col] as i32;
            let uv_row = row / 2;
            let uv_col = (col / 2) * 2;
            let uv_index = uv_row * uv_stride + uv_col;
            let u = uv_plane[uv_index] as i32 - 128;
            let v = uv_plane[uv_index + 1] as i32 - 128;
            let base = (row * width + col) * 3;
            let (r, g, b) = yuv_to_rgb(y, u, v);
            output[base] = r;
            output[base + 1] = g;
            output[base + 2] = b;
        }
    }
    output
}

fn push_yuv_rgb(output: &mut Vec<u8>, y: i32, u: i32, v: i32) {
    let (r, g, b) = yuv_to_rgb(y, u, v);
    output.push(r);
    output.push(g);
    output.push(b);
}

fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let c = y - 16;
    let d = u;
    let e = v;
    let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
    let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
    let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
    (r, g, b)
}

fn mjpeg_to_rgb(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoder = zune_jpeg::JpegDecoder::new(data);
    decoder
        .decode()
        .map_err(|error| Error::MjpegDecode(error.to_string()))
}

#[cfg(feature = "analysis")]
pub(crate) fn to_luma8(frame: &Frame) -> Vec<u8> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    match frame.pixel_format {
        PixelFormat::Rgb8 => rgb_to_luma(&frame.plane_primary),
        PixelFormat::Rgba8 => rgba_to_luma(&frame.plane_primary),
        PixelFormat::Bgra8 => bgra_to_luma(&frame.plane_primary, width, height, stride),
        PixelFormat::Yuyv => yuyv_to_luma(&frame.plane_primary, width, height, stride),
        PixelFormat::Nv12 => nv12_y_to_luma(&frame.plane_primary, width, height, stride),
        PixelFormat::Mjpeg => mjpeg_to_rgb(&frame.plane_primary)
            .map(|rgb| rgb_to_luma(&rgb))
            .unwrap_or_default(),
    }
}

#[cfg(feature = "analysis")]
const LUMA_WEIGHT_RED: u32 = 299;
#[cfg(feature = "analysis")]
const LUMA_WEIGHT_GREEN: u32 = 587;
#[cfg(feature = "analysis")]
const LUMA_WEIGHT_BLUE: u32 = 114;

#[cfg(feature = "analysis")]
fn rec601_luma(red: u8, green: u8, blue: u8) -> u8 {
    let weighted = LUMA_WEIGHT_RED * red as u32
        + LUMA_WEIGHT_GREEN * green as u32
        + LUMA_WEIGHT_BLUE * blue as u32;
    (weighted / 1000) as u8
}

#[cfg(feature = "analysis")]
fn rgb_to_luma(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len() / 3);
    for pixel in data.chunks_exact(3) {
        output.push(rec601_luma(pixel[0], pixel[1], pixel[2]));
    }
    output
}

#[cfg(feature = "analysis")]
fn rgba_to_luma(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len() / 4);
    for pixel in data.chunks_exact(4) {
        output.push(rec601_luma(pixel[0], pixel[1], pixel[2]));
    }
    output
}

#[cfg(feature = "analysis")]
fn bgra_to_luma(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let effective_stride = if stride == 0 { width * 4 } else { stride };
    let rows_available = data.len().checked_div(effective_stride).unwrap_or(0);
    let rows = height.min(rows_available);
    let row_bytes_wanted = width * 4;
    let mut output = Vec::with_capacity(rows * width);
    for row in 0..rows {
        let offset = row * effective_stride;
        let end = offset.saturating_add(row_bytes_wanted).min(data.len());
        let row_bytes = &data[offset..end];
        for pixel in row_bytes.chunks_exact(4) {
            output.push(rec601_luma(pixel[2], pixel[1], pixel[0]));
        }
    }
    output
}

#[cfg(feature = "analysis")]
fn yuyv_to_luma(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let row_bytes = width * 2;
    let effective_stride = if stride == 0 { row_bytes } else { stride };
    let rows_available = data.len().checked_div(effective_stride).unwrap_or(0);
    let rows = height.min(rows_available);
    let mut output = Vec::with_capacity(rows * width);
    for row in 0..rows {
        let start = row * effective_stride;
        let end = start.saturating_add(row_bytes).min(data.len());
        let row_slice = &data[start..end];
        for pair in row_slice.chunks_exact(4) {
            output.push(pair[0]);
            output.push(pair[2]);
        }
    }
    output
}

#[cfg(feature = "analysis")]
fn nv12_y_to_luma(y_plane: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let y_stride = if stride == 0 { width } else { stride };
    let rows_available = y_plane.len().checked_div(y_stride).unwrap_or(0);
    let rows = height.min(rows_available);
    let mut output = Vec::with_capacity(rows * width);
    for row in 0..rows {
        let start = row * y_stride;
        let end = start.saturating_add(width).min(y_plane.len());
        output.extend_from_slice(&y_plane[start..end]);
    }
    output
}
