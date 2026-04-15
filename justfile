set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

_dx_version := "0.7.3"

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

# Check just the published library
check-lib:
  cargo check -p chimeras --all-targets

# Lint just the published library
lint-lib:
  cargo clippy -p chimeras --all-targets -- -D warnings

# Run the chimeras-demo with hot-reloading
run: _require-dx
  dx serve -p chimeras-demo --hotpatch

# Run the demo in release mode
run-release:
  cargo run -p chimeras-demo --release

# Enumerate devices and their capabilities
run-list:
  cargo run -p chimeras --example list

# Capture 30 frames from the first camera
run-capture:
  cargo run -p chimeras --example capture

# Serve a local MP4 file as an RTSP stream on rtsp://127.0.0.1:8554/live.
# Requires mediamtx and ffmpeg on PATH. Video stream is passed through
# as-is (most MP4s are H.264); audio is dropped because mediamtx rejects
# many audio codecs in the RTSP publish path. chimeras does not yet
# decode H.264/H.265 RTSP streams, so `just run` against this URL
# will surface Error::FormatNotSupported until that lands.
[unix]
serve-rtsp args="test_video.mp4":
  @echo "note: chimeras does not yet decode H.264/H.265 RTSP streams"
  mediamtx &
  sleep 1
  ffmpeg -re -stream_loop -1 -i {{args}} -an -c:v copy -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:8554/live

[windows]
serve-rtsp args="test_video.mp4":
  @echo "note: chimeras does not yet decode H.264/H.265 RTSP streams"
  start mediamtx
  powershell -Command "Start-Sleep -Seconds 1"
  ffmpeg -re -stream_loop -1 -i {{args}} -an -c:v copy -f rtsp -rtsp_transport tcp rtsp://127.0.0.1:8554/live

# Check for unused dependencies with cargo-machete
udeps:
  cargo machete

# Dry-run publish to crates.io
publish-dry:
  cargo publish -p chimeras --dry-run

# Publish chimeras to crates.io (requires cargo login)
publish:
  cargo publish -p chimeras

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
