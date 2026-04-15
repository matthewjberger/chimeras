use bytes::Bytes;
use chimeras::{Credentials, Device, Frame, PixelFormat, Resolution, StreamConfig};
use dioxus::prelude::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP_CSS: &str = include_str!("../assets/app.css");
const PREVIEW_JS: &str = include_str!("../assets/preview.js");

const PREVIEW_MAGIC: [u8; 4] = *b"CHIM";
const PREVIEW_VERSION: u8 = 1;
const PREVIEW_FORMAT_NONE: u8 = 0;
const PREVIEW_FORMAT_NV12: u8 = 1;
const PREVIEW_FORMAT_BGRA: u8 = 2;
const PREVIEW_FORMAT_RGBA: u8 = 3;
const PREVIEW_HEADER_LEN: usize = 24;

fn main() {
    let registry = Registry::new();
    let registry_for_server = registry.clone();
    let preview_port =
        start_preview_server(registry_for_server).expect("failed to start preview server");

    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new().with_menu(None).with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("chimeras demo")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(1400.0, 900.0)),
            ),
        )
        .with_context(registry)
        .with_context(PreviewPort(preview_port))
        .launch(App);
}

#[derive(Clone)]
struct PreviewPort(u16);

#[derive(Clone)]
struct Registry {
    inner: Arc<Mutex<HashMap<u32, LatestFrame>>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn get_or_create(&self, id: u32) -> LatestFrame {
        let mut guard = self.inner.lock().expect("registry mutex poisoned");
        guard.entry(id).or_insert_with(LatestFrame::new).clone()
    }

    fn remove(&self, id: u32) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.remove(&id);
        }
    }

    fn snapshot(&self, id: u32) -> Option<(Frame, u32)> {
        let guard = self.inner.lock().ok()?;
        let latest = guard.get(&id)?;
        latest.snapshot_with_counter()
    }
}

#[derive(Clone)]
struct LatestFrame {
    frame: Arc<Mutex<Option<Frame>>>,
    counter: Arc<AtomicU32>,
}

impl LatestFrame {
    fn new() -> Self {
        Self {
            frame: Arc::new(Mutex::new(None)),
            counter: Arc::new(AtomicU32::new(0)),
        }
    }

    fn set(&self, frame: Frame) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(frame);
            self.counter.fetch_add(1, Ordering::Release);
        }
    }

    fn snapshot_with_counter(&self) -> Option<(Frame, u32)> {
        let frame = self.frame.lock().ok()?.clone()?;
        Some((frame, self.counter.load(Ordering::Acquire)))
    }
}

fn start_preview_server(registry: Registry) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new()
        .name("chimeras-preview-server".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let registry = registry.clone();
                let _ = std::thread::Builder::new()
                    .name("chimeras-preview-conn".into())
                    .spawn(move || {
                        let _ = stream.set_nodelay(true);
                        let _ = handle_preview_connection(stream, &registry);
                    });
            }
        })?;
    Ok(port)
}

fn handle_preview_connection(mut stream: TcpStream, registry: &Registry) -> std::io::Result<()> {
    let mut request_buf = [0u8; 2048];
    loop {
        let n = stream.read(&mut request_buf)?;
        if n == 0 {
            return Ok(());
        }
        let id = parse_preview_id(&request_buf[..n]);
        write_preview_response(&mut stream, registry, id)?;
    }
}

fn parse_preview_id(request_bytes: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(request_bytes).ok()?;
    let path = text.split_whitespace().nth(1)?;
    let rest = path.strip_prefix("/preview/")?;
    let id_str = rest.strip_suffix(".bin")?;
    id_str.parse().ok()
}

fn write_preview_response(
    stream: &mut TcpStream,
    registry: &Registry,
    id: Option<u32>,
) -> std::io::Result<()> {
    let parts = match id.and_then(|id| registry.snapshot(id)) {
        Some((frame, counter)) => preview_parts(&frame, counter),
        None => PreviewParts {
            header: preview_header(PREVIEW_FORMAT_NONE, 0, 0, 0, 0),
            primary: None,
            secondary: None,
        },
    };
    let total_body_len = parts.header.len()
        + parts.primary.as_ref().map(|b| b.len()).unwrap_or(0)
        + parts.secondary.as_ref().map(|b| b.len()).unwrap_or(0);
    let http_header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: keep-alive\r\n\r\n",
        total_body_len
    );
    stream.write_all(http_header.as_bytes())?;
    stream.write_all(&parts.header)?;
    if let Some(primary) = &parts.primary {
        stream.write_all(primary)?;
    }
    if let Some(secondary) = &parts.secondary {
        stream.write_all(secondary)?;
    }
    Ok(())
}

struct PreviewParts {
    header: Vec<u8>,
    primary: Option<Bytes>,
    secondary: Option<Bytes>,
}

fn preview_parts(frame: &Frame, counter: u32) -> PreviewParts {
    match frame.pixel_format {
        PixelFormat::Nv12 => PreviewParts {
            header: preview_header(
                PREVIEW_FORMAT_NV12,
                frame.width,
                frame.height,
                frame.stride,
                counter,
            ),
            primary: Some(frame.plane_primary.clone()),
            secondary: Some(frame.plane_secondary.clone()),
        },
        PixelFormat::Bgra8 => {
            let stride = if frame.stride == 0 {
                frame.width * 4
            } else {
                frame.stride
            };
            PreviewParts {
                header: preview_header(
                    PREVIEW_FORMAT_BGRA,
                    frame.width,
                    frame.height,
                    stride,
                    counter,
                ),
                primary: Some(frame.plane_primary.clone()),
                secondary: None,
            }
        }
        _ => {
            let Ok(rgba) = chimeras::to_rgba8(frame) else {
                return PreviewParts {
                    header: preview_header(PREVIEW_FORMAT_NONE, 0, 0, 0, counter),
                    primary: None,
                    secondary: None,
                };
            };
            let stride = frame.width * 4;
            PreviewParts {
                header: preview_header(
                    PREVIEW_FORMAT_RGBA,
                    frame.width,
                    frame.height,
                    stride,
                    counter,
                ),
                primary: Some(Bytes::from(rgba)),
                secondary: None,
            }
        }
    }
}

fn preview_header(format: u8, width: u32, height: u32, stride: u32, counter: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(PREVIEW_HEADER_LEN);
    header.extend_from_slice(&PREVIEW_MAGIC);
    header.push(PREVIEW_VERSION);
    header.push(format);
    header.extend_from_slice(&[0u8, 0u8]);
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    header.extend_from_slice(&stride.to_le_bytes());
    header.extend_from_slice(&counter.to_le_bytes());
    header
}

struct Session {
    #[allow(dead_code)]
    pump: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.pump.take() {
            let _ = handle.join();
        }
    }
}

enum ConnectOutcome {
    Connected { session: Session, label: String },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum SourceMode {
    Usb,
    Rtsp,
}

#[derive(Clone, Default)]
struct DeviceList(Arc<Mutex<Vec<Device>>>);

impl DeviceList {
    fn refresh(&self) {
        if let Ok(list) = chimeras::devices()
            && let Ok(mut slot) = self.0.lock()
        {
            *slot = list;
        }
    }

    fn current(&self) -> Vec<Device> {
        self.0.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
}

#[component]
fn App() -> Element {
    let next_id = use_signal(|| 1u32);
    let stream_ids = use_signal(|| vec![0u32]);
    let device_list = use_hook(DeviceList::default);
    let devices = use_signal(Vec::<Device>::new);

    {
        let device_list = device_list.clone();
        let mut devices = devices;
        use_effect(move || {
            device_list.refresh();
            devices.set(device_list.current());
        });
    }

    let add_stream = {
        let mut next_id = next_id;
        let mut stream_ids = stream_ids;
        move |_| {
            let id = *next_id.peek();
            next_id.set(id + 1);
            stream_ids.write().push(id);
        }
    };

    let refresh_devices = {
        let device_list = device_list.clone();
        let mut devices = devices;
        move |_| {
            device_list.refresh();
            devices.set(device_list.current());
        }
    };

    rsx! {
        style { {APP_CSS} }
        div { class: "app",
            header { class: "title-bar",
                h1 { "chimeras" }
                span { class: "subtitle", "cross-platform camera demo" }
                button {
                    class: "btn btn-ghost",
                    onclick: refresh_devices,
                    "Refresh cameras"
                }
                button {
                    class: "btn btn-primary add-stream-btn",
                    onclick: add_stream,
                    "+ Add stream"
                }
            }

            section { class: "stream-grid",
                for id in stream_ids() {
                    StreamCell { key: "{id}", id, stream_ids, devices }
                }
            }
        }
        script { dangerous_inner_html: "{PREVIEW_JS}" }
    }
}

#[component]
fn StreamCell(id: u32, stream_ids: Signal<Vec<u32>>, devices: Signal<Vec<Device>>) -> Element {
    let source_mode = use_signal(|| SourceMode::Rtsp);
    let selected_device = use_signal(|| 0usize);
    let url = use_signal(|| "rtsp://127.0.0.1:8554/live".to_string());
    let username = use_signal(String::new);
    let password = use_signal(String::new);
    let status = use_signal(|| "Idle".to_string());
    let session: Signal<Option<Session>> = use_signal(|| None);

    let registry = use_context::<Registry>();
    let port = use_context::<PreviewPort>().0;

    let preview_url = format!("http://127.0.0.1:{port}/preview/{id}.bin");

    let connect_channel = use_hook(|| {
        let (tx, rx) = std::sync::mpsc::channel::<ConnectOutcome>();
        (tx, Arc::new(Mutex::new(rx)))
    });
    let connect_tx = connect_channel.0.clone();
    let connect_rx = Arc::clone(&connect_channel.1);

    {
        let mut session_signal = session;
        let mut status_signal = status;
        use_future(move || {
            let connect_rx = Arc::clone(&connect_rx);
            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let mut outcomes = Vec::new();
                    if let Ok(rx) = connect_rx.lock() {
                        while let Ok(outcome) = rx.try_recv() {
                            outcomes.push(outcome);
                        }
                    }
                    for outcome in outcomes {
                        match outcome {
                            ConnectOutcome::Connected {
                                session: new_session,
                                label,
                            } => {
                                session_signal.set(Some(new_session));
                                status_signal.set(format!("Streaming: {label}"));
                            }
                            ConnectOutcome::Failed(message) => status_signal.set(message),
                        }
                    }
                }
            }
        });
    }

    enum OpenRequest {
        Usb(Device),
        Rtsp {
            url: String,
            credentials: Option<Credentials>,
        },
    }

    let connect = {
        let registry = registry.clone();
        let connect_tx = connect_tx.clone();
        move |_| {
            let mode = *source_mode.peek();
            let (request, label) = match mode {
                SourceMode::Usb => {
                    let device_list = devices.peek();
                    let index = *selected_device.peek();
                    let Some(device) = device_list.get(index).cloned() else {
                        status.clone().set("No camera selected".into());
                        return;
                    };
                    let label = device.name.clone();
                    (OpenRequest::Usb(device), label)
                }
                SourceMode::Rtsp => {
                    let trimmed = url.peek().trim().to_string();
                    if trimmed.is_empty() {
                        status.clone().set("RTSP URL is empty".into());
                        return;
                    }
                    let user = username.peek().trim().to_string();
                    let pass = password.peek().to_string();
                    let credentials = if user.is_empty() && pass.is_empty() {
                        None
                    } else {
                        Some(Credentials {
                            username: user,
                            password: pass,
                        })
                    };
                    let label = trimmed.clone();
                    (
                        OpenRequest::Rtsp {
                            url: trimmed,
                            credentials,
                        },
                        label,
                    )
                }
            };
            session.clone().set(None);
            status.clone().set(format!("Connecting to {label}..."));

            let registry = registry.clone();
            let connect_tx = connect_tx.clone();
            let config = StreamConfig {
                resolution: Resolution {
                    width: 1280,
                    height: 720,
                },
                framerate: 30,
                pixel_format: PixelFormat::Bgra8,
            };
            let _ = std::thread::Builder::new()
                .name("demo-connect".into())
                .spawn(move || {
                    let open_result = match request {
                        OpenRequest::Usb(device) => chimeras::open(&device, config),
                        OpenRequest::Rtsp { url, credentials } => {
                            chimeras::open_rtsp(&url, credentials, config)
                        }
                    };
                    let outcome = match open_result {
                        Ok(camera) => {
                            let latest_for_pump = registry.get_or_create(id);
                            let shutdown = Arc::new(AtomicBool::new(false));
                            let shutdown_for_pump = Arc::clone(&shutdown);
                            let pump = std::thread::Builder::new()
                                .name(format!("demo-pump-{id}"))
                                .spawn(move || {
                                    let camera = camera;
                                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                        || {
                                            while !shutdown_for_pump.load(Ordering::Relaxed) {
                                                match chimeras::next_frame(
                                                    &camera,
                                                    Duration::from_millis(500),
                                                ) {
                                                    Ok(frame) => latest_for_pump.set(frame),
                                                    Err(chimeras::Error::Timeout) => continue,
                                                    Err(_) => break,
                                                }
                                            }
                                        },
                                    ));
                                })
                                .expect("failed to spawn camera pump thread");
                            ConnectOutcome::Connected {
                                session: Session {
                                    pump: Some(pump),
                                    shutdown,
                                },
                                label,
                            }
                        }
                        Err(error) => ConnectOutcome::Failed(format!("Open failed: {error}")),
                    };
                    let _ = connect_tx.send(outcome);
                });
        }
    };

    let disconnect = {
        move |_| {
            session.clone().set(None);
            status.clone().set("Disconnected".into());
        }
    };

    let remove = {
        let registry = registry.clone();
        let mut stream_ids = stream_ids;
        move |_| {
            session.clone().set(None);
            registry.remove(id);
            stream_ids.write().retain(|other| *other != id);
        }
    };

    let is_connected = session.peek().is_some();
    let connect_label = if is_connected { "Reconnect" } else { "Connect" };
    let mode = source_mode();
    let is_usb = mode == SourceMode::Usb;
    let is_rtsp = mode == SourceMode::Rtsp;
    let device_count = devices().len();
    let connect_enabled = match mode {
        SourceMode::Usb => device_count > 0,
        SourceMode::Rtsp => !url().trim().is_empty(),
    };

    rsx! {
        div { class: "stream-cell",
            div { class: "stream-cell-header",
                span { class: "stream-cell-title", "Stream {id}" }
                div { class: "mode-toggle",
                    button {
                        class: if is_usb { "mode-btn mode-btn-active" } else { "mode-btn" },
                        onclick: move |_| source_mode.clone().set(SourceMode::Usb),
                        "USB"
                    }
                    button {
                        class: if is_rtsp { "mode-btn mode-btn-active" } else { "mode-btn" },
                        onclick: move |_| source_mode.clone().set(SourceMode::Rtsp),
                        "RTSP"
                    }
                }
                button {
                    class: "btn btn-ghost stream-cell-remove",
                    onclick: remove,
                    "Remove"
                }
            }
            div { class: "stream-cell-inputs",
                if is_usb {
                    select {
                        class: "input",
                        disabled: device_count == 0,
                        onchange: move |event| {
                            if let Ok(index) = event.value().parse::<usize>() {
                                selected_device.clone().set(index);
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
                } else {
                    input {
                        class: "input",
                        r#type: "text",
                        placeholder: "rtsp://host:port/path",
                        value: "{url()}",
                        oninput: move |event| url.clone().set(event.value()),
                    }
                    input {
                        class: "input input-narrow",
                        r#type: "text",
                        placeholder: "Username",
                        value: "{username()}",
                        oninput: move |event| username.clone().set(event.value()),
                    }
                    input {
                        class: "input input-narrow",
                        r#type: "password",
                        placeholder: "Password",
                        value: "{password()}",
                        oninput: move |event| password.clone().set(event.value()),
                    }
                }
            }
            div { class: "stream-cell-actions",
                button {
                    class: "btn btn-primary",
                    disabled: !connect_enabled,
                    onclick: connect,
                    "{connect_label}"
                }
                if is_connected {
                    button {
                        class: "btn btn-ghost",
                        onclick: disconnect,
                        "Disconnect"
                    }
                }
                span {
                    class: "status-dot",
                    "data-state": if is_connected { "live" } else { "idle" },
                }
                span { class: "stream-cell-status", "{status()}" }
            }
            div { class: "stream-cell-preview",
                canvas {
                    id: "chimeras-preview-{id}",
                    "data-stream-id": "{id}",
                    "data-preview-url": "{preview_url}",
                    class: "preview-canvas",
                }
            }
        }
    }
}
