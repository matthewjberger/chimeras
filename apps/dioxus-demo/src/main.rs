use cameras::analysis;
use cameras::{CameraSource, Credentials, PixelFormat, Resolution, StreamConfig};
use dioxus::prelude::*;
use dioxus_cameras::{
    PreviewScript, StreamPreview, StreamStatus, UseDevices, UseStreams, register_with,
    start_preview_server, use_camera_stream, use_devices, use_streams,
};

const APP_CSS: &str = include_str!("../assets/app.css");

const STREAM_CONFIG: StreamConfig = StreamConfig {
    resolution: Resolution {
        width: 1280,
        height: 720,
    },
    framerate: 30,
    pixel_format: PixelFormat::Bgra8,
};

fn main() {
    let server = start_preview_server().expect("failed to start preview server");

    let launch = dioxus::LaunchBuilder::desktop().with_cfg(
        dioxus::desktop::Config::new().with_menu(None).with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title("cameras demo")
                .with_inner_size(dioxus::desktop::LogicalSize::new(1400.0, 900.0)),
        ),
    );

    register_with(&server, launch).launch(App);
}

#[derive(Clone, Copy, PartialEq)]
enum SourceMode {
    Usb,
    Rtsp,
}

#[component]
fn App() -> Element {
    let streams = use_streams();
    let devices = use_devices();
    let ids = streams.ids;

    rsx! {
        style { {APP_CSS} }
        div { class: "app",
            header { class: "title-bar",
                h1 { "cameras" }
                span { class: "subtitle", "cross-platform camera demo" }
                button {
                    class: "btn btn-ghost",
                    onclick: move |_| devices.refresh.call(()),
                    "Refresh cameras"
                }
                button {
                    class: "btn btn-primary add-stream-btn",
                    onclick: move |_| { streams.add.call(()); },
                    "+ Add stream"
                }
            }

            section { class: "stream-grid",
                for id in ids() {
                    StreamCell { key: "{id}", id, streams, devices }
                }
            }
        }
        PreviewScript {}
    }
}

#[component]
fn StreamCell(id: u32, streams: UseStreams, devices: UseDevices) -> Element {
    let source_mode = use_signal(|| SourceMode::Rtsp);
    let selected_device = use_signal(|| 0usize);
    let url = use_signal(|| "rtsp://127.0.0.1:8554/live".to_string());
    let username = use_signal(String::new);
    let password = use_signal(String::new);
    let source: Signal<Option<CameraSource>> = use_signal(|| None);
    let last_capture = use_signal(|| String::from("-"));

    let stream = use_camera_stream(id, source, STREAM_CONFIG);

    let connect = move |_| {
        let next = match *source_mode.peek() {
            SourceMode::Usb => {
                let list = devices.devices.peek();
                let index = *selected_device.peek();
                list.get(index).cloned().map(CameraSource::Usb)
            }
            SourceMode::Rtsp => build_rtsp_source(&url.peek(), &username.peek(), &password.peek()),
        };
        source.clone().set(next);
    };

    let disconnect = move |_| source.clone().set(None);

    let remove = move |_| {
        source.clone().set(None);
        streams.remove.call(id);
    };

    let toggle_preview = move |_| {
        let now = *stream.active.peek();
        stream.active.clone().set(!now);
    };

    let take_picture = move |_| match stream.capture_frame.call(()) {
        Some(frame) => {
            let variance = analysis::blur_variance(&frame);
            last_capture.clone().set(format!(
                "Captured {}x{} (sharpness {variance:.1})",
                frame.width, frame.height
            ));
        }
        None => {
            last_capture.clone().set("Capture failed".into());
        }
    };

    let analyze_sharpness = move |_| match stream.capture_frame.call(()) {
        Some(frame) => {
            let variance = analysis::blur_variance(&frame);
            last_capture.clone().set(format!(
                "Sharpness {variance:.1} (relative, calibrate per source)"
            ));
        }
        None => {
            last_capture.clone().set("Capture failed".into());
        }
    };

    let status_value = stream.status.read().clone();
    let is_streaming = matches!(status_value, StreamStatus::Streaming { .. });
    let connect_label = if is_streaming { "Reconnect" } else { "Connect" };
    let mode = source_mode();
    let is_usb = mode == SourceMode::Usb;
    let is_rtsp = mode == SourceMode::Rtsp;
    let device_count = devices.devices.read().len();
    let devices_ready = *devices.ready.read();
    let connect_enabled = match mode {
        SourceMode::Usb => device_count > 0,
        SourceMode::Rtsp => !url().trim().is_empty(),
    };
    let show_disconnect = !matches!(status_value, StreamStatus::Idle);
    let preview_on = *stream.active.read();

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
                        if !devices_ready {
                            option { "Scanning for cameras..." }
                        } else if device_count == 0 {
                            option { "No cameras detected" }
                        } else {
                            for (index, device) in devices.devices.read().iter().enumerate() {
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
                if show_disconnect {
                    button {
                        class: "btn btn-ghost",
                        onclick: disconnect,
                        "Disconnect"
                    }
                }
                span {
                    class: "status-dot",
                    "data-state": if is_streaming { "live" } else { "idle" },
                }
                span { class: "stream-cell-status", "{status_value}" }
            }
            if is_streaming {
                div { class: "stream-cell-actions",
                    button {
                        class: if preview_on { "btn btn-accent" } else { "btn btn-ghost" },
                        onclick: toggle_preview,
                        if preview_on { "Preview: on" } else { "Preview: off" }
                    }
                    button {
                        class: "btn btn-primary",
                        onclick: take_picture,
                        "Take picture"
                    }
                    button {
                        class: "btn btn-ghost",
                        onclick: analyze_sharpness,
                        "Analyze sharpness"
                    }
                    span { class: "stream-cell-status", "Last capture: {last_capture}" }
                }
            }
            div { class: "stream-cell-preview",
                StreamPreview { id }
            }
        }
    }
}

fn build_rtsp_source(url: &str, username: &str, password: &str) -> Option<CameraSource> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let username = username.trim();
    let credentials = if username.is_empty() && password.is_empty() {
        None
    } else {
        Some(Credentials {
            username: username.to_string(),
            password: password.to_string(),
        })
    };
    Some(CameraSource::Rtsp {
        url: url.to_string(),
        credentials,
    })
}
