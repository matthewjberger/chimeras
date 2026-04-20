use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use cameras::Credentials;
use cameras::analysis;
use cameras::discover::DiscoverConfig;
use cameras::{
    CameraSource, ControlCapabilities, ControlKind, ControlRange, Controls, Device, PixelFormat,
    Rect, Resolution, StreamConfig,
};
use eframe::egui;
use egui_cameras::{
    DiscoverySession, capture_frame, poll_discovery, set_active, show_discovery, start_discovery,
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

type ApplyRequest = (Device, Controls);

struct AutofocusState {
    focus_min: f32,
    focus_max: f32,
    step: f32,
    total_samples: u32,
    sample_index: u32,
    best: Option<(f32, f32)>,
    phase: AutofocusPhase,
}

enum AutofocusPhase {
    Apply,
    Settle(Instant),
    Sample,
    Finalize,
}

impl AutofocusState {
    fn focus_at(&self, index: u32) -> f32 {
        (self.focus_min + index as f32 * self.step).min(self.focus_max)
    }
}

const SHARPEST_BURST_SIZE: usize = 16;
const SHARPEST_BURST_INTERVAL: Duration = Duration::from_millis(30);
const AUTOFOCUS_SETTLE: Duration = Duration::from_millis(150);
const AUTOFOCUS_SWEEP_STRIDE_MULTIPLIER: f32 = 4.0;
const AUTOFOCUS_CONTINUOUS_SAMPLES: f32 = 20.0;

const STREAM_CONFIG: StreamConfig = StreamConfig {
    resolution: Resolution {
        width: 1280,
        height: 720,
    },
    framerate: 30,
    pixel_format: PixelFormat::Bgra8,
};

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 820.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cameras egui demo",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum SourceMode {
    Usb,
    Rtsp,
}

struct App {
    devices: Vec<Device>,
    selected_device: usize,
    source_mode: SourceMode,
    rtsp_url: String,
    rtsp_username: String,
    rtsp_password: String,

    stream: Option<egui_cameras::Stream>,
    active: bool,
    status: String,
    last_capture: Option<String>,
    capabilities: Option<ControlCapabilities>,
    capabilities_error: Option<String>,
    pending_controls: Controls,
    current_controls: Controls,
    apply_tx: mpsc::Sender<ApplyRequest>,
    live_apply: bool,
    autofocus: Option<AutofocusState>,
    discover_open: bool,
    discover_subnet: String,
    discover_error: Option<String>,
    discovery: Option<DiscoverySession>,
}

impl App {
    fn new() -> Self {
        let devices = cameras::devices().unwrap_or_default();
        let apply_tx = spawn_apply_worker();
        Self {
            devices,
            selected_device: 0,
            source_mode: SourceMode::Usb,
            rtsp_url: "rtsp://127.0.0.1:8554/live".into(),
            rtsp_username: String::new(),
            rtsp_password: String::new(),
            stream: None,
            active: true,
            status: "Idle".into(),
            last_capture: None,
            capabilities: None,
            capabilities_error: None,
            pending_controls: Controls::default(),
            current_controls: Controls::default(),
            apply_tx,
            live_apply: false,
            autofocus: None,
            discover_open: false,
            discover_subnet: "192.168.1.0/24".into(),
            discover_error: None,
            discovery: None,
        }
    }

    fn render_discover(&mut self, ui: &mut egui::Ui) {
        let running = self
            .discovery
            .as_ref()
            .map(|session| !session.done)
            .unwrap_or(false);
        ui.horizontal(|ui| {
            ui.label("Targets");
            ui.add(
                egui::TextEdit::singleline(&mut self.discover_subnet)
                    .hint_text("CIDR or host:port, comma-separated")
                    .desired_width(280.0),
            );
            let start_enabled = !running;
            if ui
                .add_enabled(start_enabled, egui::Button::new("Scan"))
                .clicked()
            {
                self.start_discover();
            }
            if ui
                .add_enabled(running, egui::Button::new("Cancel"))
                .clicked()
            {
                self.discovery = None;
            }
            if let Some(message) = &self.discover_error {
                ui.colored_label(egui::Color32::from_rgb(200, 80, 80), message);
            }
        });
        let clicked = self
            .discovery
            .as_ref()
            .and_then(|session| show_discovery(session, ui));
        if let Some(camera) = clicked {
            self.open_discovered(camera);
        }
    }

    fn start_discover(&mut self) {
        let (subnets, endpoints) = match parse_targets(&self.discover_subnet) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.discover_error = Some(error);
                return;
            }
        };
        let config = DiscoverConfig {
            subnets,
            endpoints,
            ..Default::default()
        };
        match start_discovery(config) {
            Ok(session) => {
                self.discovery = Some(session);
                self.discover_error = None;
            }
            Err(error) => {
                self.discover_error = Some(format!("Start failed: {error}"));
            }
        }
    }

    fn open_discovered(&mut self, camera: cameras::discover::DiscoveredCamera) {
        let CameraSource::Rtsp { url, .. } = &camera.source else {
            return;
        };
        self.source_mode = SourceMode::Rtsp;
        self.rtsp_url = url.clone();
        self.rtsp_username.clear();
        self.rtsp_password.clear();
        self.connect();
    }

    fn queue_apply(&self) {
        let Some(device) = self.devices.get(self.selected_device) else {
            return;
        };
        let _ = self
            .apply_tx
            .send((device.clone(), self.pending_controls.clone()));
    }

    fn refresh_capabilities(&mut self) {
        self.capabilities = None;
        self.capabilities_error = None;
        self.pending_controls = Controls::default();
        self.current_controls = Controls::default();
        if self.source_mode != SourceMode::Usb {
            return;
        }
        let Some(device) = self.devices.get(self.selected_device) else {
            return;
        };
        match cameras::control_capabilities(device) {
            Ok(caps) => {
                self.current_controls = cameras::read_controls(device).unwrap_or_default();
                self.capabilities = Some(caps);
            }
            Err(error) => self.capabilities_error = Some(error.to_string()),
        }
    }

    fn start_autofocus(&mut self) {
        if self.autofocus.is_some() {
            return;
        }
        if self.stream.is_none() {
            return;
        }
        let Some(device) = self.devices.get(self.selected_device).cloned() else {
            return;
        };
        let Some(caps) = self.capabilities.clone() else {
            self.last_capture = Some("No capabilities".into());
            return;
        };
        let Some(focus_range) = caps.focus else {
            self.last_capture = Some("Camera has no controllable focus".into());
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
            self.last_capture = Some(format!("Autofocus setup failed: {error}"));
            return;
        }

        let step = if focus_range.step > 0.0 {
            focus_range.step * AUTOFOCUS_SWEEP_STRIDE_MULTIPLIER
        } else {
            ((focus_range.max - focus_range.min) / AUTOFOCUS_CONTINUOUS_SAMPLES).max(f32::EPSILON)
        };
        let span = (focus_range.max - focus_range.min).max(0.0);
        let total_samples = (span / step).floor() as u32 + 1;

        self.autofocus = Some(AutofocusState {
            focus_min: focus_range.min,
            focus_max: focus_range.max,
            step,
            total_samples,
            sample_index: 0,
            best: None,
            phase: AutofocusPhase::Apply,
        });
        self.last_capture = Some(format!("Autofocus: sweeping {total_samples} samples"));
    }

    fn tick_autofocus(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.autofocus.take() else {
            return;
        };
        let Some(stream) = self.stream.as_ref() else {
            self.autofocus = None;
            self.last_capture = Some("Autofocus cancelled (stream closed)".into());
            return;
        };
        let Some(device) = self.devices.get(self.selected_device).cloned() else {
            self.autofocus = None;
            return;
        };

        match state.phase {
            AutofocusPhase::Apply => {
                let focus_value = state.focus_at(state.sample_index);
                if cameras::apply_controls(
                    &device,
                    &Controls {
                        focus: Some(focus_value),
                        ..Default::default()
                    },
                )
                .is_err()
                {
                    self.last_capture = Some("Autofocus: apply failed".into());
                    self.autofocus = None;
                    return;
                }
                state.phase = AutofocusPhase::Settle(Instant::now());
                self.autofocus = Some(state);
                ctx.request_repaint_after(AUTOFOCUS_SETTLE);
            }
            AutofocusPhase::Settle(start) => {
                if start.elapsed() >= AUTOFOCUS_SETTLE {
                    state.phase = AutofocusPhase::Sample;
                }
                self.autofocus = Some(state);
                ctx.request_repaint_after(Duration::from_millis(16));
            }
            AutofocusPhase::Sample => {
                let focus_value = state.focus_at(state.sample_index);
                if let Some(frame) = capture_frame(&stream.pump) {
                    let roi = Rect {
                        x: frame.width / 4,
                        y: frame.height / 4,
                        width: frame.width / 2,
                        height: frame.height / 2,
                    };
                    let variance = analysis::blur_variance_in(&frame, roi);
                    if state.best.is_none_or(|(_, score)| variance > score) {
                        state.best = Some((focus_value, variance));
                    }
                }
                state.sample_index += 1;
                if state.sample_index >= state.total_samples {
                    state.phase = AutofocusPhase::Finalize;
                } else {
                    state.phase = AutofocusPhase::Apply;
                }
                self.autofocus = Some(state);
                ctx.request_repaint();
            }
            AutofocusPhase::Finalize => match state.best {
                Some((winner, score)) => {
                    let _ = cameras::apply_controls(
                        &device,
                        &Controls {
                            focus: Some(winner),
                            ..Default::default()
                        },
                    );
                    self.pending_controls.focus = Some(winner);
                    self.pending_controls.auto_focus = Some(false);
                    self.last_capture = Some(format!(
                        "Autofocus locked at {winner:.2} (variance {score:.1})"
                    ));
                    self.autofocus = None;
                }
                None => {
                    self.last_capture = Some("Autofocus: no samples captured".into());
                    self.autofocus = None;
                }
            },
        }
    }

    fn build_source(&self) -> Option<CameraSource> {
        match self.source_mode {
            SourceMode::Usb => self
                .devices
                .get(self.selected_device)
                .cloned()
                .map(CameraSource::Usb),
            SourceMode::Rtsp => {
                build_rtsp_source(&self.rtsp_url, &self.rtsp_username, &self.rtsp_password)
            }
        }
    }

    fn connect(&mut self) {
        let Some(source) = self.build_source() else {
            self.status = match self.source_mode {
                SourceMode::Usb => "No camera selected".into(),
                SourceMode::Rtsp => "RTSP URL is empty".into(),
            };
            return;
        };
        self.status = format!("Connecting to {}...", source_label(&source));
        match cameras::open_source(source.clone(), STREAM_CONFIG) {
            Ok(camera) => {
                self.stream = Some(egui_cameras::spawn(camera));
                self.active = true;
                self.status = format!("Streaming: {}", source_label(&source));
                self.refresh_capabilities();
            }
            Err(error) => {
                self.stream = None;
                self.status = format!("Open failed: {error}");
            }
        }
    }

    fn disconnect(&mut self) {
        self.stream = None;
        self.status = "Disconnected".into();
    }

    fn refresh_devices(&mut self) {
        self.devices = cameras::devices().unwrap_or_default();
        if self.selected_device >= self.devices.len() {
            self.selected_device = 0;
        }
    }

    fn snapshot(&mut self) {
        let Some(stream) = &self.stream else {
            return;
        };
        let Some(frame) = capture_frame(&stream.pump) else {
            self.last_capture = Some("Capture failed".into());
            return;
        };
        let variance = analysis::blur_variance(&frame);
        let path = format!("snapshot-{}.png", unix_timestamp());
        match save_png(&frame, &path) {
            Ok(()) => self.last_capture = Some(format!("Wrote {path} (sharpness {variance:.1})")),
            Err(error) => self.last_capture = Some(format!("Save failed: {error}")),
        }
    }

    fn reset_to_defaults(&mut self) {
        let Some(device) = self.devices.get(self.selected_device) else {
            return;
        };
        match cameras::reset_to_defaults(device) {
            Ok(()) => {
                self.current_controls = cameras::read_controls(device).unwrap_or_default();
                self.pending_controls = Controls::default();
                self.last_capture = Some("Controls reset to factory defaults".into());
            }
            Err(error) => {
                self.last_capture = Some(format!("Reset failed: {error}"));
            }
        }
    }

    fn analyze_frame(&mut self) {
        let Some(stream) = &self.stream else {
            return;
        };
        let Some(frame) = capture_frame(&stream.pump) else {
            self.last_capture = Some("Capture failed".into());
            return;
        };
        let variance = analysis::blur_variance(&frame);
        self.last_capture = Some(format!(
            "Current frame sharpness: {variance:.1} (higher = sharper; calibrate per source)"
        ));
    }

    fn pick_sharpest(&mut self) {
        let Some(stream) = &self.stream else {
            return;
        };
        let mut ring = analysis::ring_new(SHARPEST_BURST_SIZE);
        for _ in 0..SHARPEST_BURST_SIZE {
            if let Some(frame) = capture_frame(&stream.pump) {
                analysis::ring_push(&mut ring, frame);
            }
            sleep(SHARPEST_BURST_INTERVAL);
        }
        let Some(sharpest) = analysis::take_sharpest(&ring) else {
            self.last_capture = Some("Burst captured no frames".into());
            return;
        };
        let variance = analysis::blur_variance(&sharpest);
        let path = format!("sharpest-{}.png", unix_timestamp());
        match save_png(&sharpest, &path) {
            Ok(()) => {
                self.last_capture = Some(format!(
                    "Wrote {path} (sharpness {variance:.1}, best of {})",
                    ring.frames.len()
                ))
            }
            Err(error) => self.last_capture = Some(format!("Save failed: {error}")),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.autofocus.is_some() {
            self.tick_autofocus(&ctx);
        }
        if let Some(session) = self.discovery.as_mut() {
            poll_discovery(session);
        }
        if let Some(stream) = &mut self.stream
            && let Err(error) = egui_cameras::update_texture(stream, &ctx)
        {
            self.status = format!("Texture upload failed: {error}");
        }

        egui::Panel::top("bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("cameras egui demo");
                ui.separator();
                ui.selectable_value(&mut self.source_mode, SourceMode::Usb, "USB");
                ui.selectable_value(&mut self.source_mode, SourceMode::Rtsp, "RTSP");
                ui.separator();
                ui.toggle_value(&mut self.discover_open, "Discover");
            });

            if self.discover_open {
                self.render_discover(ui);
            }

            let mut selection_changed = false;
            ui.horizontal(|ui| match self.source_mode {
                SourceMode::Usb => {
                    let labels: Vec<String> = if self.devices.is_empty() {
                        vec!["No cameras".into()]
                    } else {
                        self.devices.iter().map(|d| d.name.clone()).collect()
                    };
                    egui::ComboBox::from_label("Camera")
                        .selected_text(
                            labels
                                .get(self.selected_device)
                                .cloned()
                                .unwrap_or_else(|| "No cameras".into()),
                        )
                        .show_ui(ui, |ui| {
                            for (index, label) in labels.iter().enumerate() {
                                let previous = self.selected_device;
                                let response =
                                    ui.selectable_value(&mut self.selected_device, index, label);
                                if response.clicked() && self.selected_device != previous {
                                    selection_changed = true;
                                }
                            }
                        });
                    if ui.button("Refresh").clicked() {
                        self.refresh_devices();
                        selection_changed = true;
                    }
                }
                SourceMode::Rtsp => {
                    let _ = &mut selection_changed;
                    ui.label("URL");
                    ui.text_edit_singleline(&mut self.rtsp_url);
                    ui.label("User");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rtsp_username).desired_width(120.0),
                    );
                    ui.label("Pass");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rtsp_password)
                            .password(true)
                            .desired_width(120.0),
                    );
                }
            });

            if selection_changed {
                self.refresh_capabilities();
            }

            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    self.connect();
                }
                if self.stream.is_some() && ui.button("Disconnect").clicked() {
                    self.disconnect();
                }
                ui.separator();
                ui.label(&self.status);
                if let Some(last) = &self.last_capture {
                    ui.separator();
                    ui.label(last);
                }
            });
        });

        if self.stream.is_some() {
            egui::Panel::bottom("actions").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    let preview_label = if self.active {
                        "Preview: on"
                    } else {
                        "Preview: off"
                    };
                    if ui.button(preview_label).clicked() {
                        self.active = !self.active;
                        if let Some(stream) = &self.stream {
                            set_active(&stream.pump, self.active);
                        }
                    }
                    if ui.button("Take picture").clicked() {
                        self.snapshot();
                    }
                    if ui.button("Analyze sharpness").clicked() {
                        self.analyze_frame();
                    }
                    if ui
                        .button(format!("Pick sharpest ({SHARPEST_BURST_SIZE}-frame burst)"))
                        .clicked()
                    {
                        self.pick_sharpest();
                    }
                });
                ui.collapsing("Capabilities", |ui| {
                    render_capabilities_panel(
                        ui,
                        self.capabilities.as_ref(),
                        self.capabilities_error.as_deref(),
                    );
                });
                let has_controls_ui = self.capabilities.is_some();
                if has_controls_ui {
                    ui.collapsing("Controls", |ui| {
                        let changed = render_controls_editor(
                            ui,
                            self.capabilities.as_ref().unwrap(),
                            &self.current_controls,
                            &mut self.pending_controls,
                        );
                        if changed && self.live_apply {
                            self.queue_apply();
                        }
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.live_apply, "Apply live");
                            if !self.live_apply && ui.button("Apply").clicked() {
                                self.queue_apply();
                            }
                            if ui.button("Reset to defaults").clicked() {
                                self.reset_to_defaults();
                            }
                            let has_focus = self
                                .capabilities
                                .as_ref()
                                .and_then(|caps| caps.focus.as_ref())
                                .is_some();
                            if has_focus {
                                let autofocus_label = match self.autofocus.as_ref() {
                                    Some(state) => format!(
                                        "Autofocusing {}/{}",
                                        state.sample_index.min(state.total_samples),
                                        state.total_samples
                                    ),
                                    None => "Autofocus (sweep)".to_string(),
                                };
                                let autofocus_button = egui::Button::new(autofocus_label);
                                if ui
                                    .add_enabled(self.autofocus.is_none(), autofocus_button)
                                    .clicked()
                                {
                                    self.start_autofocus();
                                }
                            }
                        });
                    });
                }
            });
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(stream) = &self.stream {
                egui_cameras::show(stream, ui);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Pick a source, configure it, and press Connect.");
                });
            }
        });

        ctx.request_repaint();
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn build_rtsp_source(_url: &str, _username: &str, _password: &str) -> Option<CameraSource> {
    None
}

fn source_label(source: &CameraSource) -> String {
    match source {
        CameraSource::Usb(device) => device.name.clone(),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        CameraSource::Rtsp { url, .. } => url.clone(),
    }
}

fn save_png(frame: &cameras::Frame, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rgba = cameras::to_rgba8(frame)?;
    let file = std::fs::File::create(path)?;
    let encoder = PngEncoder::new(std::io::BufWriter::new(file));
    encoder.write_image(&rgba, frame.width, frame.height, ExtendedColorType::Rgba8)?;
    Ok(())
}

const SUPPORTED_MARK: &str = "✓";
const UNSUPPORTED_MARK: &str = "×";

fn render_capabilities_panel(
    ui: &mut egui::Ui,
    capabilities: Option<&ControlCapabilities>,
    error: Option<&str>,
) {
    if let Some(message) = error {
        ui.colored_label(egui::Color32::from_rgb(200, 80, 80), message);
        return;
    }
    let Some(caps) = capabilities else {
        ui.label("Connect to a USB camera to view its control capabilities.");
        return;
    };
    let rows: [(ControlKind, CapabilityDisplay); 16] = [
        (
            ControlKind::Focus,
            CapabilityDisplay::Range(caps.focus.as_ref()),
        ),
        (
            ControlKind::AutoFocus,
            CapabilityDisplay::Bool(caps.auto_focus),
        ),
        (
            ControlKind::Exposure,
            CapabilityDisplay::Range(caps.exposure.as_ref()),
        ),
        (
            ControlKind::AutoExposure,
            CapabilityDisplay::Bool(caps.auto_exposure),
        ),
        (
            ControlKind::WhiteBalanceTemperature,
            CapabilityDisplay::Range(caps.white_balance_temperature.as_ref()),
        ),
        (
            ControlKind::AutoWhiteBalance,
            CapabilityDisplay::Bool(caps.auto_white_balance),
        ),
        (
            ControlKind::Brightness,
            CapabilityDisplay::Range(caps.brightness.as_ref()),
        ),
        (
            ControlKind::Contrast,
            CapabilityDisplay::Range(caps.contrast.as_ref()),
        ),
        (
            ControlKind::Saturation,
            CapabilityDisplay::Range(caps.saturation.as_ref()),
        ),
        (
            ControlKind::Sharpness,
            CapabilityDisplay::Range(caps.sharpness.as_ref()),
        ),
        (
            ControlKind::Gain,
            CapabilityDisplay::Range(caps.gain.as_ref()),
        ),
        (
            ControlKind::BacklightCompensation,
            CapabilityDisplay::Range(caps.backlight_compensation.as_ref()),
        ),
        (
            ControlKind::PowerLineFrequency,
            CapabilityDisplay::PowerLine(caps.power_line_frequency.is_some()),
        ),
        (
            ControlKind::Pan,
            CapabilityDisplay::Range(caps.pan.as_ref()),
        ),
        (
            ControlKind::Tilt,
            CapabilityDisplay::Range(caps.tilt.as_ref()),
        ),
        (
            ControlKind::Zoom,
            CapabilityDisplay::Range(caps.zoom.as_ref()),
        ),
    ];
    egui::Grid::new("capabilities_grid")
        .num_columns(3)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for (key, display) in rows {
                render_capability_row(ui, key, display);
                ui.end_row();
            }
        });
}

enum CapabilityDisplay<'a> {
    Range(Option<&'a ControlRange>),
    Bool(Option<bool>),
    PowerLine(bool),
}

fn render_capability_row(ui: &mut egui::Ui, key: ControlKind, display: CapabilityDisplay) {
    let green = egui::Color32::from_rgb(80, 200, 120);
    let red = egui::Color32::from_rgb(200, 80, 80);
    let (supported, detail) = match display {
        CapabilityDisplay::Range(Some(range)) => (true, format_range(range)),
        CapabilityDisplay::Range(None) => (false, "not supported".to_string()),
        CapabilityDisplay::Bool(Some(true)) => (true, "auto toggle supported".to_string()),
        CapabilityDisplay::Bool(Some(false)) => (true, "manual only".to_string()),
        CapabilityDisplay::Bool(None) => (false, "not supported".to_string()),
        CapabilityDisplay::PowerLine(true) => (true, "menu supported".to_string()),
        CapabilityDisplay::PowerLine(false) => (false, "not supported".to_string()),
    };
    let mark = if supported {
        SUPPORTED_MARK
    } else {
        UNSUPPORTED_MARK
    };
    let mark_color = if supported { green } else { red };
    let caveat = if supported { None } else { key.caveat() };
    let mark_response = ui.colored_label(mark_color, mark);
    let name_response = ui.label(key.label());
    let detail_response = ui.label(detail);
    if let Some(caveat) = caveat {
        mark_response.on_hover_text(caveat);
        name_response.on_hover_text(caveat);
        detail_response.on_hover_text(caveat);
    }
}

fn auto_lock_hover(paired_label: &str) -> String {
    format!(
        "Disabled because {paired_label} is set to 'auto'. Change {paired_label} to 'leave' or \
         'manual' to enable this slider."
    )
}

fn render_controls_editor(
    ui: &mut egui::Ui,
    capabilities: &ControlCapabilities,
    current: &Controls,
    pending: &mut Controls,
) -> bool {
    let mut changed = false;
    egui::Grid::new("controls_editor_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            changed |= numeric_slider_row(
                ui,
                ControlKind::Focus,
                capabilities.focus.as_ref(),
                current.focus,
                pending.auto_focus == Some(true),
                Some(ControlKind::AutoFocus),
                &mut pending.focus,
            );
            changed |= auto_tri_state_row(
                ui,
                ControlKind::AutoFocus,
                capabilities.auto_focus,
                &mut pending.auto_focus,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Exposure,
                capabilities.exposure.as_ref(),
                current.exposure,
                pending.auto_exposure == Some(true),
                Some(ControlKind::AutoExposure),
                &mut pending.exposure,
            );
            changed |= auto_tri_state_row(
                ui,
                ControlKind::AutoExposure,
                capabilities.auto_exposure,
                &mut pending.auto_exposure,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::WhiteBalanceTemperature,
                capabilities.white_balance_temperature.as_ref(),
                current.white_balance_temperature,
                pending.auto_white_balance == Some(true),
                Some(ControlKind::AutoWhiteBalance),
                &mut pending.white_balance_temperature,
            );
            changed |= auto_tri_state_row(
                ui,
                ControlKind::AutoWhiteBalance,
                capabilities.auto_white_balance,
                &mut pending.auto_white_balance,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Brightness,
                capabilities.brightness.as_ref(),
                current.brightness,
                false,
                None,
                &mut pending.brightness,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Contrast,
                capabilities.contrast.as_ref(),
                current.contrast,
                false,
                None,
                &mut pending.contrast,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Saturation,
                capabilities.saturation.as_ref(),
                current.saturation,
                false,
                None,
                &mut pending.saturation,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Sharpness,
                capabilities.sharpness.as_ref(),
                current.sharpness,
                false,
                None,
                &mut pending.sharpness,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Gain,
                capabilities.gain.as_ref(),
                current.gain,
                false,
                None,
                &mut pending.gain,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::BacklightCompensation,
                capabilities.backlight_compensation.as_ref(),
                current.backlight_compensation,
                false,
                None,
                &mut pending.backlight_compensation,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Pan,
                capabilities.pan.as_ref(),
                current.pan,
                false,
                None,
                &mut pending.pan,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Tilt,
                capabilities.tilt.as_ref(),
                current.tilt,
                false,
                None,
                &mut pending.tilt,
            );
            changed |= numeric_slider_row(
                ui,
                ControlKind::Zoom,
                capabilities.zoom.as_ref(),
                current.zoom,
                false,
                None,
                &mut pending.zoom,
            );
        });
    changed
}

fn numeric_slider_row(
    ui: &mut egui::Ui,
    key: ControlKind,
    range: Option<&ControlRange>,
    current: Option<f32>,
    locked_by_auto: bool,
    paired_auto: Option<ControlKind>,
    pending: &mut Option<f32>,
) -> bool {
    let mut changed = false;
    ui.label(key.label());
    match range {
        Some(range) => {
            let fallback_value = current.unwrap_or(range.default);
            ui.horizontal(|ui| {
                let lock_hover = if locked_by_auto {
                    paired_auto.map(|auto| auto_lock_hover(auto.label()))
                } else {
                    None
                };
                let mut enabled = pending.is_some();
                let checkbox_enabled = !locked_by_auto;
                let checkbox_response =
                    ui.add_enabled(checkbox_enabled, egui::Checkbox::new(&mut enabled, ""));
                if checkbox_response.changed() {
                    *pending = if enabled { Some(fallback_value) } else { None };
                    changed = true;
                }
                if let Some(hover) = lock_hover.as_deref() {
                    checkbox_response.on_hover_text(hover);
                }
                let slider_enabled = enabled && !locked_by_auto;
                let mut value = pending.unwrap_or(fallback_value);
                let slider_response = ui.add_enabled(
                    slider_enabled,
                    egui::Slider::new(&mut value, range.min..=range.max),
                );
                if slider_response.changed() {
                    *pending = Some(value);
                    changed = true;
                }
                if let Some(hover) = lock_hover.as_deref() {
                    slider_response.on_hover_text(hover);
                }
                if locked_by_auto {
                    ui.weak("auto on");
                } else if let Some(current_value) = current {
                    ui.weak(format!("now {current_value:.2}"));
                }
            });
        }
        None => {
            let response = ui.weak("not supported");
            if let Some(caveat) = key.caveat() {
                response.on_hover_text(caveat);
            }
        }
    }
    ui.end_row();
    changed
}

fn auto_tri_state_row(
    ui: &mut egui::Ui,
    key: ControlKind,
    capability: Option<bool>,
    pending: &mut Option<bool>,
) -> bool {
    ui.label(key.label());
    if capability != Some(true) {
        let response = ui.weak("not supported");
        if let Some(caveat) = key.caveat() {
            response.on_hover_text(caveat);
        }
        ui.end_row();
        return false;
    }
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.radio_value(pending, None, "leave").changed();
        changed |= ui.radio_value(pending, Some(false), "manual").changed();
        changed |= ui.radio_value(pending, Some(true), "auto").changed();
    });
    ui.end_row();
    changed
}

fn spawn_apply_worker() -> mpsc::Sender<ApplyRequest> {
    let (tx, rx) = mpsc::channel::<ApplyRequest>();
    std::thread::Builder::new()
        .name("egui-demo-controls".into())
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

fn format_range(range: &ControlRange) -> String {
    if range.step > 0.0 {
        format!(
            "{:.0}..{:.0} (step {:.0}, default {:.0})",
            range.min, range.max, range.step, range.default
        )
    } else {
        format!(
            "{:.2}..{:.2} (default {:.2})",
            range.min, range.max, range.default
        )
    }
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_targets(input: &str) -> Result<(Vec<ipnet::IpNet>, Vec<std::net::SocketAddr>), String> {
    let mut subnets = Vec::new();
    let mut endpoints = Vec::new();
    for raw in input.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        if let Ok(endpoint) = token.parse::<std::net::SocketAddr>() {
            endpoints.push(endpoint);
        } else if let Ok(net) = token.parse::<ipnet::IpNet>() {
            subnets.push(net);
        } else {
            return Err(format!("could not parse `{token}` as CIDR or host:port"));
        }
    }
    if subnets.is_empty() && endpoints.is_empty() {
        return Err("enter at least one CIDR or host:port".into());
    }
    Ok((subnets, endpoints))
}
