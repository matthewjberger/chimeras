use crate::error::Error;
use crate::macos::enumerate::devices as list_devices;
use crate::monitor::DeviceMonitor;
use crate::types::{DeviceEvent, DeviceId};
use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(1000);

pub fn monitor() -> Result<DeviceMonitor, Error> {
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<DeviceEvent>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_thread = Arc::clone(&shutdown);

    let initial = list_devices()?;
    for device in &initial {
        let _ = event_tx.send(DeviceEvent::Added(device.clone()));
    }

    let worker = std::thread::Builder::new()
        .name("chimeras-monitor".into())
        .spawn(move || poll_loop(event_tx, shutdown_for_thread, initial))
        .map_err(|error| Error::Backend {
            platform: "macos",
            message: error.to_string(),
        })?;

    Ok(DeviceMonitor {
        event_rx,
        shutdown,
        worker: Some(worker),
    })
}

fn poll_loop(
    event_tx: Sender<DeviceEvent>,
    shutdown: Arc<AtomicBool>,
    initial: Vec<crate::types::Device>,
) {
    let mut known: HashMap<DeviceId, crate::types::Device> = initial
        .into_iter()
        .map(|device| (device.id.clone(), device))
        .collect();

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let Ok(current) = list_devices() else {
            continue;
        };

        let mut current_map: HashMap<DeviceId, crate::types::Device> = current
            .into_iter()
            .map(|device| (device.id.clone(), device))
            .collect();

        for (id, device) in &current_map {
            if !known.contains_key(id) {
                let _ = event_tx.send(DeviceEvent::Added(device.clone()));
            }
        }

        let removed: Vec<DeviceId> = known
            .keys()
            .filter(|id| !current_map.contains_key(id))
            .cloned()
            .collect();
        for id in removed {
            let _ = event_tx.send(DeviceEvent::Removed(id.clone()));
            known.remove(&id);
        }

        for (id, device) in current_map.drain() {
            known.insert(id, device);
        }
    }
}
