use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chimeras::{Device, Frame, PixelFormat, Resolution, StreamConfig};
use dioxus::prelude::*;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP_CSS: Asset = asset!("/assets/app.css");

fn main() {
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new().with_menu(None).with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("chimeras demo")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(1100.0, 760.0)),
            ),
        )
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

    fn peek_data_url(&self) -> Option<String> {
        let frame = self.frame.lock().ok()?.clone()?;
        frame_to_data_url(&frame)
    }
}

fn frame_to_data_url(frame: &Frame) -> Option<String> {
    let rgb = chimeras::to_rgb8(frame).ok()?;
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(
            &rgb,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    let encoded = BASE64.encode(&png_bytes);
    Some(format!("data:image/png;base64,{encoded}"))
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
    let preview_url = use_signal(|| None::<String>);
    let saved_path = use_signal(|| None::<String>);

    let latest_frame = use_hook(LatestFrame::new);

    use_effect(move || {
        refresh_devices(devices, status, selected_index);
    });

    let render_preview_frame = latest_frame.clone();
    use_future(move || {
        let latest_frame = render_preview_frame.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(33)).await;
                if let Some(url) = latest_frame.peek_data_url() {
                    preview_url.clone().set(Some(url));
                }
            }
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
        document::Stylesheet { href: APP_CSS }
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
                if let Some(url) = preview_url() {
                    img { class: "preview-image", src: "{url}" }
                } else {
                    div { class: "preview-placeholder",
                        div { class: "placeholder-icon", "●" }
                        div { class: "placeholder-text",
                            if is_connected {
                                "Waiting for first frame..."
                            } else if device_count == 0 {
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
