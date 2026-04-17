//! Unified source enum covering both USB and RTSP cameras, plus
//! [`open_source`] for opening either through one call.

use std::hash::{Hash, Hasher};

use crate::error::Error;
#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
use crate::open_rtsp;
#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
use crate::types::Credentials;
use crate::types::{Device, StreamConfig};
use crate::{Camera, open};

/// Describes where a stream's frames come from.
///
/// The `Rtsp` variant is available only with the `rtsp` feature (enabled by
/// default) on macOS and Windows. Two sources compare equal when they
/// describe the same logical stream: USB by device id, RTSP by URL and
/// credentials.
#[derive(Clone, Debug)]
pub enum CameraSource {
    /// A USB / built-in camera enumerated via [`crate::devices`].
    Usb(Device),
    /// An RTSP network stream.
    #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows"))))
    )]
    Rtsp {
        /// `rtsp://host:port/path` URL.
        url: String,
        /// Optional credentials for authenticated streams.
        credentials: Option<Credentials>,
    },
}

impl PartialEq for CameraSource {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CameraSource::Usb(a), CameraSource::Usb(b)) => a.id == b.id,
            #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
            (
                CameraSource::Rtsp {
                    url: a_url,
                    credentials: a_creds,
                },
                CameraSource::Rtsp {
                    url: b_url,
                    credentials: b_creds,
                },
            ) => a_url == b_url && credentials_eq(a_creds.as_ref(), b_creds.as_ref()),
            #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
            _ => false,
        }
    }
}

impl Eq for CameraSource {}

impl Hash for CameraSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            CameraSource::Usb(device) => device.id.hash(state),
            #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
            CameraSource::Rtsp { url, credentials } => {
                url.hash(state);
                match credentials {
                    None => 0u8.hash(state),
                    Some(c) => {
                        1u8.hash(state);
                        c.username.hash(state);
                        c.password.hash(state);
                    }
                }
            }
        }
    }
}

/// Produce a human-readable label for a source: device name for USB, URL
/// for RTSP. Useful for status lines and toasts.
pub fn source_label(source: &CameraSource) -> String {
    match source {
        CameraSource::Usb(device) => device.name.clone(),
        #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
        CameraSource::Rtsp { url, .. } => url.clone(),
    }
}

/// Open a camera described by `source`, dispatching to [`open`] or
/// `open_rtsp` as appropriate.
pub fn open_source(source: CameraSource, config: StreamConfig) -> Result<Camera, Error> {
    match source {
        CameraSource::Usb(device) => open(&device, config),
        #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
        CameraSource::Rtsp { url, credentials } => open_rtsp(&url, credentials, config),
    }
}

#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
fn credentials_eq(a: Option<&Credentials>, b: Option<&Credentials>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.username == b.username && a.password == b.password,
        _ => false,
    }
}
