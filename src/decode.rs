//! Hardware-accelerated H.264 / H.265 decoders for the RTSP backend.
//!
//! Each platform has a native path:
//!
//! - macOS: `VideoToolbox` via `objc2-video-toolbox`
//! - Windows: `Media Foundation` `IMFTransform` via the `windows` crate
//! - Linux: `VA-API` via `cros-libva`
//!
//! Decoders accept AVCC-formatted NAL units (length-prefixed, as delivered by
//! `retina`) plus codec extradata (SPS/PPS parsed from the SDP) and produce
//! BGRA [`Frame`]s through the same channel USB backends use.

use crate::error::Error;
use crate::types::Frame;
use std::time::Duration;

/// Which video codec the decoder is configured for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    /// H.264 / AVC
    H264,
    /// H.265 / HEVC
    H265,
}

/// Platform decoder contract.
///
/// Concrete implementations hold platform-specific state
/// (`VTDecompressionSession`, `IMFTransform`, VA-API context). Construction
/// prepares the decoder from extradata; `decode` accepts one NAL unit payload
/// and returns every BGRA frame that is ready to deliver.
pub(crate) trait VideoDecoder: Send {
    fn new(codec: VideoCodec, extradata: &[u8]) -> Result<Self, Error>
    where
        Self: Sized;

    fn decode(&mut self, nal: &[u8], timestamp: Duration) -> Result<Vec<Frame>, Error>;
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub(crate) type Decoder = macos::VideoToolboxDecoder;

#[cfg(target_os = "windows")]
pub(crate) type Decoder = windows::MediaFoundationDecoder;
