//! Contrast-detection autofocus composing the `controls` and `analysis` features.
//!
//! Requires `--features "analysis,controls"`. USB-only — RTSP cameras have no
//! remote control surface.
//!
//! ```bash
//! just run-autofocus
//! ```
//!
//! Algorithm: brute-force focus sweep, measuring Laplacian variance over a center
//! region-of-interest at each step. `read_controls` can replace the fixed 150ms
//! sleep with "poll until reported focus matches requested" on platforms where
//! the read reflects actuator position (V4L2 and Media Foundation usually do;
//! macOS returns the target, not the current).
//!
//! Sharpness scores are relative; threshold values do not transfer across cameras
//! or lighting conditions. Golden-section search converges in ~6-8 samples for
//! unimodal scenes; brute-force handles repetitive textures and ringing better.

use std::error::Error;
use std::thread::sleep;
use std::time::Duration;

use cameras::analysis;
use cameras::{Controls, Frame, PixelFormat, Rect, Resolution, StreamConfig};

const SETTLE_DELAY: Duration = Duration::from_millis(150);
const SWEEP_STRIDE_MULTIPLIER: f32 = 4.0;
const CONTINUOUS_RANGE_SAMPLES: f32 = 20.0;
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(1);

fn main() -> Result<(), Box<dyn Error>> {
    let devices = cameras::devices()?;
    let device = devices.first().ok_or("no cameras connected")?;
    println!("opening {}", device.name);

    let capabilities = cameras::control_capabilities(device)?;
    let Some(focus_range) = capabilities.focus else {
        println!(
            "{} has no controllable focus (e.g. a fixed-focus camera)",
            device.name
        );
        return Ok(());
    };
    println!(
        "focus range: {:.2}..{:.2} (step {:.2}, default {:.2})",
        focus_range.min, focus_range.max, focus_range.step, focus_range.default
    );

    let config = StreamConfig {
        resolution: Resolution {
            width: 1280,
            height: 720,
        },
        framerate: 30,
        pixel_format: PixelFormat::Bgra8,
    };
    let camera = cameras::open(device, config)?;

    cameras::apply_controls(
        device,
        &Controls {
            auto_focus: Some(false),
            auto_exposure: Some(false),
            auto_white_balance: Some(false),
            ..Default::default()
        },
    )?;

    let step = if focus_range.step > 0.0 {
        focus_range.step * SWEEP_STRIDE_MULTIPLIER
    } else {
        ((focus_range.max - focus_range.min) / CONTINUOUS_RANGE_SAMPLES).max(f32::EPSILON)
    };
    let span = (focus_range.max - focus_range.min).max(0.0);
    let sample_count = (span / step).floor() as u32 + 1;
    let mut best: Option<(f32, f32)> = None;

    for index in 0..sample_count {
        let focus_value = (focus_range.min + index as f32 * step).min(focus_range.max);
        cameras::apply_controls(
            device,
            &Controls {
                focus: Some(focus_value),
                ..Default::default()
            },
        )?;
        sleep(SETTLE_DELAY);

        let frame = cameras::next_frame(&camera, FRAME_READ_TIMEOUT)?;
        let roi = center_quarter_roi(&frame);
        let variance = analysis::blur_variance_in(&frame, roi);
        println!("  focus={focus_value:.2}  variance={variance:.1}");

        match best {
            None => best = Some((focus_value, variance)),
            Some((_, best_variance)) if variance > best_variance => {
                best = Some((focus_value, variance));
            }
            _ => {}
        }
    }
    let samples = sample_count as usize;

    let (winner, score) = best.ok_or("sweep captured no samples")?;
    println!("swept {samples} samples; best focus={winner:.2} (variance={score:.1})");

    cameras::apply_controls(
        device,
        &Controls {
            focus: Some(winner),
            ..Default::default()
        },
    )?;
    sleep(SETTLE_DELAY);

    println!("focus locked at {winner:.2}");
    Ok(())
}

fn center_quarter_roi(frame: &Frame) -> Rect {
    let width = frame.width;
    let height = frame.height;
    let roi_width = width / 2;
    let roi_height = height / 2;
    Rect {
        x: width / 4,
        y: height / 4,
        width: roi_width,
        height: roi_height,
    }
}
