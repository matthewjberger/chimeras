//! Scan a subnet or explicit endpoints for Axis RTSP cameras and print
//! what turns up.
//!
//! Accepts a comma-separated list of CIDR blocks and/or `host:port`
//! endpoints. Defaults to `192.168.1.0/24` when no argument is given.
//! Only available on macOS and Windows, the RTSP sink is platform-gated.
//!
//! ```bash
//! cargo run --features discover --example discover -- 192.168.1.0/24
//! cargo run --features discover --example discover -- 127.0.0.1:554,127.0.0.1:555
//! cargo run --features discover --example discover -- "10.0.0.0/24,127.0.0.1:8554"
//! ```

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), cameras::Error> {
    use std::net::SocketAddr;
    use std::time::Duration;

    use cameras::discover::{DiscoverConfig, DiscoverEvent, discover, next_event};

    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.1.0/24".into());

    let mut subnets = Vec::new();
    let mut endpoints = Vec::new();
    for raw in input.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        if let Ok(endpoint) = token.parse::<SocketAddr>() {
            endpoints.push(endpoint);
        } else if let Ok(net) = token.parse::<ipnet::IpNet>() {
            subnets.push(net);
        } else {
            eprintln!(
                "usage: discover <target[,target...]>\n  \
                 target is a CIDR (e.g. 10.0.0.0/24) or host:port (e.g. 127.0.0.1:554)"
            );
            panic!("could not parse `{token}` as CIDR or SocketAddr");
        }
    }

    let config = DiscoverConfig {
        subnets,
        endpoints,
        ..Default::default()
    };

    let discovery = discover(config)?;
    let mut hosts = 0;
    let mut cams = 0;

    loop {
        match next_event(&discovery, Duration::from_millis(500)) {
            Ok(DiscoverEvent::HostFound { host, vendor, .. }) => {
                hosts += 1;
                println!("host: {host} [{vendor}]");
            }
            Ok(DiscoverEvent::HostUnmatched { host, server }) => {
                println!("host: {host} [unmatched] server={server:?}");
            }
            Ok(DiscoverEvent::CameraFound(cam)) => {
                cams += 1;
                let url = match &cam.source {
                    cameras::CameraSource::Rtsp { url, .. } => url.as_str(),
                    _ => "(non-rtsp)",
                };
                let channel = cam
                    .channel
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into());
                println!("  cam: ch{channel} -> {url}");
            }
            Ok(DiscoverEvent::Progress { scanned, total }) => {
                eprint!("\r  scanned {scanned}/{total}");
            }
            Ok(DiscoverEvent::Done) => {
                eprintln!();
                break;
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }

    println!("done: {hosts} hosts, {cams} cameras");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("cameras::discover is only available on macOS and Windows");
}
