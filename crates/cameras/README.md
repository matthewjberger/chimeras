<h1 align="center">cameras</h1>

<p align="center">
  <a href="https://github.com/matthewjberger/cameras"><img alt="github" src="https://img.shields.io/badge/github-matthewjberger/cameras-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20"></a>
  <a href="https://crates.io/crates/cameras"><img alt="crates.io" src="https://img.shields.io/crates/v/cameras.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://docs.rs/cameras"><img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-cameras-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20"></a>
  <a href="https://github.com/matthewjberger/cameras/blob/main/LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=for-the-badge&labelColor=555555" height="20"></a>
</p>

<p align="center"><strong>A cross-platform camera library for Rust.</strong></p>

<p align="center">
  <code>cargo add cameras</code>
</p>

`cameras` enumerates cameras, probes their supported formats, opens a streaming session, and delivers frames. It runs on macOS (AVFoundation), Windows (Media Foundation), and Linux (V4L2) with the same API on each platform.

The public surface is plain data types (`Device`, `Capabilities`, `FormatDescriptor`, `StreamConfig`, `Frame`) and a handful of free functions. There are no trait objects in the public API, no hidden global state, and no `unsafe` required of consumers.

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
cameras = "0.1"
```

And in `main.rs`:

```rust
use std::time::Duration;

fn main() -> Result<(), cameras::Error> {
    let devices = cameras::devices()?;
    let device = devices.first().expect("no cameras");

    let capabilities = cameras::probe(device)?;
    let config = cameras::StreamConfig {
        resolution: cameras::Resolution { width: 1280, height: 720 },
        framerate: 30,
        pixel_format: cameras::PixelFormat::Bgra8,
    };

    let camera = cameras::open(device, config)?;
    let frame = cameras::next_frame(&camera, Duration::from_secs(2))?;
    let rgb = cameras::to_rgb8(&frame)?;

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

## API Overview

Enumerate and probe:

```rust
let devices = cameras::devices()?;
let capabilities = cameras::probe(&devices[0])?;
```

Open a camera and read frames:

```rust
use std::time::Duration;

let camera = cameras::open(&devices[0], config)?;
let frame = cameras::next_frame(&camera, Duration::from_secs(2))?;
```

Convert pixel formats (BGRA8, RGBA8, YUYV, NV12, MJPEG via `zune-jpeg`):

```rust
let rgb = cameras::to_rgb8(&frame)?;
let rgba = cameras::to_rgba8(&frame)?;
```

Watch for camera hotplug:

```rust
use std::time::Duration;

let monitor = cameras::monitor()?;
while let Ok(event) = cameras::next_event(&monitor, Duration::from_secs(1)) {
    match event {
        cameras::DeviceEvent::Added(device) => println!("+ {}", device.name),
        cameras::DeviceEvent::Removed(id) => println!("- {}", id.0),
    }
}
```

Pick a fallback format if the exact request is not supported:

```rust
let picked = cameras::best_format(&capabilities, &config).expect("no fallback");
```

## Higher-level primitives

Two optional modules layer on top of the core. They are pure conveniences; callers who want full control can stay on `open` and `next_frame`.

### `cameras::source`: one enum for USB and RTSP

`CameraSource` lets UIs and configs carry a single "where do frames come from" value instead of branching between `open` and `open_rtsp` at every call site.

```rust
use cameras::{CameraSource, Device, StreamConfig};

fn open_any(device: Device, config: StreamConfig) -> Result<cameras::Camera, cameras::Error> {
    let source = CameraSource::Usb(device);
    cameras::open_source(source, config)
}
```

`CameraSource` implements `PartialEq`, `Eq`, and `Hash` (USB compared by device id, RTSP by URL plus credentials) so it works as a map key or a `Signal` value.

### `cameras::pump`: background frame pump with pause and snapshot

`pump::spawn` takes a `Camera` and a sink closure, runs the frame loop on its own thread, and returns a `Pump` handle with three operations:

- `pump::set_active(&pump, bool)`: pause or resume streaming without closing the camera (no per-frame work while paused).
- `pump::capture_frame(&pump) -> Option<Frame>`: fetch a single fresh frame on demand, works whether the pump is active or paused.
- `pump::stop_and_join(pump)`: deterministic teardown.

```rust
use cameras::pump;

fn drive(camera: cameras::Camera) {
    let p = pump::spawn(camera, |frame| {
        // publish the frame wherever: a channel, a Mutex, your own UI state.
        let _ = frame;
    });

    // Take a snapshot regardless of whether the pump is active:
    if let Some(_snapshot) = pump::capture_frame(&p) { /* use the frame */ }

    // Preview hidden? Park the pump.
    pump::set_active(&p, false);

    // Bring it back later.
    pump::set_active(&p, true);

    pump::stop_and_join(p);
}
```

Pause eliminates Rust-side per-frame work. The OS camera pipeline keeps running. See the `pump` module docs for the full trade-off.

### `cameras::analysis`: blur variance and sharpest-of-burst (feature: `analysis`)

Enable the `analysis` feature for pure-function helpers over `Frame`:

- `blur_variance(&Frame) -> f32`: 3×3 Laplacian variance as a relative sharpness score.
- `blur_variance_in(&Frame, Rect) -> f32`: same, restricted to a region of interest.
- `blur_variance_subsampled(&Frame, stride) -> f32`: fast subsampled variant for real-time gating.
- `Ring` + `ring_push` + `take_sharpest`: collect a burst of recent frames and pick the sharpest.

```toml
[dependencies]
cameras = { version = "0.1", features = ["analysis"] }
```

```rust
use cameras::analysis;

let variance = analysis::blur_variance(&frame);
```

Scores are relative; calibrate thresholds per camera and lighting condition. The `examples/sharpest.rs` example (runnable via `just run-sharpest`) collects a short burst and saves the sharpest frame to a PNG.

## Dioxus integration

If you are building a Dioxus app, see the companion crate [`dioxus-cameras`](dioxus-cameras/), which provides:

- `use_camera_stream` hook with `active` + `capture_frame` on the returned handle.
- `use_devices` and `use_streams` hooks for camera enumeration and multi-stream id management.
- A loopback HTTP preview server plus a WebGL2 canvas renderer (`StreamPreview` + `PreviewScript`).

## Testing RTSP Locally

The `demo/` app can view RTSP streams on macOS and Windows. To exercise the full path without a real IP camera, serve a local MP4 as an RTSP stream using [`mediamtx`](https://github.com/bluenviron/mediamtx) and `ffmpeg` (both on `PATH`):

```bash
# terminal 1: start mediamtx with the repo's mediamtx.yml
just rtsp-host

# terminal 2: publish an MP4 file as an RTSP stream on rtsp://127.0.0.1:8554/live
just rtsp-publish path/to/some.mp4

# terminal 3: launch the demo app
just run
```

In the demo window, switch the source toggle to **RTSP**, paste `rtsp://127.0.0.1:8554/live` into the URL field, and press **Connect**. On macOS and Windows, H.264/H.265 streams are hardware-decoded (VideoToolbox / Media Foundation); MJPEG streams are delivered verbatim and decoded via `zune-jpeg` on demand.

## Discovery

Enable the `discover` feature to scan for Axis RTSP cameras. Discovery is RTSP-only and currently recognizes a single vendor (Axis). Scans mix expanded CIDR subnets with explicit `host:port` endpoints so you can probe both a local subnet and a set of port-forwarded tunnels in one run.

```toml
[dependencies]
cameras = { version = "0.1", features = ["discover"] }
ipnet = "2"
```

```rust
use std::time::Duration;
use cameras::discover::{self, DiscoverConfig, DiscoverEvent};

let net: ipnet::IpNet = "192.168.1.0/24".parse().unwrap();
let discovery = discover::discover(DiscoverConfig {
    subnets: vec![net],
    ..Default::default()
})?;

loop {
    match discover::next_event(&discovery, Duration::from_millis(500)) {
        Ok(DiscoverEvent::CameraFound(camera)) => println!("{:?}", camera),
        Ok(DiscoverEvent::Progress { scanned, total }) => eprintln!("{scanned}/{total}"),
        Ok(DiscoverEvent::Done) => break,
        _ => continue,
    }
}
```

Port-forwarded tunnel scenario (e.g. Teleport forwarding several remote DVRs to distinct local ports on `127.0.0.1`):

```rust
use cameras::discover::{self, DiscoverConfig};

let discovery = discover::discover(DiscoverConfig {
    endpoints: vec![
        "127.0.0.1:10001".parse().unwrap(),
        "127.0.0.1:10002".parse().unwrap(),
        "127.0.0.1:10003".parse().unwrap(),
    ],
    ..Default::default()
})?;
```

- **Vendors**: Axis. PRs for more welcome.
- **Scope**: RTSP only. No ONVIF, no WS-Discovery, no mDNS. IPv4 only.
- **Credentials**: anonymous-only for v1.
- **Targets**: mix `subnets` (expanded at `rtsp_port`, default 554) and `endpoints` (verbatim `SocketAddr`s) freely. Channel URLs use each target's actual port, not a hardcoded one.
- **Limits**: 65,536 combined hosts per scan, configurable `concurrency`, `connect_timeout`, `rtsp_timeout`.
- **Debugging unknown hosts**: `DiscoverEvent::HostUnmatched { host, server }` is emitted for every host that answered RTSP but whose `Server:` header did not match a known vendor. Inspecting `server` tells you exactly what to add to the vendor dispatch table.
- **Example**: `just run-discover 192.168.1.0/24` or `just run-discover 127.0.0.1:554,127.0.0.1:555`.

## Examples

Runnable integration templates for using cameras outside Dioxus (CLI, egui / iced, Tauri, daemons, anything). See the [examples](examples/) directory.

| Example | What it shows |
|---------|---------------|
| [`snapshot`](examples/snapshot.rs) | Open, grab one frame, save as PNG. `to_rgba8` + file I/O. |
| [`pump`](examples/pump.rs) | `pump::spawn` with a closure sink, `set_active` pause/resume, `capture_frame` while paused. The template for plugging cameras into your own runtime. |
| [`monitor`](examples/monitor.rs) | Camera hotplug event loop with `monitor` + `next_event`. |

```bash
just run-snapshot           # writes snapshot.png from the first camera
just run-pump               # 5-second stream + pause + capture demo
just run-monitor            # hotplug events until Ctrl-C
```

## Publishing

```bash
just publish      # cameras
just publish-dx   # dioxus-cameras
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
