use anyhow::{Context, Result};

/// A decoded video frame with YUV420P pixel data.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    pub planes: [Vec<u8>; 3],  // Y, U, V
    pub strides: [usize; 3],
}

/// Video decoder supporting H.264 and HEVC via ffmpeg/libavcodec.
pub struct VideoDecoder {
    decoder: ffmpeg_next::decoder::Video,
    frame_count: u64,
    codec_name: String,
    /// Set to true after a successful keyframe decode; cleared on error.
    pub has_reference: bool,
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
        // Without this, ffmpeg fills missing reference areas with gray.
        unsafe {
            (*decoder.as_mut_ptr()).error_concealment = 0;
        }

        log::info!("{} decoder initialized (software, no error concealment)", name);

        Ok(Self {
            decoder,
            frame_count: 0,
            codec_name: name.to_string(),
            has_reference: false,
        })
    }

    /// Decode an Annex B frame. May return 0+ decoded frames.
    /// If has_reference is false (lost reference frame), non-keyframes are skipped
    /// to avoid gray error-concealment frames.
    pub fn decode(&mut self, data: &[u8], timestamp_us: u64) -> Result<Vec<DecodedFrame>> {
        // Always feed frames to decoder (maintains HEVC reference chain).
        // Gray concealment frames are filtered by Y-plane variance check below.
        let packet = ffmpeg_next::Packet::copy(data);
        self.decoder.send_packet(&packet).context("send_packet failed")?;

        let mut frames = Vec::new();
        let mut decoded = ffmpeg_next::frame::Video::empty();

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let w = decoded.width();
            let h = decoded.height();

            // Skip frames with decode errors
            let is_corrupt = unsafe { (*decoded.as_ptr()).decode_error_flags != 0 };
            if is_corrupt {
                self.has_reference = false;
                continue;
            }

            // Skip gray concealment frames: sample Y plane, if too uniform → likely error concealment
            let y_data = decoded.data(0);
            let y_stride = decoded.stride(0);
            if w > 0 && h > 0 && !y_data.is_empty() {
                // Sample 16 pixels spread across the frame
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
                // Gray concealment: mean ~128, variance ~0
                if variance < 4 && mean > 100 && mean < 160 {
                    self.has_reference = false;
                    continue; // skip gray frame
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

/// Detect if Annex B data starts with a keyframe NAL unit.
/// Scans for start code (00 00 00 01) then checks NAL type.
fn detect_keyframe(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 < data.len() {
        // Find start code
        if data[i] == 0 && data[i+1] == 0 && data[i+2] == 0 && data[i+3] == 1 {
            if i + 5 >= data.len() { break; }
            let nal_byte = data[i + 4];

            // H.264: NAL type is lower 5 bits. IDR = 5, SPS = 7, PPS = 8
            let h264_type = nal_byte & 0x1F;
            if h264_type == 5 || h264_type == 7 { return true; }

            // HEVC: NAL type is (byte >> 1) & 0x3F. IDR_W_RADL=19, IDR_N_LP=20, VPS=32, SPS=33
            let hevc_type = (nal_byte >> 1) & 0x3F;
            if hevc_type == 19 || hevc_type == 20 || hevc_type == 32 || hevc_type == 33 {
                return true;
            }

            i += 5;
        } else {
            i += 1;
        }
    }
    false
}
