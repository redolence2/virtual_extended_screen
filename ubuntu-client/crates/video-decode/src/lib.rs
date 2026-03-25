// Phase 4: H.264 decode via ffmpeg-next (VAAPI + SW fallback)
// Stub for now — only the interface.

/// A decoded video frame (NV12 pixel data).
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // NV12 pixel data
    pub timestamp_us: u64,
}
