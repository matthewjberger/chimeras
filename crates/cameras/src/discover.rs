//! Passive RTSP discovery over a local subnet and/or explicit endpoints.
//!
//! Gated behind the `discover` feature on macOS and Windows. Scans a mix of
//! IPv4 CIDR blocks (probed at [`DiscoverConfig::rtsp_port`], default 554)
//! and explicit `host:port` endpoints, classifies matching hosts by their
//! `Server:` header, and enumerates the camera channels each vendor
//! exposes. Results stream back as [`DiscoverEvent`] over a blocking API
//! shaped like [`mod@crate::monitor`].
//!
//! Hosts that respond to RTSP but whose `Server:` header does not match any
//! known vendor profile are surfaced as [`DiscoverEvent::HostUnmatched`]
//! with the raw header string. This lets callers see exactly what a device
//! returned without having to rebuild with prints, useful when extending the
//! vendor dispatch table or diagnosing "scan finished, nothing found".
//!
//! ```no_run
//! use std::time::Duration;
//! use cameras::discover::{self, DiscoverConfig, DiscoverEvent};
//!
//! fn main() -> Result<(), cameras::Error> {
//!     let net: ipnet::IpNet = "192.168.1.0/24".parse().unwrap();
//!     let discovery = discover::discover(DiscoverConfig {
//!         subnets: vec![net],
//!         ..Default::default()
//!     })?;
//!     loop {
//!         match discover::next_event(&discovery, Duration::from_millis(500)) {
//!             Ok(DiscoverEvent::Done) => break,
//!             Ok(_) => continue,
//!             Err(_) => continue,
//!         }
//!     }
//!     Ok(())
//! }
//! ```

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::error::Error;
use crate::source::CameraSource;

const DEFAULT_RTSP_PORT: u16 = 554;
const MAX_HOSTS: usize = 65_536;
const RESPONSE_HEADER_CAP: usize = 64 * 1024;
const SDP_BODY_CAP: usize = 256 * 1024;
const USER_AGENT: &str = concat!("cameras/", env!("CARGO_PKG_VERSION"), " discover");

/// A camera confirmed by discovery.
///
/// One instance per channel. For a multi-channel encoder this crate yields
/// several [`DiscoveredCamera`]s with the same `host` and distinct `channel`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscoveredCamera {
    /// Source suitable for passing to [`crate::open_source`].
    pub source: CameraSource,
    /// Host the stream lives on.
    pub host: IpAddr,
    /// Channel index inside the host, when the vendor exposes one.
    pub channel: Option<u32>,
    /// Vendor label derived from the RTSP `Server:` header.
    pub vendor: Option<String>,
    /// Model string, when the server header carries one.
    pub model: Option<String>,
}

/// Tunables for a discovery run.
#[derive(Debug, Clone)]
pub struct DiscoverConfig {
    /// CIDR blocks to scan. IPv4 only. Each expanded host is probed at
    /// [`rtsp_port`](Self::rtsp_port). The combined total of subnet-expanded
    /// hosts + [`endpoints`](Self::endpoints) is capped at 65,536.
    pub subnets: Vec<ipnet::IpNet>,
    /// Additional explicit endpoints to probe. Scanned alongside `subnets`.
    /// Use this when you already know the host:port pairs (e.g. local
    /// tunnel endpoints forwarded from remote DVRs). Each entry carries
    /// its own port and ignores [`rtsp_port`](Self::rtsp_port).
    pub endpoints: Vec<SocketAddr>,
    /// Port used when expanding `subnets`. Endpoints carry their own port.
    pub rtsp_port: u16,
    /// How long to wait for a TCP connect before giving up.
    pub connect_timeout: Duration,
    /// Per-read / per-write deadline for RTSP requests.
    pub rtsp_timeout: Duration,
    /// Maximum number of hosts being probed simultaneously.
    pub concurrency: usize,
}

impl Default for DiscoverConfig {
    fn default() -> Self {
        Self {
            subnets: vec![],
            endpoints: vec![],
            rtsp_port: DEFAULT_RTSP_PORT,
            connect_timeout: Duration::from_millis(500),
            rtsp_timeout: Duration::from_secs(2),
            concurrency: 32,
        }
    }
}

/// Events emitted by a running [`Discovery`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DiscoverEvent {
    /// A host answered RTSP and matched a known vendor.
    HostFound {
        /// Host that responded.
        host: IpAddr,
        /// Vendor label derived from the `Server:` header.
        vendor: String,
        /// Model string, when the server header carries one.
        model: Option<String>,
    },
    /// A host answered RTSP but its `Server:` header did not match any known
    /// vendor profile. Useful for debugging "scan completed, nothing found"
    /// situations, inspecting the raw `server` string tells you whether a new
    /// vendor profile is needed or whether the header is branded differently
    /// than expected.
    HostUnmatched {
        /// Host that responded.
        host: IpAddr,
        /// Raw `Server:` header as returned by the host.
        server: String,
    },
    /// A channel on a host returned a playable SDP.
    CameraFound(DiscoveredCamera),
    /// Per-host progress counter; `scanned` increments once per host finished.
    Progress {
        /// Number of hosts fully processed so far.
        scanned: usize,
        /// Total hosts the scan intends to visit.
        total: usize,
    },
    /// The scan finished. Subsequent `next_event` calls keep returning `Done`.
    Done,
}

/// A running discovery scan.
///
/// Obtained from [`discover`]. Cancels and joins its worker on drop.
///
/// The internal event channel is unbounded: the scan task never blocks on
/// backpressure, so a consumer that polls infrequently (or not at all for
/// stretches) cannot stall the scan. Events accumulate until drained or the
/// [`Discovery`] is dropped. For a 65,536-host scan the worst-case memory
/// footprint is a few tens of MB.
pub struct Discovery {
    runtime: Option<tokio::runtime::Runtime>,
    event_rx: Mutex<mpsc::UnboundedReceiver<DiscoverEvent>>,
    shutdown: Arc<AtomicBool>,
    done_emitted: AtomicBool,
}

/// Kick off a scan.
///
/// Returns immediately. If `config.subnets` and `config.endpoints` are both
/// empty, the returned [`Discovery`] emits [`DiscoverEvent::Done`] on the
/// first [`next_event`] call.
pub fn discover(config: DiscoverConfig) -> Result<Discovery, Error> {
    let targets = build_targets(&config)?;
    let total = targets.len();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(config.concurrency.clamp(1, 8))
        .thread_name("cameras-discover")
        .build()
        .map_err(|error| Error::Backend {
            platform: "discover",
            message: format!("tokio runtime: {error}"),
        })?;

    let (event_tx, event_rx) = mpsc::unbounded_channel::<DiscoverEvent>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_task = Arc::clone(&shutdown);
    let config_for_task = config.clone();

    runtime.spawn(async move {
        run_scan(targets, total, config_for_task, shutdown_for_task, event_tx).await;
    });

    Ok(Discovery {
        runtime: Some(runtime),
        event_rx: Mutex::new(event_rx),
        shutdown,
        done_emitted: AtomicBool::new(false),
    })
}

/// Block for the next event up to `timeout`.
///
/// Returns [`Error::Timeout`] if nothing arrived in the window (the scan is
/// still running; try again). Once the scan has finished, this function
/// returns `Ok(DiscoverEvent::Done)` indefinitely.
pub fn next_event(discovery: &Discovery, timeout: Duration) -> Result<DiscoverEvent, Error> {
    if discovery.done_emitted.load(Ordering::Relaxed) {
        return Ok(DiscoverEvent::Done);
    }
    let runtime = discovery
        .runtime
        .as_ref()
        .expect("discovery runtime is always present until drop");
    let mut rx = discovery
        .event_rx
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let result = runtime.block_on(async { tokio::time::timeout(timeout, rx.recv()).await });
    match result {
        Err(_) => Err(Error::Timeout),
        Ok(None) => {
            discovery.done_emitted.store(true, Ordering::Relaxed);
            Ok(DiscoverEvent::Done)
        }
        Ok(Some(event)) => {
            if matches!(event, DiscoverEvent::Done) {
                discovery.done_emitted.store(true, Ordering::Relaxed);
            }
            Ok(event)
        }
    }
}

/// Return the next event immediately if one is buffered, or `None` otherwise.
///
/// After the scan has finished and [`DiscoverEvent::Done`] has been drained,
/// subsequent calls return `None`. This mirrors
/// [`try_next_event`](crate::try_next_event) on [`DeviceMonitor`](crate::DeviceMonitor).
pub fn try_next_event(discovery: &Discovery) -> Option<DiscoverEvent> {
    if discovery.done_emitted.load(Ordering::Relaxed) {
        return None;
    }
    let mut rx = discovery
        .event_rx
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match rx.try_recv() {
        Ok(event) => {
            if matches!(event, DiscoverEvent::Done) {
                discovery.done_emitted.store(true, Ordering::Relaxed);
            }
            Some(event)
        }
        Err(_) => None,
    }
}

/// Cancel the scan and release the runtime. Equivalent to dropping the
/// [`Discovery`].
pub fn cancel(discovery: Discovery) {
    drop(discovery);
}

/// Probe a single host synchronously at [`config.rtsp_port`] and return any
/// cameras it exposes.
///
/// Uses `config.connect_timeout`, `config.rtsp_timeout`, `config.rtsp_port`,
/// and the vendor dispatch table. `config.subnets`, `config.endpoints`, and
/// `config.concurrency` are ignored. For a non-default port, use
/// [`probe_endpoint`].
///
/// [`config.rtsp_port`]: DiscoverConfig::rtsp_port
pub fn probe_host(host: IpAddr, config: &DiscoverConfig) -> Result<Vec<DiscoveredCamera>, Error> {
    probe_endpoint(SocketAddr::new(host, config.rtsp_port), config)
}

/// Probe a single `host:port` endpoint synchronously and return any cameras
/// it exposes.
///
/// Uses `config.connect_timeout`, `config.rtsp_timeout`, and the vendor
/// dispatch table. `config.subnets`, `config.endpoints`, `config.rtsp_port`,
/// and `config.concurrency` are ignored.
pub fn probe_endpoint(
    endpoint: SocketAddr,
    config: &DiscoverConfig,
) -> Result<Vec<DiscoveredCamera>, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Backend {
            platform: "discover",
            message: format!("tokio runtime: {error}"),
        })?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let cfg = config.clone();
    Ok(runtime.block_on(async move {
        let mut cameras = Vec::new();
        let Some(server) = probe_rtsp_server(endpoint, &cfg).await else {
            return cameras;
        };
        let Some(profile) = match_profile(&server) else {
            return cameras;
        };
        cameras.extend(enumerate_channels(endpoint, profile, &cfg, &shutdown).await);
        cameras
    }))
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_millis(200));
        }
    }
}

fn build_targets(config: &DiscoverConfig) -> Result<Vec<SocketAddr>, Error> {
    let mut targets = expand_subnets(&config.subnets, config.rtsp_port)?;
    let subnet_count = targets.len();
    targets.reserve(config.endpoints.len());
    for endpoint in &config.endpoints {
        targets.push(*endpoint);
        if targets.len() > MAX_HOSTS {
            return Err(Error::InvalidSubnet(format!(
                "total hosts exceed {MAX_HOSTS} ({subnet_count} from subnets + \
                 {} from endpoints)",
                config.endpoints.len()
            )));
        }
    }
    Ok(targets)
}

fn expand_subnets(subnets: &[ipnet::IpNet], port: u16) -> Result<Vec<SocketAddr>, Error> {
    let mut hosts = Vec::new();
    for net in subnets {
        match net {
            ipnet::IpNet::V4(v4) => {
                for ipv4 in v4.hosts() {
                    hosts.push(SocketAddr::new(IpAddr::V4(ipv4), port));
                    if hosts.len() > MAX_HOSTS {
                        return Err(Error::InvalidSubnet(format!(
                            "total hosts exceed {MAX_HOSTS}"
                        )));
                    }
                }
            }
            ipnet::IpNet::V6(_) => {
                return Err(Error::InvalidSubnet(
                    "IPv6 subnets are not supported".into(),
                ));
            }
        }
    }
    Ok(hosts)
}

async fn run_scan(
    targets: Vec<SocketAddr>,
    total: usize,
    config: DiscoverConfig,
    shutdown: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<DiscoverEvent>,
) {
    let semaphore = Arc::new(Semaphore::new(config.concurrency.max(1)));
    let scanned = Arc::new(AtomicUsize::new(0));
    let mut set = JoinSet::new();

    for target in targets {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        if shutdown.load(Ordering::Relaxed) {
            drop(permit);
            break;
        }
        let shutdown_for_task = Arc::clone(&shutdown);
        let events_for_task = events.clone();
        let scanned_for_task = Arc::clone(&scanned);
        let config_for_task = config.clone();
        set.spawn(async move {
            handle_host(
                target,
                &config_for_task,
                &shutdown_for_task,
                &events_for_task,
            )
            .await;
            let done = scanned_for_task.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = events_for_task.send(DiscoverEvent::Progress {
                scanned: done,
                total,
            });
            drop(permit);
        });
    }

    while set.join_next().await.is_some() {}
    let _ = events.send(DiscoverEvent::Done);
}

async fn handle_host(
    addr: SocketAddr,
    config: &DiscoverConfig,
    shutdown: &AtomicBool,
    events: &mpsc::UnboundedSender<DiscoverEvent>,
) {
    if shutdown.load(Ordering::Relaxed) {
        return;
    }
    let Some(server) = probe_rtsp_server(addr, config).await else {
        return;
    };
    let Some(profile) = match_profile(&server) else {
        let _ = events.send(DiscoverEvent::HostUnmatched {
            host: addr.ip(),
            server,
        });
        return;
    };
    let vendor = profile.name.to_string();
    let _ = events.send(DiscoverEvent::HostFound {
        host: addr.ip(),
        vendor,
        model: None,
    });

    for cam in enumerate_channels(addr, profile, config, shutdown).await {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let _ = events.send(DiscoverEvent::CameraFound(cam));
    }
}

async fn probe_rtsp_server(addr: SocketAddr, config: &DiscoverConfig) -> Option<String> {
    let connect = tokio::time::timeout(config.connect_timeout, TcpStream::connect(addr)).await;
    let mut stream = match connect {
        Ok(Ok(stream)) => stream,
        _ => return None,
    };
    let response = rtsp_options(&mut stream, addr, config.rtsp_timeout)
        .await
        .ok()?;
    let _ = stream.shutdown().await;
    response.server
}

/// Enumerate confirmed channels for a matched vendor.
async fn enumerate_channels(
    addr: SocketAddr,
    profile: &'static VendorProfile,
    config: &DiscoverConfig,
    shutdown: &AtomicBool,
) -> Vec<DiscoveredCamera> {
    enumerate_channels_with_probe(
        addr,
        profile,
        config,
        shutdown,
        |addr, path, connect_timeout, rtsp_timeout| {
            Box::pin(rtsp_describe(addr, path, connect_timeout, rtsp_timeout))
        },
    )
    .await
}

type ProbeFuture = Pin<Box<dyn Future<Output = Result<DescribeResult, ProbeError>> + Send>>;

async fn enumerate_channels_with_probe<F>(
    addr: SocketAddr,
    profile: &VendorProfile,
    config: &DiscoverConfig,
    shutdown: &AtomicBool,
    mut probe: F,
) -> Vec<DiscoveredCamera>
where
    F: FnMut(SocketAddr, String, Duration, Duration) -> ProbeFuture,
{
    let mut cameras = Vec::new();
    for channel in 1..=profile.max_channels {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let path = (profile.channel_path)(channel);
        let url = format!("rtsp://{}:{}/{}", addr.ip(), addr.port(), path);
        match probe(addr, path, config.connect_timeout, config.rtsp_timeout).await {
            Ok(DescribeResult::Ok { .. }) => cameras.push(DiscoveredCamera {
                source: CameraSource::Rtsp {
                    url,
                    credentials: None,
                },
                host: addr.ip(),
                channel: Some(channel),
                vendor: Some(profile.name.to_string()),
                model: None,
            }),
            Ok(DescribeResult::NotFound) | Ok(DescribeResult::Other(_)) | Err(_) => break,
        }
    }
    cameras
}

struct VendorProfile {
    name: &'static str,
    matches: fn(&str) -> bool,
    channel_path: fn(u32) -> String,
    max_channels: u32,
}

const PROFILES: &[VendorProfile] = &[
    VendorProfile {
        name: "Axis",
        matches: axis_matches,
        channel_path: axis_channel_path,
        max_channels: 8,
    },
    VendorProfile {
        name: "Axis (GStreamer)",
        matches: |s| s.to_ascii_lowercase().contains("gstreamer rtsp server"),
        channel_path: |n| format!("axis-media/media.amp?camera={n}"),
        max_channels: 8,
    },
];

fn axis_matches(server: &str) -> bool {
    server.to_ascii_lowercase().contains("axis")
}

fn axis_channel_path(channel: u32) -> String {
    format!("axis-media/media.amp?camera={channel}")
}

fn match_profile(server: &str) -> Option<&'static VendorProfile> {
    PROFILES.iter().find(|profile| (profile.matches)(server))
}

#[derive(Debug)]
struct OptionsResponse {
    server: Option<String>,
}

/// Result of a single `DESCRIBE` probe.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DescribeResult {
    Ok { sdp: String },
    NotFound,
    Other(u16),
}

#[derive(Debug)]
enum ProbeError {
    Timeout,
    Io,
    Eof,
    Malformed,
}

impl From<std::io::Error> for ProbeError {
    fn from(_: std::io::Error) -> Self {
        ProbeError::Io
    }
}

async fn rtsp_options(
    stream: &mut TcpStream,
    addr: SocketAddr,
    timeout: Duration,
) -> Result<OptionsResponse, ProbeError> {
    let request = format!(
        "OPTIONS rtsp://{}:{}/ RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: {USER_AGENT}\r\n\r\n",
        addr.ip(),
        addr.port()
    );
    write_all(stream, request.as_bytes(), timeout).await?;
    let (_status, headers, _body) = read_rtsp_response(stream, timeout, false).await?;
    Ok(OptionsResponse {
        server: header_get(&headers, "server"),
    })
}

async fn rtsp_describe(
    addr: SocketAddr,
    path: String,
    connect_timeout: Duration,
    rtsp_timeout: Duration,
) -> Result<DescribeResult, ProbeError> {
    let connect = tokio::time::timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ProbeError::Timeout)??;
    let mut stream = connect;
    let request = format!(
        "DESCRIBE rtsp://{}:{}/{path} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\nUser-Agent: {USER_AGENT}\r\n\r\n",
        addr.ip(),
        addr.port()
    );
    write_all(&mut stream, request.as_bytes(), rtsp_timeout).await?;
    let (status, _headers, body) = read_rtsp_response(&mut stream, rtsp_timeout, true).await?;
    let _ = stream.shutdown().await;
    classify_describe(status, &body)
}

fn classify_describe(status: u16, body: &[u8]) -> Result<DescribeResult, ProbeError> {
    match status {
        200 => {
            let sdp = String::from_utf8_lossy(body).into_owned();
            if sdp_has_video(&sdp) {
                Ok(DescribeResult::Ok { sdp })
            } else {
                Ok(DescribeResult::Other(200))
            }
        }
        404 => Ok(DescribeResult::NotFound),
        other => Ok(DescribeResult::Other(other)),
    }
}

fn sdp_has_video(sdp: &str) -> bool {
    sdp.lines().any(|line| line.starts_with("m=video"))
}

async fn write_all(
    stream: &mut TcpStream,
    bytes: &[u8],
    timeout: Duration,
) -> Result<(), ProbeError> {
    tokio::time::timeout(timeout, stream.write_all(bytes))
        .await
        .map_err(|_| ProbeError::Timeout)??;
    Ok(())
}

async fn read_rtsp_response(
    stream: &mut TcpStream,
    timeout: Duration,
    read_body: bool,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), ProbeError> {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::<u8>::new();
    let header_end = loop {
        if let Some(index) = find_header_terminator(&buf) {
            break index;
        }
        if buf.len() > RESPONSE_HEADER_CAP {
            return Err(ProbeError::Malformed);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProbeError::Timeout);
        }
        let mut chunk = [0u8; 2048];
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .map_err(|_| ProbeError::Timeout)??;
        if read == 0 {
            return Err(ProbeError::Eof);
        }
        buf.extend_from_slice(&chunk[..read]);
    };

    let header_bytes = &buf[..header_end];
    let (status, headers) = parse_status_and_headers(header_bytes)?;
    if !read_body {
        return Ok((status, headers, Vec::new()));
    }

    let body_start = header_end + 4;
    let content_length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(SDP_BODY_CAP);

    let mut body = if buf.len() > body_start {
        buf[body_start..].to_vec()
    } else {
        Vec::new()
    };
    while body.len() < content_length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProbeError::Timeout);
        }
        let needed = content_length - body.len();
        let mut chunk = vec![0u8; needed.min(4096)];
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .map_err(|_| ProbeError::Timeout)??;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok((status, headers, body))
}

fn find_header_terminator(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_status_and_headers(bytes: &[u8]) -> Result<(u16, Vec<(String, String)>), ProbeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProbeError::Malformed)?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or(ProbeError::Malformed)?;
    let mut status_parts = status_line.splitn(3, ' ');
    let _version = status_parts.next().ok_or(ProbeError::Malformed)?;
    let status_code = status_parts
        .next()
        .ok_or(ProbeError::Malformed)?
        .parse::<u16>()
        .map_err(|_| ProbeError::Malformed)?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    Ok((status_code, headers))
}

fn header_get(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis_profile() -> &'static VendorProfile {
        PROFILES.iter().find(|p| p.name == "Axis").unwrap()
    }

    #[test]
    fn subnet_expansion_slash_twenty_four() {
        let net: ipnet::IpNet = "192.168.1.0/24".parse().unwrap();
        let hosts = expand_subnets(&[net], 554).unwrap();
        assert_eq!(hosts.len(), 254);
        assert!(hosts.iter().all(|addr| addr.port() == 554));
    }

    #[test]
    fn subnet_expansion_slash_twenty_two() {
        let net: ipnet::IpNet = "10.0.0.0/22".parse().unwrap();
        let hosts = expand_subnets(&[net], 554).unwrap();
        assert_eq!(hosts.len(), 1022);
    }

    #[test]
    fn subnet_expansion_slash_sixteen() {
        let net: ipnet::IpNet = "10.0.0.0/16".parse().unwrap();
        let hosts = expand_subnets(&[net], 554).unwrap();
        assert_eq!(hosts.len(), 65_534);
    }

    #[test]
    fn subnet_expansion_uses_configured_port() {
        let net: ipnet::IpNet = "192.168.1.0/30".parse().unwrap();
        let hosts = expand_subnets(&[net], 8554).unwrap();
        assert!(hosts.iter().all(|addr| addr.port() == 8554));
    }

    #[test]
    fn subnet_expansion_rejects_ipv6() {
        let net: ipnet::IpNet = "fe80::/64".parse().unwrap();
        let result = expand_subnets(&[net], 554);
        match result {
            Err(Error::InvalidSubnet(message)) => assert!(message.contains("IPv6")),
            other => panic!("expected InvalidSubnet, got {other:?}"),
        }
    }

    #[test]
    fn subnet_expansion_rejects_over_cap() {
        let a: ipnet::IpNet = "10.0.0.0/16".parse().unwrap();
        let b: ipnet::IpNet = "10.1.0.0/16".parse().unwrap();
        let result = expand_subnets(&[a, b], 554);
        match result {
            Err(Error::InvalidSubnet(message)) => assert!(message.contains("65536")),
            other => panic!("expected InvalidSubnet, got {other:?}"),
        }
    }

    #[test]
    fn default_rtsp_port_is_554() {
        let config = DiscoverConfig::default();
        assert_eq!(config.rtsp_port, 554);
    }

    #[test]
    fn default_endpoints_is_empty() {
        let config = DiscoverConfig::default();
        assert!(config.endpoints.is_empty());
    }

    #[test]
    fn build_targets_endpoint_only() {
        let config = DiscoverConfig {
            endpoints: vec![
                "127.0.0.1:554".parse().unwrap(),
                "127.0.0.1:555".parse().unwrap(),
            ],
            ..Default::default()
        };
        let targets = build_targets(&config).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].port(), 554);
        assert_eq!(targets[1].port(), 555);
    }

    #[test]
    fn build_targets_mixes_subnets_and_endpoints() {
        let net: ipnet::IpNet = "192.168.1.0/30".parse().unwrap();
        let config = DiscoverConfig {
            subnets: vec![net],
            endpoints: vec!["127.0.0.1:9000".parse().unwrap()],
            ..Default::default()
        };
        let targets = build_targets(&config).unwrap();
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].port(), 554);
        assert_eq!(targets[1].port(), 554);
        assert_eq!(targets[2].port(), 9000);
    }

    #[test]
    fn build_targets_combined_cap() {
        let net: ipnet::IpNet = "10.0.0.0/16".parse().unwrap();
        let endpoints: Vec<SocketAddr> = (0..1000)
            .map(|i| {
                format!("127.0.0.{}:{}", (i % 250) + 1, 10000 + i)
                    .parse()
                    .unwrap()
            })
            .collect();
        let config = DiscoverConfig {
            subnets: vec![net],
            endpoints,
            ..Default::default()
        };
        let result = build_targets(&config);
        match result {
            Err(Error::InvalidSubnet(message)) => {
                assert!(message.contains("65536"), "{message}");
                assert!(message.contains("endpoints"), "{message}");
                assert!(message.contains("subnets"), "{message}");
            }
            other => panic!("expected InvalidSubnet, got {other:?}"),
        }
    }

    #[test]
    fn axis_matches_axis_header() {
        assert!(axis_matches("AXIS Communications AB"));
        assert!(axis_matches("axis Q6000-E"));
    }

    #[test]
    fn axis_does_not_match_other_vendors() {
        assert!(!axis_matches("Hikvision"));
        assert!(!axis_matches(""));
        assert!(!axis_matches("Dahua"));
    }

    #[test]
    fn gstreamer_header_matches_axis_gstreamer_profile() {
        let profile = match_profile("GStreamer RTSP server").expect("should match");
        assert_eq!(profile.name, "Axis (GStreamer)");
    }

    #[test]
    fn axis_branded_header_still_matches_axis_profile() {
        let profile = match_profile("AXIS Communications AB").expect("should match");
        assert_eq!(profile.name, "Axis");
    }

    #[test]
    fn enumeration_early_stop_after_two_hits() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let profile = axis_profile();
        let shutdown = AtomicBool::new(false);
        let config = DiscoverConfig::default();
        let addr: SocketAddr = "10.0.0.1:554".parse().unwrap();
        let cameras = runtime.block_on(enumerate_channels_with_probe(
            addr,
            profile,
            &config,
            &shutdown,
            |_addr, path, _connect, _rtsp| {
                Box::pin(async move {
                    if path.contains("camera=1") || path.contains("camera=2") {
                        Ok(DescribeResult::Ok {
                            sdp: "v=0\r\nm=video 0 RTP/AVP 96\r\n".into(),
                        })
                    } else {
                        Ok(DescribeResult::NotFound)
                    }
                })
            },
        ));
        assert_eq!(cameras.len(), 2);
        assert_eq!(cameras[0].channel, Some(1));
        assert_eq!(cameras[1].channel, Some(2));
    }

    #[test]
    fn enumeration_yields_max_when_all_ok() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let profile = axis_profile();
        let shutdown = AtomicBool::new(false);
        let config = DiscoverConfig::default();
        let addr: SocketAddr = "10.0.0.1:554".parse().unwrap();
        let cameras = runtime.block_on(enumerate_channels_with_probe(
            addr,
            profile,
            &config,
            &shutdown,
            |_addr, _path, _connect, _rtsp| {
                Box::pin(async move {
                    Ok(DescribeResult::Ok {
                        sdp: "m=video\r\n".into(),
                    })
                })
            },
        ));
        assert_eq!(cameras.len(), profile.max_channels as usize);
    }

    #[test]
    fn enumeration_immediate_not_found_yields_zero() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let profile = axis_profile();
        let shutdown = AtomicBool::new(false);
        let config = DiscoverConfig::default();
        let addr: SocketAddr = "10.0.0.1:554".parse().unwrap();
        let cameras = runtime.block_on(enumerate_channels_with_probe(
            addr,
            profile,
            &config,
            &shutdown,
            |_addr, _path, _connect, _rtsp| Box::pin(async move { Ok(DescribeResult::NotFound) }),
        ));
        assert!(cameras.is_empty());
    }

    #[test]
    fn enumeration_url_uses_endpoint_port() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let profile = axis_profile();
        let shutdown = AtomicBool::new(false);
        let config = DiscoverConfig::default();
        let addr: SocketAddr = "127.0.0.1:555".parse().unwrap();
        let cameras = runtime.block_on(enumerate_channels_with_probe(
            addr,
            profile,
            &config,
            &shutdown,
            |_addr, path, _connect, _rtsp| {
                Box::pin(async move {
                    if path.contains("camera=1") {
                        Ok(DescribeResult::Ok {
                            sdp: "m=video\r\n".into(),
                        })
                    } else {
                        Ok(DescribeResult::NotFound)
                    }
                })
            },
        ));
        assert_eq!(cameras.len(), 1);
        let CameraSource::Rtsp { url, .. } = &cameras[0].source else {
            panic!("expected RTSP source");
        };
        assert_eq!(
            url, "rtsp://127.0.0.1:555/axis-media/media.amp?camera=1",
            "URL must use endpoint's port, not 554"
        );
    }

    #[test]
    fn classify_describe_requires_video_media() {
        let audio_only = b"v=0\r\nm=audio 0 RTP/AVP 0\r\n";
        match classify_describe(200, audio_only).unwrap() {
            DescribeResult::Other(200) => {}
            other => panic!("expected Other(200), got {other:?}"),
        }
    }

    #[test]
    fn classify_describe_accepts_video() {
        let body = b"v=0\r\nm=video 0 RTP/AVP 96\r\n";
        match classify_describe(200, body).unwrap() {
            DescribeResult::Ok { .. } => {}
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn classify_describe_not_found() {
        let result = classify_describe(404, b"").unwrap();
        assert_eq!(result, DescribeResult::NotFound);
    }

    #[test]
    fn parse_status_and_headers_basic() {
        let raw = b"RTSP/1.0 200 OK\r\nServer: AXIS\r\nContent-Length: 10\r\n";
        let (status, headers) = parse_status_and_headers(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(header_get(&headers, "server").as_deref(), Some("AXIS"));
        assert_eq!(
            header_get(&headers, "content-length").as_deref(),
            Some("10")
        );
    }

    #[test]
    fn empty_subnets_returns_empty_hosts() {
        let hosts = expand_subnets(&[], 554).unwrap();
        assert!(hosts.is_empty());
    }

    #[test]
    fn empty_discovery_emits_only_done() {
        let config = DiscoverConfig::default();
        assert!(
            config.subnets.is_empty() && config.endpoints.is_empty(),
            "this test asserts the both-empty invariant; keep Default aligned",
        );
        let discovery = discover(config).expect("discover");
        let event = next_event(&discovery, Duration::from_secs(2)).expect("event");
        assert!(matches!(event, DiscoverEvent::Done));
        let again = next_event(&discovery, Duration::from_millis(10)).expect("event");
        assert!(matches!(again, DiscoverEvent::Done));
    }
}
