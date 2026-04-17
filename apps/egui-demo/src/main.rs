use std::thread::sleep;
use std::time::Duration;

use cameras::analysis;
use cameras::{CameraSource, Credentials, Device, PixelFormat, Resolution, StreamConfig};
use eframe::egui;
use egui_cameras::{capture_frame, set_active};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

const SHARPEST_BURST_SIZE: usize = 16;
const SHARPEST_BURST_INTERVAL: Duration = Duration::from_millis(30);

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
}

impl App {
    fn new() -> Self {
        let devices = cameras::devices().unwrap_or_default();
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(stream) = &mut self.stream
            && let Err(error) = egui_cameras::update_texture(stream, ctx)
        {
            self.status = format!("Texture upload failed: {error}");
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("cameras egui demo");
                ui.separator();
                ui.selectable_value(&mut self.source_mode, SourceMode::Usb, "USB");
                ui.selectable_value(&mut self.source_mode, SourceMode::Rtsp, "RTSP");
            });

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
                                ui.selectable_value(&mut self.selected_device, index, label);
                            }
                        });
                    if ui.button("Refresh").clicked() {
                        self.refresh_devices();
                    }
                }
                SourceMode::Rtsp => {
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
            egui::TopBottomPanel::bottom("actions").show(ctx, |ui| {
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
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
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

fn source_label(source: &CameraSource) -> String {
    match source {
        CameraSource::Usb(device) => device.name.clone(),
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

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
