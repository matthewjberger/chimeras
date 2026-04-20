<h1 align="center">egui-cameras</h1>

<p align="center">
  <a href="https://github.com/matthewjberger/cameras"><img alt="github" src="https://img.shields.io/badge/github-matthewjberger/cameras-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20"></a>
  <a href="https://crates.io/crates/egui-cameras"><img alt="crates.io" src="https://img.shields.io/crates/v/egui-cameras.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20"></a>
  <a href="https://docs.rs/egui-cameras"><img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-egui--cameras-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20"></a>
  <a href="https://github.com/matthewjberger/cameras/blob/main/LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=for-the-badge&labelColor=555555" height="20"></a>
</p>

<p align="center"><strong>Drop live camera streams into your egui / eframe app.</strong></p>

<p align="center">
  <code>cargo add egui-cameras</code>
</p>

`egui-cameras` is the egui integration for [`cameras`](https://crates.io/crates/cameras), a cross-platform camera library for Rust. It owns the thin glue between a running `cameras::pump::Pump` and an `egui::TextureHandle`, so you can render live camera frames as an `egui::Image` with a few lines of code.

Every camera-side primitive (pause / resume pump, single-frame capture, unified `CameraSource`, hotplug monitor) lives upstream in `cameras` itself and is re-exported from this crate as `egui_cameras::cameras`, so a single dependency is enough.

## Quick Start

```toml
[dependencies]
egui-cameras = "0.1"
eframe = "0.32"
```

```rust
use egui_cameras::cameras::{self, PixelFormat, Resolution, StreamConfig};
use eframe::egui;

struct App {
    stream: egui_cameras::Stream,
}

impl App {
    fn new() -> Result<Self, cameras::Error> {
        let devices = cameras::devices()?;
        let device = devices.first().ok_or(cameras::Error::DeviceNotFound("no cameras".into()))?;
        let config = StreamConfig {
            resolution: Resolution { width: 1280, height: 720 },
            framerate: 30,
            pixel_format: PixelFormat::Bgra8,
        };
        let camera = cameras::open(device, config)?;
        Ok(Self { stream: egui_cameras::spawn(camera) })
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui_cameras::update_texture(&mut self.stream, ctx).ok();
        egui::CentralPanel::default().show(ctx, |ui| {
            egui_cameras::show(&self.stream, ui);
        });
        ctx.request_repaint();
    }
}
```

## What's in the box

| Item | Purpose |
|------|---------|
| `Stream` | Bundle of a `Pump` + `Sink` + `TextureHandle`. Holds everything one live camera needs. |
| `Sink` | Shared slot the pump writes each frame into. Cheap to clone. |
| `spawn(camera) -> Stream` | Convenience: spawn a pump and wire it to a fresh `Stream` with a default texture name. |
| `spawn_named(camera, name) -> Stream` | Like `spawn`, but lets you name the texture (useful for multi-camera apps). |
| `spawn_pump(camera, sink) -> Pump` | Lower-level: spawn a pump that writes into your own `Sink`. |
| `publish_frame(sink, frame)` | Write a frame into a sink (for custom pump code). |
| `take_frame(sink) -> Option<Frame>` | Pull the latest frame out of a sink. |
| `frame_to_color_image(frame)` | Convert a cameras `Frame` into an `egui::ColorImage`. |
| `update_texture(stream, ctx)` | Upload the latest frame to the stream's texture. Call each `update` tick. |
| `show(stream, ui)` | Draw the texture as an `egui::Image` scaled to fit the available area. |

Pump controls (`set_active`, `capture_frame`, `stop_and_join`) are re-exported directly from `cameras::pump`.

## Pause + snapshot

Pause the pump when the user isn't looking, grab fresh frames on demand without closing the camera.

```rust
use egui_cameras::{capture_frame, set_active};

// Park the pump (no per-frame Rust work; camera stays open):
set_active(&stream.pump, false);

// Grab a fresh snapshot regardless of pause state:
let frame = capture_frame(&stream.pump);
```

## Features

| Feature | Default | Description |
|---------|:-------:|-------------|
| `rtsp` | off | Forwards to `cameras/rtsp`; enables `CameraSource::Rtsp` on macOS and Windows. |
| `discover` | off | Forwards to `cameras/discover`; enables the `DiscoveryWidget` for scanning a subnet for Axis RTSP cameras. |

## Discovery

With the `discover` feature, `start_discovery(config)` returns a `DiscoverySession`. Call `poll_discovery(&mut session)` each frame and `show_discovery(&session, ui)` to render a clickable result list; a click yields a `DiscoveredCamera` that can be passed to `cameras::open_source`. Configs mix CIDR `subnets` and explicit `host:port` `endpoints`, so port-forwarded tunnels work alongside a LAN scan. `poll_discovery` ignores `DiscoverEvent::HostUnmatched` internally, callers that need to see the raw `Server:` header for debugging should use the lower-level `cameras::discover` API directly. The `apps/egui-demo` Discover panel accepts comma-separated CIDRs and/or `host:port` entries in its Targets field.

## Versioning

`egui-cameras` pins a specific minor version of `cameras` via `pub use cameras;`, so installing `egui-cameras = "0.1"` gives you a compatible `cameras` automatically.

## Example

A runnable example lives at [`apps/egui-demo`](https://github.com/matthewjberger/cameras/tree/main/apps/egui-demo) in the repository: live preview, pause toggle, take-picture button.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
