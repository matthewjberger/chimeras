<h1 align="center">chimeras</h1>

<p align="center">
  <a href="https://github.com/matthewjberger/chimeras"><img alt="github" src="https://img.shields.io/badge/github-matthewjberger/chimeras-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20"></a>
  <a href="https://crates.io/crates/chimeras"><img alt="crates.io" src="https://img.shields.io/crates/v/chimeras.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://docs.rs/chimeras"><img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-chimeras-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20"></a>
  <a href="https://github.com/matthewjberger/chimeras/blob/main/LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=for-the-badge&labelColor=555555" height="20"></a>
</p>

<p align="center"><strong>A cross-platform camera library for Rust.</strong></p>

<p align="center">
  <code>cargo add chimeras</code>
</p>

`chimeras` enumerates cameras, probes their supported formats, opens a streaming session, and delivers frames. It runs on macOS (AVFoundation), Windows (Media Foundation), and Linux (V4L2) with the same API on each platform.

The public surface is plain data types (`Device`, `Capabilities`, `FormatDescriptor`, `StreamConfig`, `Frame`) and a handful of free functions. There are no trait objects in the public API, no hidden global state, and no `unsafe` required of consumers.

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
chimeras = "0.1"
```

And in `main.rs`:

```rust
use std::time::Duration;

fn main() -> Result<(), chimeras::Error> {
    let devices = chimeras::devices()?;
    let device = devices.first().expect("no cameras");

    let capabilities = chimeras::probe(device)?;
    let config = chimeras::StreamConfig {
        resolution: chimeras::Resolution { width: 1280, height: 720 },
        framerate: 30,
        pixel_format: chimeras::PixelFormat::Bgra8,
    };

    let camera = chimeras::open(device, config)?;
    let frame = chimeras::next_frame(&camera, Duration::from_secs(2))?;
    let rgb = chimeras::to_rgb8(&frame)?;

    println!("{}x{}, {} bytes rgb", frame.width, frame.height, rgb.len());
    Ok(())
}
```

Dropping the `Camera` stops the stream. Dropping the `DeviceMonitor` joins its worker.

## Platform Support

| Platform | USB / Built-in | RTSP (`rtsp` feature) |
|----------|----------------|------------------------|
| macOS    | AVFoundation (via `objc2`) | retina + VideoToolbox (H.264 / H.265 / MJPEG) |
| Windows  | Media Foundation (via `windows`) | retina + Media Foundation (H.264 / H.265 / MJPEG) |
| Linux    | V4L2 mmap streaming (via `v4l`) | not supported |
| Other    | Returns `Error::BackendNotImplemented` | not supported |

## API Overview

Enumerate and probe:

```rust
let devices = chimeras::devices()?;
let capabilities = chimeras::probe(&devices[0])?;
```

Open a camera and read frames:

```rust
let camera = chimeras::open(&devices[0], config)?;
let frame = chimeras::next_frame(&camera, Duration::from_secs(2))?;
```

Convert pixel formats (BGRA8, RGBA8, YUYV, NV12, MJPEG via `zune-jpeg`):

```rust
let rgb = chimeras::to_rgb8(&frame)?;
let rgba = chimeras::to_rgba8(&frame)?;
```

Watch for camera hotplug:

```rust
let monitor = chimeras::monitor()?;
while let Ok(event) = chimeras::next_event(&monitor, Duration::from_secs(1)) {
    match event {
        chimeras::DeviceEvent::Added(device) => println!("+ {}", device.name),
        chimeras::DeviceEvent::Removed(id) => println!("- {}", id.0),
    }
}
```

Pick a fallback format if the exact request is not supported:

```rust
let picked = chimeras::best_format(&capabilities, &config).expect("no fallback");
```

## Examples

See the [examples](examples/) directory:

- `list.rs`: enumerate every camera and its capabilities
- `capture.rs`: open a camera and pull 30 frames

```bash
cargo run --example list
cargo run --example capture
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
