<h1 align="center">chimeras</h1>

<p align="center">
  <a href="https://github.com/matthewjberger/chimeras"><img alt="github" src="https://img.shields.io/badge/github-matthewjberger/chimeras-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20"></a>
  <a href="https://crates.io/crates/chimeras"><img alt="crates.io" src="https://img.shields.io/crates/v/chimeras.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://docs.rs/chimeras"><img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-chimeras-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20"></a>
  <a href="https://github.com/matthewjberger/chimeras/blob/main/LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=for-the-badge&labelColor=555555" height="20"></a>
</p>

<p align="center"><strong>A cross-platform camera library for Rust, built with data-oriented design.</strong></p>

<p align="center">
  <code>cargo add chimeras</code>
</p>

`chimeras` is a cross-platform camera library built with data-oriented design. The library exposes plain data — `Device`, `Capabilities`, `FormatDescriptor`, `StreamConfig`, `Frame` — and free functions that operate on that data. Every public type has public fields. Format negotiation is explicit: you probe, you pick, you open. Errors are typed. Platform dispatch happens at compile time via `cfg` and an associated-type `Backend` trait; there are zero trait objects anywhere in the library.

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

## API

All operations are free functions at the crate root. No methods on `Device`, no builder patterns, no hidden state:

```rust
chimeras::devices()                                         -> Result<Vec<Device>, Error>
chimeras::probe(&Device)                                    -> Result<Capabilities, Error>
chimeras::open(&Device, StreamConfig)                       -> Result<Camera, Error>
chimeras::next_frame(&Camera, Duration)                     -> Result<Frame, Error>
chimeras::try_next_frame(&Camera)                           -> Option<Result<Frame, Error>>
chimeras::monitor()                                         -> Result<DeviceMonitor, Error>
chimeras::next_event(&DeviceMonitor, Duration)              -> Result<DeviceEvent, Error>
chimeras::try_next_event(&DeviceMonitor)                    -> Option<DeviceEvent>
chimeras::best_format(&Capabilities, &StreamConfig)         -> Option<FormatDescriptor>
chimeras::to_rgb8(&Frame)                                   -> Result<Vec<u8>, Error>
chimeras::to_rgba8(&Frame)                                  -> Result<Vec<u8>, Error>
```

## Features

### Data-Oriented API

Types hold data. Functions operate on data. No `impl` blocks with hidden accessors, no trait objects, no inheritance:

```rust
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub position: Position,
    pub transport: Transport,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub timestamp: Duration,
    pub pixel_format: PixelFormat,
    pub plane_primary: Bytes,
    pub plane_secondary: Bytes,
}
```

Backends are selected at compile time via `cfg`. Each platform module is a `Driver` struct that implements the internal `Backend` trait with an associated `SessionHandle` type. The compiler verifies all platforms implement the same surface — if you add a method to `Backend`, every platform must implement it. Users never see a `dyn Backend`.

### Explicit Format Negotiation

Nothing is auto-selected under the hood. `probe` returns every format the device supports with resolution, framerate range, and pixel format. You pick one and hand it to `open` via `StreamConfig`:

```rust
let capabilities = chimeras::probe(device)?;
for format in &capabilities.formats {
    println!(
        "{}x{} @ {:.0}-{:.0} fps  {:?}",
        format.resolution.width,
        format.resolution.height,
        format.framerate_range.min,
        format.framerate_range.max,
        format.pixel_format,
    );
}
```

If you don't want to pick manually, `best_format` returns the closest match to a `StreamConfig`:

```rust
let picked = chimeras::best_format(&capabilities, &config).expect("no fallback");
let camera = chimeras::open(device, StreamConfig { pixel_format: picked.pixel_format, ..config })?;
```

### Push-Based Frame Delivery

Native camera APIs push frames: AVFoundation calls a delegate, Media Foundation fires a source reader callback, V4L2 blocks on `VIDIOC_DQBUF`. `chimeras` matches that model. Each `Camera` owns a worker thread and a bounded crossbeam channel. The consumer pulls frames with a timeout:

```rust
loop {
    match chimeras::next_frame(&camera, Duration::from_millis(500)) {
        Ok(frame) => process(frame),
        Err(chimeras::Error::Timeout) => continue,
        Err(chimeras::Error::StreamEnded) => break,
        Err(other) => return Err(other),
    }
}
```

If the consumer falls behind, old frames are dropped, not buffered. The camera thread never blocks.

### Typed Errors

```rust
pub enum Error {
    PermissionDenied,
    DeviceNotFound(String),
    DeviceInUse,
    FormatNotSupported,
    Timeout,
    StreamEnded,
    MjpegDecode(String),
    BackendNotImplemented { platform: &'static str },
    Backend { platform: &'static str, message: String },
}
```

No stringly-typed error variants. Match on the shape, not the text.

### Pixel Format Conversion

Built-in conversion to RGB8 and RGBA8 from BGRA8, RGBA8, YUYV, NV12, and MJPEG (via `zune-jpeg`). Stride is honored. Conversion is explicit — `chimeras` never converts behind your back:

```rust
let rgb = chimeras::to_rgb8(&frame)?;
let rgba = chimeras::to_rgba8(&frame)?;
```

### Hotplug

`DeviceMonitor` emits `DeviceEvent::Added` / `DeviceEvent::Removed` as cameras appear and disappear. The monitor is polling-based (one second interval) on all platforms for consistency:

```rust
let monitor = chimeras::monitor()?;
loop {
    match chimeras::next_event(&monitor, Duration::from_secs(1)) {
        Ok(chimeras::DeviceEvent::Added(device))    => println!("+ {}", device.name),
        Ok(chimeras::DeviceEvent::Removed(id))      => println!("- {}", id.0),
        Err(chimeras::Error::Timeout)               => continue,
        Err(other)                                  => return Err(other),
    }
}
```

### RAII Resource Handles

`Camera::drop` stops the stream and releases the underlying session. `DeviceMonitor::drop` signals its worker and joins. No `close()` methods. No half-initialized states. The only state a camera can be in is "streaming"; to stop, you drop it.

### Compile-Time Backend Contract

Platform backends are selected with `cfg`. Each is a `Driver` struct that implements an internal trait:

```rust
pub trait Backend {
    type SessionHandle;
    fn devices() -> Result<Vec<Device>, Error>;
    fn probe(id: &DeviceId) -> Result<Capabilities, Error>;
    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error>;
    fn monitor() -> Result<DeviceMonitor, Error>;
}
```

No `Box<dyn Backend>`, no virtual dispatch. The `ActiveBackend` type alias resolves at compile time. The trait is a contract: the compiler rejects any platform module that doesn't implement every item.

### Minimal Unsafe

Every unsafe block is at an FFI boundary — ObjC message sends on macOS, COM calls on Windows, `std::ptr::read` for IMFActivate array ownership transfer. The shared core (`types`, `error`, `camera`, `monitor`, `convert`, `backend`, `lib`) contains zero `unsafe`. The Linux backend contains zero `unsafe` — the `v4l` crate wraps all ioctl / mmap internally. `#![deny(unsafe_op_in_unsafe_fn)]` is on, so every unsafe operation needs an explicit `unsafe { ... }` block.

## Platform Support

| Platform | Backend | Status |
|----------|---------|--------|
| macOS    | AVFoundation via `objc2` + `objc2-av-foundation` | implemented |
| Windows  | Media Foundation via `windows` | implemented |
| Linux    | V4L2 via `v4l` (mmap streaming) | implemented |
| Others   | `Error::BackendNotImplemented` stub | graceful fallback |

## Design Principles

| Choice | Rationale |
|---|---|
| Modern `objc2 0.6` bindings on macOS | ObjC exceptions surface as typed `Error` values through `extern "C-unwind"`. |
| Typed error enum via `thiserror` | Consumers match on shape, not string text. |
| Explicit probe → pick → open | The caller is always in control of which format is negotiated. |
| Push-based delivery | Matches the native model of every platform (AVFoundation delegate, Media Foundation source reader, V4L2 `DQBUF`). A worker thread pushes to a bounded channel; consumers pull. |
| MJPEG decode via `zune-jpeg` | Safe, pure-Rust, tiny binary footprint. |
| Compile-time trait + `cfg` dispatch | One associated-type `Backend` trait enforces the contract across platforms with zero runtime polymorphism. |
| RAII handles | `Camera::drop` stops the stream, `DeviceMonitor::drop` joins its worker. |
| Zero `unsafe` in public API | Unsafe is confined to platform-specific FFI modules. |
| Hotplug first-class | `DeviceMonitor` emits `Added` / `Removed` events on all platforms. |

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
