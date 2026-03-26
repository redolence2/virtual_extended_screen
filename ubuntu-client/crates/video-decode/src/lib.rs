use anyhow::{Context, Result};

/// A decoded video frame with YUV420P pixel data.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    /// YUV420P planes: [Y, U, V]
    pub planes: [Vec<u8>; 3],
    /// Stride (bytes per row) for each plane
    pub strides: [usize; 3],
}

/// H.264 software decoder using ffmpeg/libavcodec.
pub struct H264Decoder {
    decoder: ffmpeg_next::decoder::Video,
    frame_count: u64,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg init")?;

        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .context("H.264 codec not found")?;

        let mut context = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        context.set_threading(ffmpeg_next::threading::Config {
            kind: ffmpeg_next::threading::Type::Frame,
            count: 2,
            ..Default::default()
        });

        let decoder = context.decoder().video()
            .context("Failed to open H.264 decoder")?;

        log::info!("H.264 decoder initialized (software)");

        Ok(Self { decoder, frame_count: 0 })
    }

    /// Decode an Annex B H.264 frame. May return 0 or more decoded frames
    /// (due to decoder buffering).
    pub fn decode(&mut self, data: &[u8], timestamp_us: u64) -> Result<Vec<DecodedFrame>> {
        let packet = ffmpeg_next::Packet::copy(data);

        self.decoder.send_packet(&packet)
            .context("send_packet failed")?;

        let mut frames = Vec::new();
        let mut decoded = ffmpeg_next::frame::Video::empty();

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            self.frame_count += 1;

            let w = decoded.width();
            let h = decoded.height();

            // Extract YUV420P planes
            let y_stride = decoded.stride(0);
            let u_stride = decoded.stride(1);
            let v_stride = decoded.stride(2);

            let y_data = decoded.data(0)[..y_stride * h as usize].to_vec();
            let u_data = decoded.data(1)[..u_stride * (h as usize / 2)].to_vec();
            let v_data = decoded.data(2)[..v_stride * (h as usize / 2)].to_vec();

            frames.push(DecodedFrame {
                width: w,
                height: h,
                timestamp_us,
                planes: [y_data, u_data, v_data],
                strides: [y_stride, u_stride, v_stride],
            });
        }

        Ok(frames)
    }

    /// Flush remaining frames from decoder buffer.
    pub fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        self.decoder.send_eof()?;
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
                width: w, height: h, timestamp_us: 0,
                planes: [y_data, u_data, v_data],
                strides: [y_stride, u_stride, v_stride],
            });
        }
        Ok(frames)
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
