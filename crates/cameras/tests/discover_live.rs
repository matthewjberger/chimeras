//! Live-network integration test for `cameras::discover`.
//!
//! Skipped unless `CAMERAS_DISCOVER_LIVE_SUBNET` is set to a CIDR block.
//! The test drives a real scan against that subnet and drains events until
//! `Done`.

#![cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows")))]

use std::time::Duration;

use cameras::discover::{self, DiscoverConfig, DiscoverEvent};

#[test]
fn live_scan_from_env() {
    let Ok(cidr) = std::env::var("CAMERAS_DISCOVER_LIVE_SUBNET") else {
        eprintln!("skipping: CAMERAS_DISCOVER_LIVE_SUBNET not set");
        return;
    };
    let Ok(net) = cidr.parse::<ipnet::IpNet>() else {
        eprintln!("skipping: CAMERAS_DISCOVER_LIVE_SUBNET='{cidr}' is not a valid CIDR");
        return;
    };
    let config = DiscoverConfig {
        subnets: vec![net],
        ..Default::default()
    };
    let discovery = discover::discover(config).expect("start discovery");
    let mut hosts = 0usize;
    let mut cams = 0usize;
    let mut unmatched = 0usize;
    loop {
        match discover::next_event(&discovery, Duration::from_secs(5)) {
            Ok(DiscoverEvent::HostFound { .. }) => hosts += 1,
            Ok(DiscoverEvent::HostUnmatched { host, server }) => {
                unmatched += 1;
                eprintln!("unmatched rtsp host: {host} server={server:?}");
            }
            Ok(DiscoverEvent::CameraFound(_)) => cams += 1,
            Ok(DiscoverEvent::Progress { .. }) => {}
            Ok(DiscoverEvent::Done) => break,
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    eprintln!("live scan finished: {hosts} hosts, {cams} cameras, {unmatched} unmatched");
}
