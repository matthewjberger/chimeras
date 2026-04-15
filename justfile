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

# Check for unused dependencies with cargo-machete
udeps:
  cargo machete

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
