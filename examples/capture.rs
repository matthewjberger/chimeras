use std::time::Duration;

fn main() -> Result<(), chimeras::Error> {
    let devices = chimeras::devices()?;
    let Some(device) = devices.first() else {
        println!("no cameras found");
        return Ok(());
    };

    println!("opening {}", device.name);

    let config = chimeras::StreamConfig {
        resolution: chimeras::Resolution {
            width: 1280,
            height: 720,
        },
        framerate: 30,
        pixel_format: chimeras::PixelFormat::Bgra8,
    };

    let camera = chimeras::open(device, config)?;

    for index in 0..30 {
        let frame = chimeras::next_frame(&camera, Duration::from_secs(2))?;
        println!(
            "frame {}: {}x{} stride={} format={:?}",
            index, frame.width, frame.height, frame.stride, frame.pixel_format,
        );
    }

    Ok(())
}
