use anyhow::{Context, Result};
use std::time::Instant;

/// A decoded video frame with YUV420P pixel data.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    /// This *exact emitted frame's* own recovered identity
    /// (A00_REMEDIATION_PLAN.md §4 item 7: "every emitted decoded frame
    /// recovers its own PTS; delayed and multi-output emissions never
    /// inherit the current decode call's ID"). Read from the AVFrame's
    /// `pts()`, falling back to `best_effort_timestamp()` (`timestamp()`)
    /// when `pts` is unset; `None` when neither is available.
    pub recovered_frame_id: Option<u64>,
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
            // LOW_DELAY before open zeroes CUVID's default 4-frame display
            // queue (ulMaxDisplayDelay) — measured as a 67ms standing
            // latency (99.8% of frames at exactly gap 4 in the 2026-08-05
            // demo trace). Mirrors backend-construct's characterized
            // cuvid-lowdelay configuration, extra_hw_frames included.
            (*context.as_mut_ptr()).flags |= ffmpeg_sys_next::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*context.as_mut_ptr()).extra_hw_frames = 8;
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
    ///
    /// `frame_id` is the wire `frameID` for this Annex B unit — set as the
    /// submitted packet's PTS (A00_REMEDIATION_PLAN.md §4 item 7: "the
    /// decoder packet PTS is set to frameID"). It is *not* the identity of
    /// any particular emitted frame: with B-frames/reordering one `decode()`
    /// call can emit 0, 1, or several frames, and each recovers its own PTS
    /// independently in [`Self::drain_ready`] — never this call's `frame_id`.
    pub fn decode(&mut self, data: &[u8], frame_id: u32, timestamp_us: u64, is_keyframe: bool) -> Result<Vec<DecodedFrame>> {
        if self.state == DecoderState::WaitingForIDR && !is_keyframe {
            return Ok(Vec::new());
        }

        let mut packet = ffmpeg_next::Packet::copy(data);
        packet.set_pts(Some(frame_id as i64));
        if let Err(e) = self.decoder.send_packet(&packet) {
            self.enter_waiting_for_idr(IDRReason::DecodeError);
            return Err(anyhow::anyhow!("send_packet failed: {}", e));
        }

        Ok(self.drain_ready(timestamp_us, is_keyframe))
    }

    /// End-of-stream flush (C3; A00_COMPLETION_REPORT_AMENDED_review.md
    /// finding 1 correction 4: "add delayed-output, multi-output,
    /// zero-output-then-later-output ... and unresolved-tail negative
    /// tests"). Signals EOF (`send_eof`, ffmpeg-next's wrapper for
    /// `avcodec_send_packet(ctx, NULL)`) and drains every remaining buffered
    /// frame through the identical [`Self::drain_ready`] path `decode()`
    /// uses, so a tail emission recovers its own `recovered_frame_id`
    /// exactly like decode() — never a synthesized or inherited identity.
    ///
    /// `send_eof` can itself EAGAIN exactly as `send_packet` can (ffmpeg's
    /// contract: internal output buffers are full, drain before
    /// resubmitting) — handled here by draining and retrying, bounded by
    /// `MAX_EOF_EAGAIN_ATTEMPTS`, mirroring the real, characterized
    /// retain/drain/resubmit discipline `crates/backend-construct/src/
    /// loop_engine.rs`'s `flush_tail` already proves against the real
    /// backends (`decoder-experiment --characterize`). `Err(Error::Eof)`
    /// from `send_eof` means a prior flush already fully signalled EOF —
    /// treated as accepted (idempotent), matching `flush_tail`'s handling.
    /// Used only by the client's trace-mode clean-shutdown path
    /// (`ubuntu-client/src/main.rs`).
    pub fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        // Matches decoder-experiment's MAX_EAGAIN_CYCLES discipline
        // (crates/backend-construct/src/loop_engine.rs) — bounded so a
        // backend that never converges is a fatal Err, not a true hang.
        const MAX_EOF_EAGAIN_ATTEMPTS: u32 = 64;

        let mut frames = Vec::new();
        let mut eagain_attempts: u32 = 0;
        loop {
            match self.decoder.send_eof() {
                Ok(()) => break,
                Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                    eagain_attempts += 1;
                    if eagain_attempts > MAX_EOF_EAGAIN_ATTEMPTS {
                        anyhow::bail!(
                            "send_eof EAGAIN did not converge after {} attempts",
                            eagain_attempts
                        );
                    }
                    // No wire packet at EOF: is_keyframe=false (see
                    // drain_ready's doc comment).
                    frames.extend(self.drain_ready(0, false));
                    // loop back and retry the identical EOF signal
                }
                Err(ffmpeg_next::Error::Eof) => break, // already flushed; idempotent
                Err(e) => anyhow::bail!("send_eof failed: {}", e),
            }
        }

        frames.extend(self.drain_ready(0, false));
        Ok(frames)
    }

    /// Drains every frame currently ready from `receive_frame()`, applying
    /// the exact same per-emission processing decode calls always have:
    /// GPU→CPU transfer / corrupt-frame rejection, gray-frame detection, the
    /// IDR-recovery state machine, and independent PTS recovery per emission
    /// (never inherited from whatever call triggered the drain). Shared by
    /// [`Self::decode`] and [`Self::flush`] — required by
    /// A00_COMPLETION_REPORT_AMENDED_review.md finding 1's "each with its
    /// own recovered_frame_id exactly like decode()" — so the two paths can
    /// never drift apart.
    ///
    /// `timestamp_us`/`is_keyframe` feed [`DecodedFrame::timestamp_us`] and
    /// the per-emission state-machine check below exactly as `decode()`
    /// always has. `flush()` has no corresponding wire packet at EOF, so it
    /// passes `timestamp_us=0` (unused by the identity ledger/trace path,
    /// which keys exclusively off `recovered_frame_id`) and
    /// `is_keyframe=false` (there is no new keyframe at EOF — only
    /// already-buffered output draining out; this only means a stray
    /// `WaitingForIDR` state can't be flipped to `Recovering` by a tail
    /// emission, which is the conservative choice for a path that runs only
    /// immediately before process exit).
    fn drain_ready(&mut self, timestamp_us: u64, is_keyframe: bool) -> Vec<DecodedFrame> {
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

            // A00_REMEDIATION_PLAN.md §4 item 7: recover THIS emission's own
            // PTS — never the current decode() call's `frame_id` argument.
            // Read from `decoded` (the raw receive_frame() emission), not
            // `cpu_frame`: for CUVID, `cpu_frame` is a freshly allocated
            // `sw_frame` populated by `av_hwframe_transfer_data`, which
            // copies pixel data only — it does not carry `pts` across the
            // transfer. `decoded` is re-populated fresh by `receive_frame`
            // each loop iteration, so this is always the current emission's
            // own identity, correct for delayed/multi-output emissions.
            let recovered_frame_id = decoded.pts().or_else(|| decoded.timestamp()).map(|v| v as u64);

            // Extract YUV planes — handle both I420 (software) and NV12 (CUVID)
            let frame = self.extract_yuv(&cpu_frame, timestamp_us, recovered_frame_id);
            frames.push(frame);
        }

        frames
    }

    /// Extract YUV420P planes from decoded frame.
    /// Handles I420 (software) and NV12 (CUVID GPU→CPU transfer) formats.
    fn extract_yuv(&self, frame: &ffmpeg_next::frame::Video, timestamp_us: u64, recovered_frame_id: Option<u64>) -> DecodedFrame {
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
            recovered_frame_id,
            planes: [y_data, u_data, v_data],
            strides: [y_stride, u_stride, v_stride],
        }
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
    pub fn codec_name(&self) -> &str { &self.codec_name }
    pub fn backend(&self) -> DecodeBackend { self.backend }
}

pub type H264Decoder = VideoDecoder;
