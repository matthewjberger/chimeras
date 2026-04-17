use std::sync::mpsc;
use std::time::Duration;

use cameras::analysis;
use cameras::{
    CameraSource, ControlCapabilities, ControlKind, ControlRange, Controls, Credentials, Device,
    PixelFormat, Rect, Resolution, StreamConfig,
};
use dioxus::prelude::*;
use dioxus_cameras::{
    PreviewScript, StreamPreview, StreamStatus, UseDevices, UseStreams, register_with,
    start_preview_server, use_camera_stream, use_devices, use_streams,
};
use futures_timer::Delay;

const AUTOFOCUS_SETTLE: Duration = Duration::from_millis(150);
const AUTOFOCUS_SWEEP_STRIDE_MULTIPLIER: f32 = 4.0;
const AUTOFOCUS_CONTINUOUS_SAMPLES: f32 = 20.0;

fn auto_lock_hover(paired_label: &str) -> String {
    format!(
        "Disabled because {paired_label} is set to 'auto'. Change {paired_label} to 'leave' or \
         'manual' to enable this slider."
    )
}

type ApplyRequest = (Device, Controls);

fn spawn_apply_worker() -> mpsc::Sender<ApplyRequest> {
    let (tx, rx) = mpsc::channel::<ApplyRequest>();
    std::thread::Builder::new()
        .name("dioxus-demo-controls".into())
        .spawn(move || worker_loop(rx))
        .expect("spawn controls worker");
    tx
}

fn worker_loop(rx: mpsc::Receiver<ApplyRequest>) {
    while let Ok(mut latest) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }
        let (device, controls) = latest;
        let _ = cameras::apply_controls(&device, &controls);
    }
}

#[derive(Clone)]
struct ApplySender(mpsc::Sender<ApplyRequest>);

impl ApplySender {
    fn send(&self, device: Device, controls: Controls) {
        let _ = self.0.send((device, controls));
    }
}

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
    let capabilities: Signal<Option<ControlCapabilities>> = use_signal(|| None);
    let capabilities_error: Signal<Option<String>> = use_signal(|| None);
    let pending_controls: Signal<Controls> = use_signal(Controls::default);
    let current_controls: Signal<Controls> = use_signal(Controls::default);
    let connected_device: Signal<Option<Device>> = use_signal(|| None);
    let live_apply: Signal<bool> = use_signal(|| false);
    let apply_sender: Signal<ApplySender> =
        use_hook(|| Signal::new(ApplySender(spawn_apply_worker())));
    let autofocus_progress: Signal<Option<(u32, u32)>> = use_signal(|| None);

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
        if let Some(CameraSource::Usb(device)) = next.as_ref() {
            match cameras::control_capabilities(device) {
                Ok(caps) => {
                    let current = cameras::read_controls(device).unwrap_or_default();
                    capabilities.clone().set(Some(caps));
                    capabilities_error.clone().set(None);
                    current_controls.clone().set(current);
                    pending_controls.clone().set(Controls::default());
                    connected_device.clone().set(Some(device.clone()));
                }
                Err(error) => {
                    capabilities.clone().set(None);
                    capabilities_error.clone().set(Some(error.to_string()));
                    current_controls.clone().set(Controls::default());
                    pending_controls.clone().set(Controls::default());
                    connected_device.clone().set(None);
                }
            }
        } else {
            capabilities.clone().set(None);
            capabilities_error.clone().set(None);
            current_controls.clone().set(Controls::default());
            pending_controls.clone().set(Controls::default());
            connected_device.clone().set(None);
        }
        source.clone().set(next);
    };

    let apply_controls = move |_| {
        let Some(device) = connected_device.peek().clone() else {
            last_capture.clone().set("No USB device".into());
            return;
        };
        let controls = pending_controls.peek().clone();
        apply_sender.peek().send(device, controls);
        last_capture.clone().set("Queued controls apply".into());
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

    let start_autofocus = move |_| {
        if autofocus_progress.peek().is_some() {
            return;
        }
        let Some(device) = connected_device.peek().clone() else {
            return;
        };
        let Some(caps) = capabilities.peek().clone() else {
            return;
        };
        let Some(focus_range) = caps.focus else {
            last_capture
                .clone()
                .set("Camera has no controllable focus".into());
            return;
        };
        if let Err(error) = cameras::apply_controls(
            &device,
            &Controls {
                auto_focus: Some(false),
                auto_exposure: Some(false),
                auto_white_balance: Some(false),
                ..Default::default()
            },
        ) {
            last_capture
                .clone()
                .set(format!("Autofocus setup failed: {error}"));
            return;
        }
        let step = if focus_range.step > 0.0 {
            focus_range.step * AUTOFOCUS_SWEEP_STRIDE_MULTIPLIER
        } else {
            ((focus_range.max - focus_range.min) / AUTOFOCUS_CONTINUOUS_SAMPLES).max(f32::EPSILON)
        };
        let span = (focus_range.max - focus_range.min).max(0.0);
        let total = (span / step).floor() as u32 + 1;

        autofocus_progress.clone().set(Some((0, total)));
        let capture = stream.capture_frame;
        spawn(async move {
            let mut best: Option<(f32, f32)> = None;
            for index in 0..total {
                let focus_value = (focus_range.min + index as f32 * step).min(focus_range.max);
                if cameras::apply_controls(
                    &device,
                    &Controls {
                        focus: Some(focus_value),
                        ..Default::default()
                    },
                )
                .is_err()
                {
                    last_capture.clone().set("Autofocus: apply failed".into());
                    autofocus_progress.clone().set(None);
                    return;
                }
                Delay::new(AUTOFOCUS_SETTLE).await;
                if let Some(frame) = capture.call(()) {
                    let roi = Rect {
                        x: frame.width / 4,
                        y: frame.height / 4,
                        width: frame.width / 2,
                        height: frame.height / 2,
                    };
                    let variance = analysis::blur_variance_in(&frame, roi);
                    if best.is_none_or(|(_, score)| variance > score) {
                        best = Some((focus_value, variance));
                    }
                }
                autofocus_progress.clone().set(Some((index + 1, total)));
            }
            match best {
                Some((winner, score)) => {
                    let _ = cameras::apply_controls(
                        &device,
                        &Controls {
                            focus: Some(winner),
                            ..Default::default()
                        },
                    );
                    let mut controls = pending_controls.peek().clone();
                    controls.focus = Some(winner);
                    controls.auto_focus = Some(false);
                    pending_controls.clone().set(controls);
                    last_capture.clone().set(format!(
                        "Autofocus locked at {winner:.2} (variance {score:.1})"
                    ));
                }
                None => {
                    last_capture
                        .clone()
                        .set("Autofocus: no samples captured".into());
                }
            }
            autofocus_progress.clone().set(None);
        });
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
                    {
                        let has_focus = capabilities
                            .read()
                            .as_ref()
                            .and_then(|caps| caps.focus.as_ref())
                            .is_some();
                        let progress_now = *autofocus_progress.read();
                        if has_focus {
                            let (label, disabled) = match progress_now {
                                Some((current, total)) => {
                                    (format!("Autofocusing {current}/{total}"), true)
                                }
                                None => ("Autofocus (sweep)".to_string(), false),
                            };
                            rsx! {
                                button {
                                    class: "btn btn-ghost",
                                    disabled,
                                    onclick: start_autofocus,
                                    "{label}"
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                    span { class: "stream-cell-status", "Last capture: {last_capture}" }
                }
                {
                    let caps_read = capabilities.read();
                    let err_read = capabilities_error.read();
                    render_capabilities_block(caps_read.as_ref(), err_read.as_deref())
                }
                {
                    let caps_read = capabilities.read();
                    if let Some(caps) = caps_read.as_ref() {
                        render_controls_block(
                            caps,
                            current_controls,
                            pending_controls,
                            connected_device,
                            apply_sender,
                            live_apply,
                            apply_controls,
                        )
                    } else {
                        rsx! {}
                    }
                }
            }
            div { class: "stream-cell-preview",
                StreamPreview { id }
            }
        }
    }
}

fn render_capabilities_block(
    capabilities: Option<&ControlCapabilities>,
    error: Option<&str>,
) -> Element {
    if let Some(message) = error {
        return rsx! {
            details { class: "capabilities-details",
                summary { class: "capabilities-summary", "Capabilities" }
                div { class: "stream-cell-capabilities",
                    span { class: "capability-error", "{message}" }
                }
            }
        };
    }
    let Some(caps) = capabilities else {
        return rsx! {};
    };
    let rows: Vec<(ControlKind, bool, String)> = vec![
        capability_row(ControlKind::Focus, range_row(caps.focus.as_ref())),
        capability_row(ControlKind::AutoFocus, bool_row(caps.auto_focus)),
        capability_row(ControlKind::Exposure, range_row(caps.exposure.as_ref())),
        capability_row(ControlKind::AutoExposure, bool_row(caps.auto_exposure)),
        capability_row(
            ControlKind::WhiteBalanceTemperature,
            range_row(caps.white_balance_temperature.as_ref()),
        ),
        capability_row(
            ControlKind::AutoWhiteBalance,
            bool_row(caps.auto_white_balance),
        ),
        capability_row(ControlKind::Brightness, range_row(caps.brightness.as_ref())),
        capability_row(ControlKind::Contrast, range_row(caps.contrast.as_ref())),
        capability_row(ControlKind::Saturation, range_row(caps.saturation.as_ref())),
        capability_row(ControlKind::Sharpness, range_row(caps.sharpness.as_ref())),
        capability_row(ControlKind::Gain, range_row(caps.gain.as_ref())),
        capability_row(
            ControlKind::BacklightCompensation,
            range_row(caps.backlight_compensation.as_ref()),
        ),
        capability_row(
            ControlKind::PowerLineFrequency,
            (caps.power_line_frequency.is_some(), "menu supported".into()),
        ),
        capability_row(ControlKind::Pan, range_row(caps.pan.as_ref())),
        capability_row(ControlKind::Tilt, range_row(caps.tilt.as_ref())),
        capability_row(ControlKind::Zoom, range_row(caps.zoom.as_ref())),
    ];
    rsx! {
        details { class: "capabilities-details",
            summary { class: "capabilities-summary", "Capabilities" }
            div { class: "stream-cell-capabilities",
                for (kind, supported, detail) in rows {
                    {
                        let tooltip = if supported {
                            String::new()
                        } else {
                            kind.caveat().unwrap_or("").to_string()
                        };
                        let name = kind.label();
                        rsx! {
                            div {
                                class: "capability-row",
                                title: "{tooltip}",
                                span {
                                    class: if supported { "capability-mark supported" } else { "capability-mark unsupported" },
                                    if supported { "✓" } else { "×" }
                                }
                                span { class: "capability-name", "{name}" }
                                span { class: "capability-detail", "{detail}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_controls_block(
    capabilities: &ControlCapabilities,
    current: Signal<Controls>,
    pending: Signal<Controls>,
    device: Signal<Option<Device>>,
    apply_sender: Signal<ApplySender>,
    live_apply: Signal<bool>,
    apply_handler: impl FnMut(dioxus::prelude::Event<dioxus::events::MouseData>) + 'static,
) -> Element {
    let numeric_rows = [
        (
            "focus",
            capabilities.focus.as_ref(),
            ControlField::Focus,
            Some(AutoMode::Focus),
            Some("auto_focus"),
        ),
        (
            "exposure",
            capabilities.exposure.as_ref(),
            ControlField::Exposure,
            Some(AutoMode::Exposure),
            Some("auto_exposure"),
        ),
        (
            "white_balance_temperature",
            capabilities.white_balance_temperature.as_ref(),
            ControlField::WhiteBalanceTemperature,
            Some(AutoMode::WhiteBalance),
            Some("auto_white_balance"),
        ),
        (
            "brightness",
            capabilities.brightness.as_ref(),
            ControlField::Brightness,
            None,
            None,
        ),
        (
            "contrast",
            capabilities.contrast.as_ref(),
            ControlField::Contrast,
            None,
            None,
        ),
        (
            "saturation",
            capabilities.saturation.as_ref(),
            ControlField::Saturation,
            None,
            None,
        ),
        (
            "sharpness",
            capabilities.sharpness.as_ref(),
            ControlField::Sharpness,
            None,
            None,
        ),
        (
            "gain",
            capabilities.gain.as_ref(),
            ControlField::Gain,
            None,
            None,
        ),
        (
            "backlight_compensation",
            capabilities.backlight_compensation.as_ref(),
            ControlField::BacklightCompensation,
            None,
            None,
        ),
        (
            "pan",
            capabilities.pan.as_ref(),
            ControlField::Pan,
            None,
            None,
        ),
        (
            "tilt",
            capabilities.tilt.as_ref(),
            ControlField::Tilt,
            None,
            None,
        ),
        (
            "zoom",
            capabilities.zoom.as_ref(),
            ControlField::Zoom,
            None,
            None,
        ),
    ];
    let auto_rows = [
        ("auto_focus", capabilities.auto_focus, AutoMode::Focus),
        (
            "auto_exposure",
            capabilities.auto_exposure,
            AutoMode::Exposure,
        ),
        (
            "auto_white_balance",
            capabilities.auto_white_balance,
            AutoMode::WhiteBalance,
        ),
    ];

    let live_on = *live_apply.read();
    rsx! {
        div { class: "stream-cell-controls",
            for (name, range, field, paired_auto, paired_auto_label) in numeric_rows {
                if let Some(range) = range {
                    NumericControlRow {
                        label: name,
                        range: *range,
                        field,
                        paired_auto,
                        paired_auto_label,
                        current,
                        pending,
                        device,
                        apply_sender,
                        live_apply,
                    }
                }
            }
            for (name, capability, field) in auto_rows {
                if capability == Some(true) {
                    AutoTriStateRow {
                        label: name,
                        field,
                        pending,
                        device,
                        apply_sender,
                        live_apply,
                    }
                }
            }
            div { class: "control-row",
                span { class: "control-label", "live apply" }
                label { class: "control-toggle",
                    input {
                        r#type: "checkbox",
                        checked: live_on,
                        onchange: move |event| {
                            live_apply.clone().set(event.checked());
                        },
                    }
                    span {
                        if live_on { "on — changes apply immediately" } else { "off — click Apply" }
                    }
                }
            }
            if !live_on {
                button {
                    class: "btn btn-primary",
                    onclick: apply_handler,
                    "Apply controls"
                }
            }
        }
    }
}

fn live_apply_if_enabled(
    live_apply: Signal<bool>,
    device: Signal<Option<Device>>,
    apply_sender: Signal<ApplySender>,
    controls: Controls,
) {
    if !*live_apply.peek() {
        return;
    }
    let Some(device) = device.peek().clone() else {
        return;
    };
    apply_sender.peek().send(device, controls);
}

#[derive(Copy, Clone, PartialEq)]
enum ControlField {
    Focus,
    Exposure,
    WhiteBalanceTemperature,
    Brightness,
    Contrast,
    Saturation,
    Sharpness,
    Gain,
    BacklightCompensation,
    Pan,
    Tilt,
    Zoom,
}

#[derive(Copy, Clone, PartialEq)]
enum AutoMode {
    Focus,
    Exposure,
    WhiteBalance,
}

fn controls_field_get(controls: &Controls, field: ControlField) -> Option<f32> {
    match field {
        ControlField::Focus => controls.focus,
        ControlField::Exposure => controls.exposure,
        ControlField::WhiteBalanceTemperature => controls.white_balance_temperature,
        ControlField::Brightness => controls.brightness,
        ControlField::Contrast => controls.contrast,
        ControlField::Saturation => controls.saturation,
        ControlField::Sharpness => controls.sharpness,
        ControlField::Gain => controls.gain,
        ControlField::BacklightCompensation => controls.backlight_compensation,
        ControlField::Pan => controls.pan,
        ControlField::Tilt => controls.tilt,
        ControlField::Zoom => controls.zoom,
    }
}

fn controls_field_set(controls: &mut Controls, field: ControlField, value: Option<f32>) {
    match field {
        ControlField::Focus => controls.focus = value,
        ControlField::Exposure => controls.exposure = value,
        ControlField::WhiteBalanceTemperature => controls.white_balance_temperature = value,
        ControlField::Brightness => controls.brightness = value,
        ControlField::Contrast => controls.contrast = value,
        ControlField::Saturation => controls.saturation = value,
        ControlField::Sharpness => controls.sharpness = value,
        ControlField::Gain => controls.gain = value,
        ControlField::BacklightCompensation => controls.backlight_compensation = value,
        ControlField::Pan => controls.pan = value,
        ControlField::Tilt => controls.tilt = value,
        ControlField::Zoom => controls.zoom = value,
    }
}

fn bool_field_get(controls: &Controls, field: AutoMode) -> Option<bool> {
    match field {
        AutoMode::Focus => controls.auto_focus,
        AutoMode::Exposure => controls.auto_exposure,
        AutoMode::WhiteBalance => controls.auto_white_balance,
    }
}

fn bool_field_set(controls: &mut Controls, field: AutoMode, value: Option<bool>) {
    match field {
        AutoMode::Focus => controls.auto_focus = value,
        AutoMode::Exposure => controls.auto_exposure = value,
        AutoMode::WhiteBalance => controls.auto_white_balance = value,
    }
}

#[component]
fn NumericControlRow(
    label: &'static str,
    range: ControlRange,
    field: ControlField,
    paired_auto: Option<AutoMode>,
    paired_auto_label: Option<&'static str>,
    current: Signal<Controls>,
    pending: Signal<Controls>,
    device: Signal<Option<Device>>,
    apply_sender: Signal<ApplySender>,
    live_apply: Signal<bool>,
) -> Element {
    let current_value = controls_field_get(&current.read(), field);
    let pending_value = controls_field_get(&pending.read(), field);
    let locked_by_auto = paired_auto
        .map(|mode| bool_field_get(&pending.read(), mode) == Some(true))
        .unwrap_or(false);
    let enabled = pending_value.is_some();
    let effective_value = pending_value.or(current_value).unwrap_or(range.default);
    let step_attr = if range.step > 0.0 {
        format!("{}", range.step)
    } else {
        "any".to_string()
    };
    let fallback_value = current_value.unwrap_or(range.default);
    let status_text = if locked_by_auto {
        "auto on".to_string()
    } else if let Some(current_value) = current_value {
        format!("now {current_value:.2}")
    } else {
        String::new()
    };
    let slider_disabled = !enabled || locked_by_auto;
    let checkbox_disabled = locked_by_auto;
    let lock_tooltip = if locked_by_auto {
        paired_auto_label.map(auto_lock_hover).unwrap_or_default()
    } else {
        String::new()
    };
    rsx! {
        div { class: "control-row",
            span { class: "control-label", "{label}" }
            div {
                class: "control-toggle",
                title: "{lock_tooltip}",
                input {
                    r#type: "checkbox",
                    checked: enabled,
                    disabled: checkbox_disabled,
                    onchange: move |event| {
                        let mut controls = pending.peek().clone();
                        if event.checked() {
                            controls_field_set(&mut controls, field, Some(fallback_value));
                        } else {
                            controls_field_set(&mut controls, field, None);
                        }
                        pending.clone().set(controls.clone());
                        live_apply_if_enabled(live_apply, device, apply_sender, controls);
                    },
                }
                input {
                    class: "control-slider",
                    r#type: "range",
                    min: "{range.min}",
                    max: "{range.max}",
                    step: "{step_attr}",
                    value: "{effective_value}",
                    disabled: slider_disabled,
                    oninput: move |event| {
                        if let Ok(parsed) = event.value().parse::<f32>() {
                            let mut controls = pending.peek().clone();
                            controls_field_set(&mut controls, field, Some(parsed));
                            pending.clone().set(controls.clone());
                            live_apply_if_enabled(live_apply, device, apply_sender, controls);
                        }
                    },
                }
            }
            span { class: "control-value", "{status_text}" }
        }
    }
}

#[component]
fn AutoTriStateRow(
    label: &'static str,
    field: AutoMode,
    pending: Signal<Controls>,
    device: Signal<Option<Device>>,
    apply_sender: Signal<ApplySender>,
    live_apply: Signal<bool>,
) -> Element {
    let current = bool_field_get(&pending.read(), field);
    let radio_name = format!("auto-{label}");
    rsx! {
        div { class: "control-row",
            span { class: "control-label", "{label}" }
            div { class: "control-tri-state",
                label { class: "control-tri-option",
                    input {
                        r#type: "radio",
                        name: "{radio_name}",
                        checked: current.is_none(),
                        onchange: move |_| {
                            let mut controls = pending.peek().clone();
                            bool_field_set(&mut controls, field, None);
                            pending.clone().set(controls.clone());
                            live_apply_if_enabled(live_apply, device, apply_sender, controls);
                        },
                    }
                    span { "leave" }
                }
                label { class: "control-tri-option",
                    input {
                        r#type: "radio",
                        name: "{radio_name}",
                        checked: current == Some(false),
                        onchange: move |_| {
                            let mut controls = pending.peek().clone();
                            bool_field_set(&mut controls, field, Some(false));
                            pending.clone().set(controls.clone());
                            live_apply_if_enabled(live_apply, device, apply_sender, controls);
                        },
                    }
                    span { "manual" }
                }
                label { class: "control-tri-option",
                    input {
                        r#type: "radio",
                        name: "{radio_name}",
                        checked: current == Some(true),
                        onchange: move |_| {
                            let mut controls = pending.peek().clone();
                            bool_field_set(&mut controls, field, Some(true));
                            pending.clone().set(controls.clone());
                            live_apply_if_enabled(live_apply, device, apply_sender, controls);
                        },
                    }
                    span { "auto" }
                }
            }
        }
    }
}

fn capability_row(kind: ControlKind, detail: (bool, String)) -> (ControlKind, bool, String) {
    let (supported, text) = detail;
    (kind, supported, text)
}

fn range_row(range: Option<&ControlRange>) -> (bool, String) {
    match range {
        Some(range) => {
            let text = if range.step > 0.0 {
                format!(
                    "{:.0}..{:.0} (step {:.0}, default {:.0})",
                    range.min, range.max, range.step, range.default
                )
            } else {
                format!(
                    "{:.2}..{:.2} (default {:.2})",
                    range.min, range.max, range.default
                )
            };
            (true, text)
        }
        None => (false, "not supported".into()),
    }
}

fn bool_row(value: Option<bool>) -> (bool, String) {
    match value {
        Some(true) => (true, "auto toggle supported".into()),
        Some(false) => (true, "manual only".into()),
        None => (false, "not supported".into()),
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
