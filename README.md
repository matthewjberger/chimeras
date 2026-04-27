<h1 align="center">
  <img src="assets/cameras.svg" alt="" width="48" align="center">
  cameras
</h1>

<p align="center"><strong>A cross-platform camera library for Rust, plus first-class UI-framework integrations.</strong></p>

<p align="center">
  <a href="https://github.com/matthewjberger/cameras"><img alt="github" src="https://img.shields.io/badge/github-matthewjberger/cameras-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20"></a>
  <a href="https://github.com/matthewjberger/cameras/blob/main/LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=for-the-badge&labelColor=555555" height="20"></a>
</p>

https://github.com/user-attachments/assets/8e2f4a5f-0e70-4cf7-a8de-942c0d2fada5

## Crates

| Crate | crates.io | docs.rs | Purpose |
|-------|-----------|---------|---------|
| [`cameras`](crates/cameras/) | [![crates.io](https://img.shields.io/crates/v/cameras.svg?logo=rust&color=fc8d62)](https://crates.io/crates/cameras) | [![docs.rs](https://img.shields.io/badge/docs.rs-cameras-66c2a5?logo=docs.rs)](https://docs.rs/cameras) | Enumerate, probe, open, and stream cameras. macOS (AVFoundation), Windows (Media Foundation), Linux (V4L2). Optional RTSP. |
| [`dioxus-cameras`](crates/dioxus-cameras/) | [![crates.io](https://img.shields.io/crates/v/dioxus-cameras.svg?logo=rust&color=fc8d62)](https://crates.io/crates/dioxus-cameras) | [![docs.rs](https://img.shields.io/badge/docs.rs-dioxus--cameras-66c2a5?logo=docs.rs)](https://docs.rs/dioxus-cameras) | Hooks + components for using cameras inside a Dioxus desktop app. WebGL2 preview rendering. |
| [`egui-cameras`](crates/egui-cameras/) | [![crates.io](https://img.shields.io/crates/v/egui-cameras.svg?logo=rust&color=fc8d62)](https://crates.io/crates/egui-cameras) | [![docs.rs](https://img.shields.io/badge/docs.rs-egui--cameras-66c2a5?logo=docs.rs)](https://docs.rs/egui-cameras) | Helpers for using cameras inside an egui / eframe app. Frame-to-texture conversion. |

Each integration crate is thin; almost everything lives in `cameras` itself (`cameras::pump`, `cameras::source`, `cameras::monitor`, etc.). The integration crates just bridge a running `cameras::pump::Pump` to the target UI framework's texture / canvas model.

## Layout

```
cameras/
├── crates/
│   ├── cameras/         ← the core library
│   ├── dioxus-cameras/  ← Dioxus integration
│   └── egui-cameras/    ← egui integration
└── apps/
    ├── dioxus-demo/      ← multi-stream grid, USB + RTSP sources
    └── egui-demo/        ← single-stream viewer with pause / snapshot
```

## Demos

```bash
just run-dioxus   # Dioxus desktop app: multi-stream grid, USB + RTSP
just run-egui     # egui / eframe app: single-stream viewer + snapshot
```

## Versioning

All three crates ship in lockstep on the same major + minor version. Use matching minor versions across your `Cargo.toml` when depending on them.

## Publishing

```bash
just publish       # cameras
just publish-dx    # dioxus-cameras
just publish-egui  # egui-cameras
```

## License

Dual-licensed under either of

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
