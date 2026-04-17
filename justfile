set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

_dx_version := "0.7.5"

export RUST_LOG := "info"

[private]
default:
  @just --list

# Build the workspace in release mode
build:
  cargo build --workspace --release

# Check the workspace
check:
  cargo check --workspace --all-targets

# Autoformat the workspace
format:
  cargo fmt --all

# Verify formatting
format-check:
  cargo fmt --all -- --check

# Lint the workspace
lint:
  cargo clippy --workspace --all-targets -- -D warnings

# Check just the cameras core library
check-lib:
  cargo check -p cameras --all-targets

# Lint just the cameras core library
lint-lib:
  cargo clippy -p cameras --all-targets -- -D warnings

# Check dioxus-cameras (with and without default features)
check-dx:
  cargo check -p dioxus-cameras --all-targets
  cargo check -p dioxus-cameras --no-default-features --all-targets

# Lint dioxus-cameras (with and without default features)
lint-dx:
  cargo clippy -p dioxus-cameras --all-targets -- -D warnings
  cargo clippy -p dioxus-cameras --no-default-features --all-targets -- -D warnings

# Check egui-cameras
check-egui:
  cargo check -p egui-cameras --all-targets

# Lint egui-cameras (with and without default features)
lint-egui:
  cargo clippy -p egui-cameras --all-targets -- -D warnings
  cargo clippy -p egui-cameras --no-default-features --all-targets -- -D warnings

# Build rustdoc for cameras, failing on broken links.
# (`--cfg docsrs` is set on the real docs.rs build via [package.metadata.docs.rs];
# we don't pass it here because `doc(cfg(...))` requires nightly.)
[unix]
doc:
  RUSTDOCFLAGS="-D warnings" cargo doc -p cameras --no-deps --all-features

[windows]
doc:
  $env:RUSTDOCFLAGS = "-D warnings"; cargo doc -p cameras --no-deps --all-features

# Build rustdoc for dioxus-cameras, failing on broken links.
[unix]
doc-dx:
  RUSTDOCFLAGS="-D warnings" cargo doc -p dioxus-cameras --no-deps --all-features

[windows]
doc-dx:
  $env:RUSTDOCFLAGS = "-D warnings"; cargo doc -p dioxus-cameras --no-deps --all-features

# Build rustdoc for egui-cameras, failing on broken links.
[unix]
doc-egui:
  RUSTDOCFLAGS="-D warnings" cargo doc -p egui-cameras --no-deps --all-features

[windows]
doc-egui:
  $env:RUSTDOCFLAGS = "-D warnings"; cargo doc -p egui-cameras --no-deps --all-features

# Run the Dioxus demo with hot-reloading
run-dioxus: _require-dx
  dx serve -p dioxus-demo --hotpatch

# Run the Dioxus demo in release mode
run-dioxus-release:
  cargo run -p dioxus-demo --release

# Run the egui demo
run-egui:
  cargo run -p egui-demo --release

# Take a single-frame snapshot from the first camera and write a PNG.
run-snapshot path="snapshot.png":
  cargo run -p cameras --example snapshot -- {{path}}

# Drive a camera with the pump: stream, pause, capture, resume, stop.
run-pump:
  cargo run -p cameras --example pump

# Stream camera hotplug events until Ctrl-C.
run-monitor:
  cargo run -p cameras --example monitor

# Collect a short burst from the first camera and save the sharpest frame.
run-sharpest path="sharpest.png":
  cargo run -p cameras --features analysis --example sharpest -- {{path}}

# Contrast-detection autofocus sweep on the first camera that has focus control.
run-autofocus:
  cargo run -p cameras --features "analysis,controls" --example autofocus

# Run mediamtx in the foreground to host rtsp://127.0.0.1:8554. Run this
# in one terminal, then `just rtsp-publish PATH` in another to push an
# MP4 into it. Requires mediamtx on PATH.
rtsp-host:
  mediamtx

# Publish a local MP4 file to the running mediamtx as an RTSP stream at
# rtsp://127.0.0.1:8554/<path>. Each unique path is an independent stream
# so you can run this in several terminals with different paths to feed
# the demo's grid view. Assumes `just rtsp-host` is running. Requires
# ffmpeg on PATH.
rtsp-publish file="test_video.mp4" path="live":
  ffmpeg -re -stream_loop -1 -i {{file}} -an -c:v copy -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:8554/{{path}}

# Check for unused dependencies with cargo-machete
udeps:
  cargo machete

# Dry-run publish cameras to crates.io
publish-dry:
  cargo publish -p cameras --dry-run

# Publish cameras to crates.io (requires cargo login)
publish:
  cargo publish -p cameras

# Dry-run publish dioxus-cameras to crates.io
publish-dry-dx:
  cargo publish -p dioxus-cameras --dry-run

# Publish dioxus-cameras to crates.io. cameras must already be on crates.io
# at the version dioxus-cameras depends on.
publish-dx:
  cargo publish -p dioxus-cameras

# Dry-run publish egui-cameras to crates.io
publish-dry-egui:
  cargo publish -p egui-cameras --dry-run

# Publish egui-cameras to crates.io. cameras must already be on crates.io
# at the version egui-cameras depends on.
publish-egui:
  cargo publish -p egui-cameras

# Publish all three crates in dependency order: cameras, then both integrations.
# cargo publish waits for the index to update before returning, so no sleep needed.
publish-all:
  just publish
  just publish-dx
  just publish-egui

# Display toolchain versions
@versions:
  rustc --version
  cargo fmt -- --version
  cargo clippy -- --version
  rustup --version

[private]
[unix]
_require-dx:
  @command -v dx >/dev/null 2>&1 || (echo "dx not found, installing..." && cargo install dioxus-cli@{{_dx_version}} --locked)

[private]
[windows]
_require-dx:
  @if (-not (Get-Command dx -ErrorAction SilentlyContinue)) { Write-Host "dx not found, installing..."; cargo install dioxus-cli@{{_dx_version}} --locked }
