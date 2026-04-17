//! Collect a short burst of frames, pick the sharpest by Laplacian variance,
//! and save it to disk. Requires the `analysis` feature.
//!
//! ```bash
//! cargo run --example sharpest --features analysis                  # writes sharpest.png
//! cargo run --example sharpest --features analysis -- photo.png     # writes photo.png
//! ```

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use cameras::analysis::{self, Ring};
use cameras::{PixelFormat, Resolution, StreamConfig, pump};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

const BURST_CAPACITY: usize = 16;
const BURST_DURATION: Duration = Duration::from_millis(800);

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sharpest.png".into());

    let devices = cameras::devices()?;
    let device = devices.first().ok_or("no cameras connected")?;
    println!("opening {}", device.name);

    let config = StreamConfig {
        resolution: Resolution {
            width: 1280,
            height: 720,
        },
        framerate: 30,
        pixel_format: PixelFormat::Bgra8,
    };
    let camera = cameras::open(device, config)?;

    let ring = Arc::new(Mutex::new(analysis::ring_new(BURST_CAPACITY)));
    let sink_ring = Arc::clone(&ring);
    let pump = pump::spawn(camera, move |frame| {
        if let Ok(mut guard) = sink_ring.lock() {
            analysis::ring_push(&mut guard, frame);
        }
    });

    println!("collecting {BURST_CAPACITY}-frame burst over {BURST_DURATION:?}");
    sleep(BURST_DURATION);
    pump::stop_and_join(pump);

    let ring: Ring = Arc::try_unwrap(ring)
        .map_err(|_| "ring still shared after pump join")?
        .into_inner()
        .map_err(|error| format!("ring poisoned: {error}"))?;
    let sharpest = analysis::take_sharpest(&ring).ok_or("no frames captured")?;

    let variance = analysis::blur_variance(&sharpest);
    println!(
        "sharpest frame: variance = {variance:.2} over {} candidates",
        ring.frames.len()
    );

    let rgba = cameras::to_rgba8(&sharpest)?;
    let file = std::fs::File::create(&path)?;
    let encoder = PngEncoder::new(std::io::BufWriter::new(file));
    encoder.write_image(
        &rgba,
        sharpest.width,
        sharpest.height,
        ExtendedColorType::Rgba8,
    )?;

    println!(
        "wrote {}x{} sharpest frame to {}",
        sharpest.width, sharpest.height, path
    );
    Ok(())
}
