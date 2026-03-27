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

/// Decoder recovery state machine (Item 7 from review).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecoderState {
    /// Normal operation — decoding all frames.
    Healthy,
    /// Lost reference frame. Dropping non-keyframes, waiting for IDR.
    WaitingForIDR,
    /// Got a keyframe after WaitingForIDR, verifying stability.
    Recovering,
}

/// Reason for requesting an IDR from the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IDRReason {
    DecodeError,
    CorruptFrame,
    ReferenceLoss,
}

/// Video decoder supporting H.264 and HEVC via ffmpeg/libavcodec.
pub struct VideoDecoder {
    decoder: ffmpeg_next::decoder::Video,
    frame_count: u64,
    codec_name: String,
    /// Recovery state machine.
    pub state: DecoderState,
    /// Last time we requested an IDR (rate limiting).
    last_idr_request: Option<Instant>,
    /// Pending IDR request reason (consumed by caller).
    pub pending_idr_reason: Option<IDRReason>,
    /// Frames decoded since last state change (for Recovering → Healthy).
    frames_since_recovery: u32,
}

impl VideoDecoder {
    /// Create a decoder for the specified codec (0=H.264, 1=HEVC).
    pub fn new(codec_id: u8) -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg init")?;

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
            count: 2,
            ..Default::default()
        });

        let mut decoder = context.decoder().video()
            .context(format!("Failed to open {} decoder", name))?;

        // Disable error concealment — don't output gray frames on decode errors.
        unsafe {
            (*decoder.as_mut_ptr()).error_concealment = 0;
        }

        log::info!("{} decoder initialized (software, no error concealment)", name);

        Ok(Self {
            decoder,
            frame_count: 0,
            codec_name: name.to_string(),
            state: DecoderState::Healthy,
            last_idr_request: None,
            pending_idr_reason: None,
            frames_since_recovery: 0,
        })
    }

    /// Request an IDR if rate limit allows (250ms between requests).
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

    /// Transition to WaitingForIDR state.
    fn enter_waiting_for_idr(&mut self, reason: IDRReason) {
        if self.state != DecoderState::WaitingForIDR {
            log::warn!("Decoder → WaitingForIDR (was {:?}, reason: {:?})", self.state, reason);
            self.state = DecoderState::WaitingForIDR;
        }
        self.request_idr(reason);
    }

    /// Decode an Annex B frame. May return 0+ decoded frames.
    /// In WaitingForIDR state, non-keyframes are skipped entirely.
    pub fn decode(&mut self, data: &[u8], timestamp_us: u64, is_keyframe: bool) -> Result<Vec<DecodedFrame>> {
        // In WaitingForIDR: skip non-keyframes (don't feed to decoder — broken references)
        if self.state == DecoderState::WaitingForIDR && !is_keyframe {
            return Ok(Vec::new());
        }

        // Feed frame to decoder
        let packet = ffmpeg_next::Packet::copy(data);
        if let Err(e) = self.decoder.send_packet(&packet) {
            self.enter_waiting_for_idr(IDRReason::DecodeError);
            return Err(anyhow::anyhow!("send_packet failed: {}", e));
        }

        let mut frames = Vec::new();
        let mut decoded = ffmpeg_next::frame::Video::empty();

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let w = decoded.width();
            let h = decoded.height();

            // Skip frames with decode errors
            let is_corrupt = unsafe { (*decoded.as_ptr()).decode_error_flags != 0 };
            if is_corrupt {
                self.enter_waiting_for_idr(IDRReason::CorruptFrame);
                continue;
            }

            // Skip gray concealment frames: sample Y plane variance
            let y_data = decoded.data(0);
            let y_stride = decoded.stride(0);
            if w > 0 && h > 0 && !y_data.is_empty() {
                let mut sum: u64 = 0;
                let mut sum_sq: u64 = 0;
                let samples = 16usize;
                for i in 0..samples {
                    let row = (i * h as usize / samples).min(h as usize - 1);
                    let col = (i * w as usize / samples).min(w as usize - 1);
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

            // Good frame — update state
            if is_keyframe && self.state == DecoderState::WaitingForIDR {
                log::info!("Decoder → Recovering (keyframe received)");
                self.state = DecoderState::Recovering;
                self.frames_since_recovery = 0;
            }

            if self.state == DecoderState::Recovering {
                self.frames_since_recovery += 1;
                if self.frames_since_recovery >= 5 {
                    log::info!("Decoder → Healthy (5 clean frames after recovery)");
                    self.state = DecoderState::Healthy;
                }
            }

            self.frame_count += 1;

            let y_stride = decoded.stride(0);
            let u_stride = decoded.stride(1);
            let v_stride = decoded.stride(2);

            let y_data = decoded.data(0)[..y_stride * h as usize].to_vec();
            let u_data = decoded.data(1)[..u_stride * (h as usize / 2)].to_vec();
            let v_data = decoded.data(2)[..v_stride * (h as usize / 2)].to_vec();

            frames.push(DecodedFrame {
                width: w, height: h, timestamp_us,
                planes: [y_data, u_data, v_data],
                strides: [y_stride, u_stride, v_stride],
            });
        }

        Ok(frames)
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
    pub fn codec_name(&self) -> &str { &self.codec_name }
}

// Legacy alias
pub type H264Decoder = VideoDecoder;
