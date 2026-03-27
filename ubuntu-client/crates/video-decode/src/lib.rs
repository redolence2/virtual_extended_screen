use anyhow::{Context, Result};
use std::time::Instant;

/// A decoded video frame with YUV420P pixel data.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    pub planes: [Vec<u8>; 3],  // Y, U, V
    pub strides: [usize; 3],
}

/// Decoder recovery state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecoderState {
    Healthy,
    WaitingForIDR,
    Recovering,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IDRReason {
    DecodeError,
    CorruptFrame,
    ReferenceLoss,
}

/// Whether hardware or software decode is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecodeBackend {
    Software,
    Cuvid, // NVDEC via CUVID
}

/// Video decoder supporting H.264 and HEVC.
/// Tries NVDEC (CUVID) first, falls back to software.
pub struct VideoDecoder {
    decoder: ffmpeg_next::decoder::Video,
    /// Raw CUDA device context (owned, must be freed on drop).
    hw_device_ctx: Option<*mut ffmpeg_sys_next::AVBufferRef>,
    backend: DecodeBackend,
    frame_count: u64,
    codec_name: String,
    pub state: DecoderState,
    last_idr_request: Option<Instant>,
    pub pending_idr_reason: Option<IDRReason>,
    frames_since_recovery: u32,
}

// SAFETY: VideoDecoder is only used from one thread (decode-render).
unsafe impl Send for VideoDecoder {}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        if let Some(ref mut ctx) = self.hw_device_ctx {
            unsafe { ffmpeg_sys_next::av_buffer_unref(ctx); }
        }
    }
}

impl VideoDecoder {
    /// Create decoder: tries NVDEC (CUVID) first, falls back to software.
    pub fn new(codec_id: u8) -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg init")?;

        // Try CUVID (NVDEC) hardware decoder
        match Self::new_cuvid(codec_id) {
            Ok(d) => return Ok(d),
            Err(e) => log::info!("CUVID not available ({}), using software decode", e),
        }

        Self::new_software(codec_id)
    }

    fn new_cuvid(codec_id: u8) -> Result<Self> {
        let (decoder_name, display_name) = match codec_id {
            0 => ("h264_cuvid\0", "H.264 CUVID"),
            1 => ("hevc_cuvid\0", "HEVC CUVID"),
            _ => anyhow::bail!("Unknown codec ID: {}", codec_id),
        };

        // Create CUDA hardware device context
        let mut hw_device_ctx: *mut ffmpeg_sys_next::AVBufferRef = std::ptr::null_mut();
        let ret = unsafe {
            ffmpeg_sys_next::av_hwdevice_ctx_create(
                &mut hw_device_ctx,
                ffmpeg_sys_next::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            anyhow::bail!("CUDA device init failed (ret={})", ret);
        }

        // Find CUVID decoder by name
        let codec_ptr = unsafe {
            ffmpeg_sys_next::avcodec_find_decoder_by_name(decoder_name.as_ptr() as *const i8)
        };
        if codec_ptr.is_null() {
            unsafe { ffmpeg_sys_next::av_buffer_unref(&mut hw_device_ctx); }
            anyhow::bail!("{} decoder not found", display_name);
        }

        // Create context and set CUDA device
        let codec = unsafe { ffmpeg_next::codec::codec::Codec::wrap(codec_ptr as *mut _) };
        let mut context = ffmpeg_next::codec::context::Context::new_with_codec(codec);

        unsafe {
            (*context.as_mut_ptr()).hw_device_ctx = ffmpeg_sys_next::av_buffer_ref(hw_device_ctx);
        }

        let decoder = context.decoder().video()
            .context(format!("Failed to open {} decoder", display_name))?;

        log::info!("{} decoder initialized (NVDEC hardware, RTX GPU)", display_name);

        Ok(Self {
            decoder,
            hw_device_ctx: Some(hw_device_ctx),
            backend: DecodeBackend::Cuvid,
            frame_count: 0,
            codec_name: display_name.to_string(),
            state: DecoderState::Healthy,
            last_idr_request: None,
            pending_idr_reason: None,
            frames_since_recovery: 0,
        })
    }

    fn new_software(codec_id: u8) -> Result<Self> {
        let (ff_codec_id, name) = match codec_id {
            0 => (ffmpeg_next::codec::Id::H264, "H.264"),
            1 => (ffmpeg_next::codec::Id::HEVC, "HEVC"),
            _ => anyhow::bail!("Unknown codec ID: {}", codec_id),
        };

        let codec = ffmpeg_next::decoder::find(ff_codec_id)
            .context(format!("{} codec not found", name))?;

        let mut context = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        context.set_threading(ffmpeg_next::threading::Config {
            kind: ffmpeg_next::threading::Type::Frame,
            count: 4, // Increased from 2 for better 4K performance
            ..Default::default()
        });

        let mut decoder = context.decoder().video()
            .context(format!("Failed to open {} decoder", name))?;

        unsafe {
            (*decoder.as_mut_ptr()).error_concealment = 0;
        }

        log::info!("{} decoder initialized (software, 4 threads)", name);

        Ok(Self {
            decoder,
            hw_device_ctx: None,
            backend: DecodeBackend::Software,
            frame_count: 0,
            codec_name: name.to_string(),
            state: DecoderState::Healthy,
            last_idr_request: None,
            pending_idr_reason: None,
            frames_since_recovery: 0,
        })
    }

    fn request_idr(&mut self, reason: IDRReason) {
        let now = Instant::now();
        let can_request = match self.last_idr_request {
            Some(last) => now.duration_since(last).as_millis() >= 250,
            None => true,
        };
        if can_request {
            self.pending_idr_reason = Some(reason);
            self.last_idr_request = Some(now);
            log::warn!("Requesting IDR: {:?} (state: {:?})", reason, self.state);
        }
    }

    fn enter_waiting_for_idr(&mut self, reason: IDRReason) {
        if self.state != DecoderState::WaitingForIDR {
            log::warn!("Decoder → WaitingForIDR (was {:?}, reason: {:?})", self.state, reason);
            self.state = DecoderState::WaitingForIDR;
        }
        self.request_idr(reason);
    }

    /// Decode an Annex B frame. Returns 0+ decoded frames.
    pub fn decode(&mut self, data: &[u8], timestamp_us: u64, is_keyframe: bool) -> Result<Vec<DecodedFrame>> {
        if self.state == DecoderState::WaitingForIDR && !is_keyframe {
            return Ok(Vec::new());
        }

        let packet = ffmpeg_next::Packet::copy(data);
        if let Err(e) = self.decoder.send_packet(&packet) {
            self.enter_waiting_for_idr(IDRReason::DecodeError);
            return Err(anyhow::anyhow!("send_packet failed: {}", e));
        }

        let mut frames = Vec::new();
        let mut decoded = ffmpeg_next::frame::Video::empty();

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            // Check if frame is in GPU memory (CUVID) and transfer to CPU
            let cpu_frame = if self.backend == DecodeBackend::Cuvid {
                let mut sw_frame = ffmpeg_next::frame::Video::empty();
                let ret = unsafe {
                    ffmpeg_sys_next::av_hwframe_transfer_data(
                        sw_frame.as_mut_ptr(),
                        decoded.as_ptr(),
                        0,
                    )
                };
                if ret < 0 {
                    log::warn!("GPU→CPU transfer failed (ret={})", ret);
                    self.enter_waiting_for_idr(IDRReason::DecodeError);
                    continue;
                }
                sw_frame
            } else {
                // Software: frame is already in CPU memory
                // Check for corruption
                let is_corrupt = unsafe { (*decoded.as_ptr()).decode_error_flags != 0 };
                if is_corrupt {
                    self.enter_waiting_for_idr(IDRReason::CorruptFrame);
                    continue;
                }
                decoded.clone()
            };

            let w = cpu_frame.width() as usize;
            let h = cpu_frame.height() as usize;
            if w == 0 || h == 0 { continue; }

            // Gray frame detection (software only — CUVID doesn't produce concealment frames)
            if self.backend == DecodeBackend::Software {
                let y_data = cpu_frame.data(0);
                let y_stride = cpu_frame.stride(0);
                if !y_data.is_empty() {
                    let mut sum: u64 = 0;
                    let mut sum_sq: u64 = 0;
                    let samples = 16usize;
                    for i in 0..samples {
                        let row = (i * h / samples).min(h - 1);
                        let col = (i * w / samples).min(w - 1);
                        let val = y_data[row * y_stride + col] as u64;
                        sum += val;
                        sum_sq += val * val;
                    }
                    let mean = sum / samples as u64;
                    let variance = sum_sq / samples as u64 - mean * mean;
                    if variance < 4 && mean > 100 && mean < 160 {
                        self.enter_waiting_for_idr(IDRReason::ReferenceLoss);
                        continue;
                    }
                }
            }

            // State machine updates
            if is_keyframe && self.state == DecoderState::WaitingForIDR {
                log::info!("Decoder → Recovering (keyframe received)");
                self.state = DecoderState::Recovering;
                self.frames_since_recovery = 0;
            }
            if self.state == DecoderState::Recovering {
                self.frames_since_recovery += 1;
                if self.frames_since_recovery >= 5 {
                    log::info!("Decoder → Healthy (5 clean frames)");
                    self.state = DecoderState::Healthy;
                }
            }

            self.frame_count += 1;

            // Extract YUV planes — handle both I420 (software) and NV12 (CUVID)
            let frame = self.extract_yuv(&cpu_frame, timestamp_us);
            frames.push(frame);
        }

        Ok(frames)
    }

    /// Extract YUV420P planes from decoded frame.
    /// Handles I420 (software) and NV12 (CUVID GPU→CPU transfer) formats.
    fn extract_yuv(&self, frame: &ffmpeg_next::frame::Video, timestamp_us: u64) -> DecodedFrame {
        let w = frame.width() as usize;
        let h = frame.height() as usize;
        let pix_fmt = frame.format();

        let is_nv12 = pix_fmt == ffmpeg_next::format::Pixel::NV12;

        let y_stride = frame.stride(0);
        let y_data: Vec<u8> = frame.data(0)[..y_stride * h].to_vec();

        let (u_data, v_data, u_stride, v_stride) = if is_nv12 {
            // NV12: plane 1 has interleaved UV (UVUVUV...)
            let uv_stride = frame.stride(1);
            let uv_data = frame.data(1);
            let half_w = w / 2;
            let half_h = h / 2;
            let mut u = vec![0u8; half_w * half_h];
            let mut v = vec![0u8; half_w * half_h];
            for row in 0..half_h {
                let src = &uv_data[row * uv_stride..row * uv_stride + w];
                for col in 0..half_w {
                    u[row * half_w + col] = src[col * 2];
                    v[row * half_w + col] = src[col * 2 + 1];
                }
            }
            (u, v, half_w, half_w)
        } else {
            // I420: separate U and V planes
            let u_stride = frame.stride(1);
            let v_stride = frame.stride(2);
            let u = frame.data(1)[..u_stride * (h / 2)].to_vec();
            let v = frame.data(2)[..v_stride * (h / 2)].to_vec();
            (u, v, u_stride, v_stride)
        };

        DecodedFrame {
            width: w as u32,
            height: h as u32,
            timestamp_us,
            planes: [y_data, u_data, v_data],
            strides: [y_stride, u_stride, v_stride],
        }
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
    pub fn codec_name(&self) -> &str { &self.codec_name }
    pub fn backend(&self) -> DecodeBackend { self.backend }
}

pub type H264Decoder = VideoDecoder;
