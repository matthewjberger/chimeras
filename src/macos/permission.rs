use crate::error::Error;
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeVideo};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub fn ensure_authorized() -> Result<(), Error> {
    let media_type = unsafe { AVMediaTypeVideo }.ok_or(Error::Backend {
        platform: "macos",
        message: "AVMediaTypeVideo symbol unavailable".into(),
    })?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status == AVAuthorizationStatus::NotDetermined {
        return request_authorization(media_type);
    }
    Err(Error::PermissionDenied)
}

fn request_authorization(media_type: &objc2_foundation::NSString) -> Result<(), Error> {
    let state = Arc::new((Mutex::new(None::<bool>), Condvar::new()));
    let state_for_block = Arc::clone(&state);

    let block = RcBlock::new(move |granted: Bool| {
        let (mutex, cvar) = &*state_for_block;
        if let Ok(mut slot) = mutex.lock() {
            *slot = Some(granted.as_bool());
        }
        cvar.notify_all();
    });

    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block);
    }

    let (mutex, cvar) = &*state;
    let mut slot = mutex.lock().map_err(|_| Error::PermissionDenied)?;
    let deadline = Duration::from_secs(120);
    while slot.is_none() {
        let (next_slot, wait_result) = cvar
            .wait_timeout(slot, deadline)
            .map_err(|_| Error::PermissionDenied)?;
        slot = next_slot;
        if wait_result.timed_out() {
            return Err(Error::PermissionDenied);
        }
    }

    if slot.take().unwrap_or(false) {
        Ok(())
    } else {
        Err(Error::PermissionDenied)
    }
}
