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

        let decoder = context.decoder().video()
            .context(format!("Failed to open {} decoder", name))?;

        log::info!("{} decoder initialized (software)", name);

        Ok(Self {
            decoder,
            frame_count: 0,
            codec_name: name.to_string(),
        })
    }

    /// Decode an Annex B frame. May return 0+ decoded frames.
    pub fn decode(&mut self, data: &[u8], timestamp_us: u64) -> Result<Vec<DecodedFrame>> {
        let packet = ffmpeg_next::Packet::copy(data);
        self.decoder.send_packet(&packet).context("send_packet failed")?;

        let mut frames = Vec::new();
        let mut decoded = ffmpeg_next::frame::Video::empty();

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            self.frame_count += 1;
            let w = decoded.width();
            let h = decoded.height();

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

// Legacy alias for backward compatibility
pub type H264Decoder = VideoDecoder;
