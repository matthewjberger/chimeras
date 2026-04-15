//! Windows Media Foundation hardware H.264 / H.265 decoder.
//!
//! Instantiates the OS-bundled `IMFTransform` for the requested codec (H.264 / HEVC),
//! configures input and output media types, and runs a `ProcessInput` / `ProcessOutput`
//! pump. Output is requested as NV12 because every MF hardware decoder supports it;
//! we convert NV12 -> BGRA using the shared converter in `crate::convert`.

use super::{VideoCodec, VideoDecoder};
use crate::error::Error;
use crate::types::{Frame, FrameQuality, PixelFormat};
use bytes::Bytes;
use std::time::Duration;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::GUID;

/// Media Foundation decoder instance.
pub(crate) struct MediaFoundationDecoder {
    transform: IMFTransform,
    output_type: IMFMediaType,
    width: u32,
    height: u32,
    stride: u32,
    #[allow(dead_code)]
    codec: VideoCodec,
    mf_started: bool,
}

unsafe impl Send for MediaFoundationDecoder {}

impl VideoDecoder for MediaFoundationDecoder {
    fn new(codec: VideoCodec, extradata: &[u8]) -> Result<Self, Error> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(map_error)?;
        }

        let subtype = match codec {
            VideoCodec::H264 => MFVideoFormat_H264,
            VideoCodec::H265 => MFVideoFormat_HEVC,
        };

        let transform = activate_decoder(subtype)?;
        let (input_type, width, height) = build_input_type(subtype, extradata)?;
        unsafe {
            transform
                .SetInputType(0, &input_type, 0)
                .map_err(map_error)?;
        }

        let output_type = select_output_type(&transform, width, height)?;
        unsafe {
            transform
                .SetOutputType(0, &output_type, 0)
                .map_err(map_error)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(map_error)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(map_error)?;
        }

        let stride = nv12_stride(width);

        Ok(Self {
            transform,
            output_type,
            width,
            height,
            stride,
            codec,
            mf_started: true,
        })
    }

    fn decode(&mut self, nal: &[u8], timestamp: Duration) -> Result<Vec<Frame>, Error> {
        let input_sample = build_input_sample(nal, timestamp)?;
        unsafe {
            self.transform
                .ProcessInput(0, &input_sample, 0)
                .map_err(map_error)?;
        }

        let mut frames = Vec::new();
        while let Some(frame) = self.pull_output()? {
            frames.push(frame);
        }
        Ok(frames)
    }
}

impl MediaFoundationDecoder {
    fn pull_output(&mut self) -> Result<Option<Frame>, Error> {
        let info = unsafe { self.transform.GetOutputStreamInfo(0) }.map_err(map_error)?;
        let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        let pre_allocated = if provides_samples {
            None
        } else {
            Some(create_empty_output_sample(self.width, self.height)?)
        };

        let mut buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(pre_allocated),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        };

        let mut status = 0u32;
        let hr = unsafe {
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
        };

        match hr {
            Ok(()) => {
                let decoded = unsafe { std::mem::ManuallyDrop::take(&mut buffer.pSample) };
                let Some(sample) = decoded else {
                    return Ok(None);
                };
                let bgra = sample_to_bgra(&sample, self.width, self.height, self.stride)?;
                Ok(Some(Frame {
                    width: self.width,
                    height: self.height,
                    stride: self.width * 4,
                    timestamp: Duration::ZERO,
                    pixel_format: PixelFormat::Bgra8,
                    quality: FrameQuality::Intact,
                    plane_primary: Bytes::from(bgra),
                    plane_secondary: Bytes::new(),
                }))
            }
            Err(error) if error.code().0 == MF_E_TRANSFORM_NEED_MORE_INPUT.0 => Ok(None),
            Err(error) if error.code().0 == MF_E_TRANSFORM_STREAM_CHANGE.0 => {
                self.reconfigure_output()?;
                Ok(None)
            }
            Err(error) => Err(Error::Backend {
                platform: "windows",
                message: format!("ProcessOutput failed: {}", error.message()),
            }),
        }
    }

    fn reconfigure_output(&mut self) -> Result<(), Error> {
        let mut index = 0u32;
        loop {
            let next = unsafe { self.transform.GetOutputAvailableType(0, index) };
            match next {
                Ok(media_type) => {
                    let Ok(subtype) = (unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }) else {
                        index += 1;
                        continue;
                    };
                    if subtype == MFVideoFormat_NV12 {
                        unsafe {
                            self.transform
                                .SetOutputType(0, &media_type, 0)
                                .map_err(map_error)?;
                        }
                        let packed =
                            unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE).map_err(map_error)? };
                        self.width = (packed >> 32) as u32;
                        self.height = (packed & 0xFFFF_FFFF) as u32;
                        self.stride = nv12_stride(self.width);
                        self.output_type = media_type;
                        return Ok(());
                    }
                    index += 1;
                }
                Err(_) => break,
            }
        }
        Err(Error::Backend {
            platform: "windows",
            message: "no NV12 output type available on stream change".into(),
        })
    }
}

impl Drop for MediaFoundationDecoder {
    fn drop(&mut self) {
        if self.mf_started {
            unsafe {
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
                let _ = MFShutdown();
            }
        }
    }
}

fn activate_decoder(subtype: GUID) -> Result<IMFTransform, Error> {
    let info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&info),
            None,
            &mut activates,
            &mut count,
        )
        .map_err(map_error)?;
    }
    if count == 0 {
        return Err(Error::Backend {
            platform: "windows",
            message: "no Media Foundation decoder registered".into(),
        });
    }
    let first = unsafe { (*activates).clone() }.ok_or(Error::Backend {
        platform: "windows",
        message: "null IMFActivate".into(),
    })?;
    let transform: IMFTransform = unsafe { first.ActivateObject() }.map_err(map_error)?;
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _)) };
    Ok(transform)
}

fn build_input_type(subtype: GUID, _extradata: &[u8]) -> Result<(IMFMediaType, u32, u32), Error> {
    let media_type = unsafe { MFCreateMediaType().map_err(map_error)? };
    unsafe {
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(map_error)?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &subtype)
            .map_err(map_error)?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(map_error)?;
    }
    Ok((media_type, 0, 0))
}

fn select_output_type(
    transform: &IMFTransform,
    _width: u32,
    _height: u32,
) -> Result<IMFMediaType, Error> {
    let mut index = 0u32;
    loop {
        let next = unsafe { transform.GetOutputAvailableType(0, index) };
        match next {
            Ok(media_type) => {
                let Ok(subtype) = (unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }) else {
                    index += 1;
                    continue;
                };
                if subtype == MFVideoFormat_NV12 {
                    return Ok(media_type);
                }
                index += 1;
            }
            Err(_) => break,
        }
    }
    Err(Error::Backend {
        platform: "windows",
        message: "no NV12 output type advertised by decoder".into(),
    })
}

fn build_input_sample(nal: &[u8], timestamp: Duration) -> Result<IMFSample, Error> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(nal.len() as u32).map_err(map_error)?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(map_error)?;
        std::ptr::copy_nonoverlapping(nal.as_ptr(), data_ptr, nal.len());
        buffer
            .SetCurrentLength(nal.len() as u32)
            .map_err(map_error)?;
        let _ = buffer.Unlock();

        let sample = MFCreateSample().map_err(map_error)?;
        sample.AddBuffer(&buffer).map_err(map_error)?;
        sample
            .SetSampleTime(timestamp.as_nanos() as i64 / 100)
            .map_err(map_error)?;
        Ok(sample)
    }
}

fn create_empty_output_sample(width: u32, height: u32) -> Result<IMFSample, Error> {
    let frame_size = nv12_frame_size(width, height);
    unsafe {
        let buffer = MFCreateMemoryBuffer(frame_size).map_err(map_error)?;
        let sample = MFCreateSample().map_err(map_error)?;
        sample.AddBuffer(&buffer).map_err(map_error)?;
        Ok(sample)
    }
}

fn sample_to_bgra(
    sample: &IMFSample,
    width: u32,
    height: u32,
    stride: u32,
) -> Result<Vec<u8>, Error> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer().map_err(map_error)? };
    let mut base_ptr: *mut u8 = std::ptr::null_mut();
    let mut max_length = 0u32;
    let mut current_length = 0u32;
    unsafe {
        buffer
            .Lock(
                &mut base_ptr,
                Some(&mut max_length),
                Some(&mut current_length),
            )
            .map_err(map_error)?;
    }

    let width_usize = width as usize;
    let height_usize = height as usize;
    let y_stride = stride as usize;
    let y_size = y_stride * height_usize;
    let uv_size = y_stride * (height_usize / 2);
    let total = y_size + uv_size;

    let mut bgra = vec![0u8; width_usize * height_usize * 4];
    if !base_ptr.is_null() && (current_length as usize) >= total {
        let slice = unsafe { std::slice::from_raw_parts(base_ptr, total) };
        let (y_plane, uv_plane) = slice.split_at(y_size);
        nv12_to_bgra(
            y_plane,
            uv_plane,
            y_stride,
            width_usize,
            height_usize,
            &mut bgra,
        );
    }

    unsafe {
        let _ = buffer.Unlock();
    }
    Ok(bgra)
}

fn nv12_to_bgra(
    y_plane: &[u8],
    uv_plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    bgra: &mut [u8],
) {
    for row in 0..height {
        for col in 0..width {
            let y = y_plane[row * stride + col] as i32;
            let uv_row = row / 2;
            let uv_col = (col / 2) * 2;
            let uv_index = uv_row * stride + uv_col;
            let u = uv_plane[uv_index] as i32 - 128;
            let v = uv_plane[uv_index + 1] as i32 - 128;
            let c = y - 16;
            let r = ((298 * c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * u + 128) >> 8).clamp(0, 255) as u8;
            let base = (row * width + col) * 4;
            bgra[base] = b;
            bgra[base + 1] = g;
            bgra[base + 2] = r;
            bgra[base + 3] = 255;
        }
    }
}

fn nv12_stride(width: u32) -> u32 {
    (width + 15) & !15
}

fn nv12_frame_size(width: u32, height: u32) -> u32 {
    let stride = nv12_stride(width);
    stride * height + stride * (height / 2)
}

fn map_error(error: windows::core::Error) -> Error {
    Error::Backend {
        platform: "windows",
        message: error.message().to_string(),
    }
}
