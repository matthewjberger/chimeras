//! RTSP network camera backend.
//!
//! Gated behind the `rtsp` feature on macOS and Windows only. Pulls encoded
//! frames off the wire via [`retina`](https://docs.rs/retina) and delivers
//! them through the same [`Camera`] shape as the USB backends.
//!
//! Codec support:
//!
//! - **MJPEG**: passthrough. Frames arrive as [`PixelFormat::Mjpeg`] and decode
//!   via [`crate::to_rgb8`] (which uses `zune-jpeg`).
//! - **H.264 / H.265**: hardware-accelerated native platform decoder, BGRA
//!   output. macOS uses `VideoToolbox`; Windows uses Media Foundation.

use crate::camera::{Camera, Handle};
use crate::decode::{Decoder, VideoCodec, VideoDecoder};
use crate::error::Error;
use crate::types::{Credentials, Frame, FrameQuality, PixelFormat, Resolution, StreamConfig};
use bytes::Bytes;
use crossbeam_channel::Sender;
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Owns the tokio worker thread running the RTSP session. Dropped when the [`Camera`] drops.
pub struct SessionHandle {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// Open a streaming session against an RTSP URL.
///
/// The `url` should be the full `rtsp://host[:port]/path` of the stream. Credentials go
/// in the `credentials` parameter rather than the URL so the URL can be logged safely.
/// The `config` argument is treated as advisory; RTSP streams are pre-encoded on the
/// server side, so [`Camera::config`] reports the actual resolution and framerate
/// observed from the SDP.
///
/// Returns a [`Camera`] whose worker thread runs a single-threaded tokio runtime and
/// pumps frames into the same bounded channel the USB backends use.
pub fn open_rtsp(
    url: &str,
    credentials: Option<Credentials>,
    config: StreamConfig,
) -> Result<Camera, Error> {
    let parsed = url::Url::parse(url).map_err(|error| Error::Backend {
        platform: "rtsp",
        message: format!("invalid RTSP URL: {error}"),
    })?;

    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Result<Frame, Error>>(3);
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<StreamConfig, Error>>(1);
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_worker = Arc::clone(&shutdown);
    let requested = config;

    let worker = std::thread::Builder::new()
        .name("chimeras-rtsp".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(Error::Backend {
                        platform: "rtsp",
                        message: format!("tokio runtime: {error}"),
                    }));
                    return;
                }
            };
            runtime.block_on(run_session(
                parsed,
                credentials,
                requested,
                frame_tx,
                ready_tx,
                shutdown_for_worker,
            ));
        })
        .map_err(|error| Error::Backend {
            platform: "rtsp",
            message: error.to_string(),
        })?;

    let applied = ready_rx
        .recv_timeout(Duration::from_secs(15))
        .map_err(|_| Error::Backend {
            platform: "rtsp",
            message: "RTSP handshake timed out after 15 seconds".into(),
        })??;

    Ok(Camera {
        config: applied,
        frame_rx,
        handle: Handle::Rtsp(SessionHandle {
            shutdown,
            worker: Some(worker),
        }),
    })
}

async fn run_session(
    url: url::Url,
    credentials: Option<Credentials>,
    requested: StreamConfig,
    frame_tx: Sender<Result<Frame, Error>>,
    ready_tx: Sender<Result<StreamConfig, Error>>,
    shutdown: Arc<AtomicBool>,
) {
    let retina_credentials = credentials.map(|creds| retina::client::Credentials {
        username: creds.username,
        password: creds.password,
    });
    let session_options = retina::client::SessionOptions::default().creds(retina_credentials);

    let mut session = match retina::client::Session::describe(url, session_options).await {
        Ok(session) => session,
        Err(error) => {
            let _ = ready_tx.send(Err(Error::Backend {
                platform: "rtsp",
                message: format!("DESCRIBE failed: {error}"),
            }));
            return;
        }
    };

    let Some((video_index, encoding)) = find_video_stream(&session) else {
        let _ = ready_tx.send(Err(Error::Backend {
            platform: "rtsp",
            message: "RTSP source has no video streams".into(),
        }));
        return;
    };

    let (pixel_format, video_codec) = match encoding.as_str() {
        "jpeg" => (PixelFormat::Mjpeg, None),
        "h264" => (PixelFormat::Bgra8, Some(VideoCodec::H264)),
        "h265" | "hevc" => (PixelFormat::Bgra8, Some(VideoCodec::H265)),
        other => {
            let _ = ready_tx.send(Err(Error::Backend {
                platform: "rtsp",
                message: format!("unsupported RTSP encoding '{other}'"),
            }));
            return;
        }
    };

    let extradata = extradata_for_stream(&session, video_index);
    let mut decoder: Option<Decoder> = match video_codec {
        Some(codec) => match Decoder::new(codec, &extradata) {
            Ok(decoder) => Some(decoder),
            Err(error) => {
                let _ = ready_tx.send(Err(error));
                return;
            }
        },
        None => None,
    };

    if let Err(error) = session
        .setup(video_index, retina::client::SetupOptions::default())
        .await
    {
        let _ = ready_tx.send(Err(Error::Backend {
            platform: "rtsp",
            message: format!("SETUP failed: {error}"),
        }));
        return;
    }

    let (initial_resolution, initial_framerate) = probe_initial_dimensions(&session, video_index);
    let applied = StreamConfig {
        resolution: initial_resolution.unwrap_or(requested.resolution),
        framerate: initial_framerate.unwrap_or(requested.framerate),
        pixel_format,
    };

    let playing = match session.play(retina::client::PlayOptions::default()).await {
        Ok(playing) => playing,
        Err(error) => {
            let _ = ready_tx.send(Err(Error::Backend {
                platform: "rtsp",
                message: format!("PLAY failed: {error}"),
            }));
            return;
        }
    };

    let mut demuxed = match playing.demuxed() {
        Ok(demuxed) => demuxed,
        Err(error) => {
            let _ = ready_tx.send(Err(Error::Backend {
                platform: "rtsp",
                message: format!("demuxer: {error}"),
            }));
            return;
        }
    };

    if ready_tx.send(Ok(applied)).is_err() {
        return;
    }

    let mut current_resolution = applied.resolution;
    let mut current_framerate = applied.framerate;

    while !shutdown.load(Ordering::Relaxed) {
        let next = tokio::time::timeout(Duration::from_millis(500), demuxed.next()).await;
        let Ok(item) = next else { continue };
        let Some(item) = item else { break };
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                let _ = frame_tx.try_send(Err(Error::Backend {
                    platform: "rtsp",
                    message: error.to_string(),
                }));
                break;
            }
        };

        let retina::codec::CodecItem::VideoFrame(video) = item else {
            continue;
        };

        if video.has_new_parameters()
            && let Some((new_res, new_fps)) =
                parameters_from_stream(&demuxed, video.stream_id(), current_framerate)
        {
            current_resolution = new_res;
            current_framerate = new_fps;
        }

        let quality = if video.loss() > 0 {
            FrameQuality::Recovering
        } else {
            FrameQuality::Intact
        };
        let timestamp = timestamp_to_duration(&video);

        match decoder.as_mut() {
            Some(decoder) => match decoder.decode(video.data(), timestamp) {
                Ok(mut decoded) => {
                    for mut frame in decoded.drain(..) {
                        frame.timestamp = timestamp;
                        frame.quality = quality;
                        if frame_tx.try_send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = frame_tx.try_send(Err(error));
                    break;
                }
            },
            None => {
                let frame = Frame {
                    width: current_resolution.width,
                    height: current_resolution.height,
                    stride: 0,
                    timestamp,
                    pixel_format,
                    quality,
                    plane_primary: Bytes::copy_from_slice(video.data()),
                    plane_secondary: Bytes::new(),
                };
                let _ = frame_tx.try_send(Ok(frame));
            }
        }
    }
}

fn extradata_for_stream(
    session: &retina::client::Session<retina::client::Described>,
    video_index: usize,
) -> Vec<u8> {
    session
        .streams()
        .get(video_index)
        .and_then(|stream| stream.parameters())
        .and_then(|params| match params {
            retina::codec::ParametersRef::Video(video) => Some(video.extra_data().to_vec()),
            _ => None,
        })
        .unwrap_or_default()
}

fn find_video_stream(
    session: &retina::client::Session<retina::client::Described>,
) -> Option<(usize, String)> {
    for (index, stream) in session.streams().iter().enumerate() {
        if stream.media() == "video" {
            return Some((index, stream.encoding_name().to_string()));
        }
    }
    None
}

fn probe_initial_dimensions(
    session: &retina::client::Session<retina::client::Described>,
    video_index: usize,
) -> (Option<Resolution>, Option<u32>) {
    let Some(stream) = session.streams().get(video_index) else {
        return (None, None);
    };
    let Some(retina::codec::ParametersRef::Video(video_params)) = stream.parameters() else {
        return (None, None);
    };
    let (width, height) = video_params.pixel_dimensions();
    let resolution = Some(Resolution { width, height });
    let framerate = video_params
        .frame_rate()
        .map(|(num, den)| (num as f64 / den.max(1) as f64).round() as u32);
    (resolution, framerate)
}

fn parameters_from_stream(
    demuxed: &retina::client::Demuxed,
    stream_id: usize,
    fallback_fps: u32,
) -> Option<(Resolution, u32)> {
    let stream = demuxed.streams().get(stream_id)?;
    let retina::codec::ParametersRef::Video(video_params) = stream.parameters()? else {
        return None;
    };
    let (width, height) = video_params.pixel_dimensions();
    let framerate = video_params
        .frame_rate()
        .map(|(num, den)| (num as f64 / den.max(1) as f64).round() as u32)
        .unwrap_or(fallback_fps);
    Some((Resolution { width, height }, framerate))
}

fn timestamp_to_duration(video: &retina::codec::VideoFrame) -> Duration {
    let ts = video.timestamp();
    let clock_rate = ts.clock_rate().get();
    if clock_rate == 0 {
        return Duration::ZERO;
    }
    let ticks = ts.timestamp();
    let seconds = ticks as f64 / clock_rate as f64;
    if seconds.is_sign_negative() || !seconds.is_finite() {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(seconds)
}
