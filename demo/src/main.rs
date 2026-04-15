use chimeras::{Device, Frame, PixelFormat, Resolution, StreamConfig};
use dioxus::desktop::wry::http::Response as HttpResponse;
use dioxus::prelude::*;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP_CSS: &str = include_str!("../assets/app.css");
const PROTOCOL: &str = "chimeras";

fn main() {
    let latest_frame = LatestFrame::new();
    let latest_for_protocol = latest_frame.clone();

    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new()
                .with_menu(None)
                .with_custom_protocol(PROTOCOL.to_string(), move |_id, _request| {
                    serve_frame(&latest_for_protocol)
                })
                .with_window(
                    dioxus::desktop::WindowBuilder::new()
                        .with_title("chimeras demo")
                        .with_inner_size(dioxus::desktop::LogicalSize::new(1100.0, 760.0)),
                ),
        )
        .with_context(latest_frame)
        .launch(App);
}

struct Session {
    #[allow(dead_code)]
    pump: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
struct LatestFrame {
    frame: Arc<Mutex<Option<Frame>>>,
}

impl LatestFrame {
    fn new() -> Self {
        Self {
            frame: Arc::new(Mutex::new(None)),
        }
    }

    fn set(&self, frame: Frame) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(frame);
        }
    }

    fn take(&self) -> Option<Frame> {
        self.frame.lock().ok().and_then(|mut slot| slot.take())
    }

    fn snapshot(&self) -> Option<Frame> {
        self.frame.lock().ok()?.clone()
    }
}

fn serve_frame(latest: &LatestFrame) -> HttpResponse<Cow<'static, [u8]>> {
    let bmp = latest
        .snapshot()
        .map(|frame| frame_to_bmp(&frame))
        .unwrap_or_else(placeholder_bmp);
    HttpResponse::builder()
        .status(200)
        .header("Content-Type", "image/bmp")
        .header("Cache-Control", "no-store")
        .body(Cow::Owned(bmp))
        .unwrap()
}

fn placeholder_bmp() -> Vec<u8> {
    let pixel = [0u8, 0, 0, 0];
    let mut buffer = Vec::with_capacity(58);
    buffer.extend_from_slice(b"BM");
    buffer.extend_from_slice(&58u32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&54u32.to_le_bytes());
    buffer.extend_from_slice(&40u32.to_le_bytes());
    buffer.extend_from_slice(&1i32.to_le_bytes());
    buffer.extend_from_slice(&(-1i32).to_le_bytes());
    buffer.extend_from_slice(&1u16.to_le_bytes());
    buffer.extend_from_slice(&24u16.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&4u32.to_le_bytes());
    buffer.extend_from_slice(&2835i32.to_le_bytes());
    buffer.extend_from_slice(&2835i32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&pixel);
    buffer
}

fn frame_to_bmp(frame: &Frame) -> Vec<u8> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let stride = frame.stride as usize;
    let effective_stride = if stride == 0 { width * 4 } else { stride };
    let row_bytes = width * 3;
    let row_padded = (row_bytes + 3) & !3;
    let pad = row_padded - row_bytes;
    let pixel_data_size = row_padded * height;
    let file_size = 54 + pixel_data_size;

    let mut buffer = Vec::with_capacity(file_size);
    buffer.extend_from_slice(b"BM");
    buffer.extend_from_slice(&(file_size as u32).to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&54u32.to_le_bytes());

    buffer.extend_from_slice(&40u32.to_le_bytes());
    buffer.extend_from_slice(&(width as i32).to_le_bytes());
    buffer.extend_from_slice(&(-(height as i32)).to_le_bytes());
    buffer.extend_from_slice(&1u16.to_le_bytes());
    buffer.extend_from_slice(&24u16.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&(pixel_data_size as u32).to_le_bytes());
    buffer.extend_from_slice(&2835i32.to_le_bytes());
    buffer.extend_from_slice(&2835i32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());

    let padding = [0u8; 3];
    let data = &frame.plane_primary;
    for row in 0..height {
        let row_start = row * effective_stride;
        let row_end = (row_start + width * 4).min(data.len());
        let row_slice = &data[row_start.min(data.len())..row_end];
        for pixel in row_slice.chunks_exact(4) {
            buffer.push(pixel[0]);
            buffer.push(pixel[1]);
            buffer.push(pixel[2]);
        }
        if pad > 0 {
            buffer.extend_from_slice(&padding[..pad]);
        }
    }

    buffer
}

fn refresh_devices(
    mut devices: Signal<Vec<Device>>,
    mut status: Signal<String>,
    mut selected_index: Signal<usize>,
) {
    match chimeras::devices() {
        Ok(list) => {
            let count = list.len();
            if *selected_index.peek() >= count {
                selected_index.set(0);
            }
            devices.set(list);
            status.set(match count {
                0 => "No cameras detected".into(),
                1 => "1 camera available".into(),
                n => format!("{n} cameras available"),
            });
        }
        Err(error) => status.set(format!("Enumerate failed: {error}")),
    }
}

#[component]
fn App() -> Element {
    let devices = use_signal(Vec::<Device>::new);
    let selected_index = use_signal(|| 0usize);
    let status = use_signal(|| "Idle".to_string());
    let session: Signal<Option<Session>> = use_signal(|| None);
    let preview_tick = use_signal(|| 0u64);
    let saved_path = use_signal(|| None::<String>);

    let latest_frame = use_context::<LatestFrame>();

    use_effect(move || {
        refresh_devices(devices, status, selected_index);
    });

    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_millis(33)).await;
            let next = preview_tick.peek().wrapping_add(1);
            preview_tick.clone().set(next);
        }
    });

    let refresh = move |_| {
        refresh_devices(devices, status, selected_index);
    };

    let connect = {
        let latest_frame = latest_frame.clone();
        move |_| {
            let selected = *selected_index.peek();
            let Some(device) = devices.peek().get(selected).cloned() else {
                status.clone().set("No camera selected".into());
                return;
            };

            session.clone().set(None);
            status
                .clone()
                .set(format!("Connecting to {}...", device.name));

            let config = StreamConfig {
                resolution: Resolution {
                    width: 1280,
                    height: 720,
                },
                framerate: 30,
                pixel_format: PixelFormat::Bgra8,
            };

            match chimeras::open(&device, config) {
                Ok(camera) => {
                    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let shutdown_for_pump = Arc::clone(&shutdown);
                    let latest_for_pump = latest_frame.clone();
                    let pump = std::thread::Builder::new()
                        .name("demo-camera-pump".into())
                        .spawn(move || {
                            let camera = camera;
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                while !shutdown_for_pump.load(std::sync::atomic::Ordering::Relaxed)
                                {
                                    match chimeras::next_frame(&camera, Duration::from_millis(500))
                                    {
                                        Ok(frame) => latest_for_pump.set(frame),
                                        Err(chimeras::Error::Timeout) => continue,
                                        Err(_) => break,
                                    }
                                }
                            }));
                        })
                        .expect("failed to spawn camera pump thread");
                    session.clone().set(Some(Session {
                        pump: Some(pump),
                        shutdown,
                    }));
                    status.clone().set(format!("Streaming: {}", device.name));
                }
                Err(error) => status.clone().set(format!("Open failed: {error}")),
            }
        }
    };

    let capture = {
        let latest_frame = latest_frame.clone();
        move |_| {
            let Some(frame) = latest_frame.take() else {
                status.clone().set("No frame to capture".into());
                return;
            };
            let rgb = match chimeras::to_rgb8(&frame) {
                Ok(rgb) => rgb,
                Err(error) => {
                    status.clone().set(format!("Decode failed: {error}"));
                    return;
                }
            };
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let path = std::env::temp_dir().join(format!("chimeras-capture-{timestamp}.png"));
            let file = match std::fs::File::create(&path) {
                Ok(file) => file,
                Err(error) => {
                    status.clone().set(format!("Save failed: {error}"));
                    return;
                }
            };
            let encoder = PngEncoder::new(file);
            if let Err(error) = encoder.write_image(
                &rgb,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgb8,
            ) {
                status.clone().set(format!("Save failed: {error}"));
                return;
            }
            saved_path.clone().set(Some(path.to_string_lossy().into()));
            status.clone().set(format!("Saved to {}", path.display()));
        }
    };

    let device_count = devices().len();
    let is_connected = session.peek().is_some();
    let connect_label = if is_connected { "Reconnect" } else { "Connect" };

    rsx! {
        style { {APP_CSS} }
        div { class: "app",
            header { class: "title-bar",
                h1 { "chimeras" }
                span { class: "subtitle", "cross-platform camera demo" }
            }

            section { class: "controls",
                div { class: "field",
                    span { class: "field-label", "Camera" }
                    select {
                        class: "input",
                        disabled: device_count == 0,
                        onchange: move |event| {
                            if let Ok(index) = event.value().parse::<usize>() {
                                selected_index.clone().set(index);
                            }
                        },
                        if device_count == 0 {
                            option { "No cameras detected" }
                        } else {
                            for (index, device) in devices().iter().enumerate() {
                                option { value: "{index}", "{device.name}" }
                            }
                        }
                    }
                }

                div { class: "button-row",
                    button {
                        class: "btn btn-ghost",
                        onclick: refresh,
                        "Refresh"
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: device_count == 0,
                        onclick: connect,
                        "{connect_label}"
                    }
                    button {
                        class: "btn btn-accent",
                        disabled: !is_connected,
                        onclick: capture,
                        "Capture"
                    }
                }
            }

            section { class: "status",
                span { class: "status-label", "Status" }
                span {
                    class: "status-dot",
                    "data-state": if is_connected { "live" } else { "idle" },
                }
                span { class: "status-value", "{status()}" }
            }

            section { class: "preview",
                if is_connected {
                    img {
                        class: "preview-image",
                        src: "http://{PROTOCOL}.localhost/frame.bmp?t={preview_tick()}",
                    }
                } else {
                    div { class: "preview-placeholder",
                        div { class: "placeholder-icon", "●" }
                        div { class: "placeholder-text",
                            if device_count == 0 {
                                "Plug in a camera, grant permission, and press Refresh"
                            } else {
                                "Press Connect to start streaming"
                            }
                        }
                    }
                }
            }

            if let Some(path) = saved_path() {
                section { class: "saved-note",
                    span { class: "saved-label", "Last capture" }
                    code { class: "saved-path", "{path}" }
                }
            }
        }
    }
}
