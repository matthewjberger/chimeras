use crate::error::Error;
use crate::types::{Frame, PixelFormat};
use bytes::Bytes;
use crossbeam_channel::Sender;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use std::time::Duration;

pub struct DelegateIvars {
    pub sender: Sender<Result<Frame, Error>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AllocAnyThread]
    #[name = "ChimerasFrameDelegate"]
    #[ivars = DelegateIvars]
    pub struct FrameDelegate;

    unsafe impl NSObjectProtocol for FrameDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn capture_output_did_output_sample_buffer(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            handle_sample_buffer(self, sample_buffer);
        }
    }
);

impl FrameDelegate {
    pub fn new(sender: Sender<Result<Frame, Error>>) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(DelegateIvars { sender });
        unsafe { msg_send![super(allocated), init] }
    }

    pub fn as_protocol(&self) -> &ProtocolObject<dyn AVCaptureVideoDataOutputSampleBufferDelegate> {
        ProtocolObject::from_ref(self)
    }
}

fn handle_sample_buffer(delegate: &FrameDelegate, sample_buffer: &CMSampleBuffer) {
    let Some(pixel_buffer) = (unsafe { sample_buffer.image_buffer() }) else {
        return;
    };

    unsafe {
        CVPixelBufferLockBaseAddress(&pixel_buffer, CVPixelBufferLockFlags::ReadOnly);
    }

    let width = CVPixelBufferGetWidth(&pixel_buffer) as u32;
    let height = CVPixelBufferGetHeight(&pixel_buffer) as u32;
    let stride = CVPixelBufferGetBytesPerRow(&pixel_buffer) as u32;
    let base = CVPixelBufferGetBaseAddress(&pixel_buffer);

    let mut data = Vec::new();
    if !base.is_null() {
        let byte_count = stride as usize * height as usize;
        data = unsafe { std::slice::from_raw_parts(base as *const u8, byte_count) }.to_vec();
    }

    unsafe {
        CVPixelBufferUnlockBaseAddress(&pixel_buffer, CVPixelBufferLockFlags::ReadOnly);
    }

    let timestamp = cmtime_to_duration(unsafe { sample_buffer.presentation_time_stamp() });

    let frame = Frame {
        width,
        height,
        stride,
        timestamp,
        pixel_format: PixelFormat::Bgra8,
        plane_primary: Bytes::from(data),
        plane_secondary: Bytes::new(),
    };

    let _ = delegate.ivars().sender.try_send(Ok(frame));
}

fn cmtime_to_duration(time: objc2_core_media::CMTime) -> Duration {
    if time.timescale == 0 {
        return Duration::ZERO;
    }
    let seconds = time.value as f64 / time.timescale as f64;
    if seconds.is_sign_negative() || !seconds.is_finite() {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(seconds)
}
