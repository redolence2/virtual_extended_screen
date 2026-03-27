/// Generated protobuf types for the RESC control protocol.
pub mod resc_control {
    include!(concat!(env!("OUT_DIR"), "/resc.control.rs"));
}

pub mod resc_cursor {
    include!(concat!(env!("OUT_DIR"), "/resc.cursor.rs"));
}

pub mod resc_input {
    include!(concat!(env!("OUT_DIR"), "/resc.input.rs"));
}

/// Protocol constants (v1). Must match Swift ProtocolConstants exactly.
pub mod constants {
    pub const PROTOCOL_VERSION: u8 = 1;
    pub const MAGIC: [u8; 4] = *b"RESC";

    pub const PACKET_TYPE_VIDEO_CHUNK: u8 = 0;
    pub const PACKET_TYPE_CURSOR_UPDATE: u8 = 1;
    pub const PACKET_TYPE_INPUT_EVENT: u8 = 2;

    pub const PACKET_PREFIX_BYTES: usize = 6;
    pub const VIDEO_CHUNK_HEADER_BYTES: usize = 36;  // per-packet(16) + per-frame(20)
    pub const VIDEO_TOTAL_HEADER_BYTES: usize = PACKET_PREFIX_BYTES + VIDEO_CHUNK_HEADER_BYTES; // 42
    pub const MAX_DATAGRAM_BYTES: usize = 1400;
    pub const MAX_VIDEO_PAYLOAD_BYTES: usize = MAX_DATAGRAM_BYTES - VIDEO_TOTAL_HEADER_BYTES; // 1358

    pub const CURSOR_UPDATE_BYTES: usize = 29;
    pub const CURSOR_TOTAL_PACKET_BYTES: usize = PACKET_PREFIX_BYTES + CURSOR_UPDATE_BYTES; // 35

    pub const INPUT_EVENT_BYTES: usize = 22;
    pub const INPUT_TOTAL_PACKET_BYTES: usize = PACKET_PREFIX_BYTES + INPUT_EVENT_BYTES; // 28

    pub const MDNS_SERVICE_TYPE: &str = "_remotedisplay._tcp.local.";

    /// Log and verify all constants at startup. Panics on self-inconsistency.
    pub fn log_and_verify() {
        log::info!("Protocol constants v{}:", PROTOCOL_VERSION);
        log::info!("  PACKET_PREFIX_BYTES      = {}", PACKET_PREFIX_BYTES);
        log::info!("  VIDEO_CHUNK_HEADER_BYTES  = {}", VIDEO_CHUNK_HEADER_BYTES);
        log::info!("  VIDEO_TOTAL_HEADER_BYTES  = {}", VIDEO_TOTAL_HEADER_BYTES);
        log::info!("  MAX_VIDEO_PAYLOAD_BYTES   = {}", MAX_VIDEO_PAYLOAD_BYTES);
        log::info!("  CURSOR_TOTAL_PACKET_BYTES = {}", CURSOR_TOTAL_PACKET_BYTES);
        log::info!("  INPUT_TOTAL_PACKET_BYTES  = {}", INPUT_TOTAL_PACKET_BYTES);

        assert_eq!(PACKET_PREFIX_BYTES, 6);
        assert_eq!(VIDEO_CHUNK_HEADER_BYTES, 36);
        assert_eq!(VIDEO_TOTAL_HEADER_BYTES, PACKET_PREFIX_BYTES + VIDEO_CHUNK_HEADER_BYTES);
        assert_eq!(MAX_VIDEO_PAYLOAD_BYTES, MAX_DATAGRAM_BYTES - VIDEO_TOTAL_HEADER_BYTES);
        assert_eq!(CURSOR_TOTAL_PACKET_BYTES, PACKET_PREFIX_BYTES + CURSOR_UPDATE_BYTES);
        assert_eq!(INPUT_TOTAL_PACKET_BYTES, PACKET_PREFIX_BYTES + INPUT_EVENT_BYTES);
    }
}

/// Binary packet structures for UDP channels (NOT protobuf).
pub mod binary {
    use super::constants::*;

    /// Common prefix for all UDP packets (6 bytes).
    #[derive(Debug, Clone, Copy)]
    pub struct PacketPrefix {
        pub magic: [u8; 4],
        pub version: u8,
        pub packet_type: u8,
    }

    impl PacketPrefix {
        pub fn parse(buf: &[u8]) -> Option<Self> {
            if buf.len() < PACKET_PREFIX_BYTES { return None; }
            let magic = [buf[0], buf[1], buf[2], buf[3]];
            if magic != MAGIC { return None; }
            Some(Self {
                magic,
                version: buf[4],
                packet_type: buf[5],
            })
        }

        pub fn is_valid(&self) -> bool {
            self.magic == MAGIC && self.version == PROTOCOL_VERSION
        }
    }

    /// Parsed CursorUpdate packet (29 bytes after PacketPrefix).
    #[derive(Debug, Clone, Copy)]
    pub struct CursorUpdate {
        pub seq: u32,
        pub timestamp_us: u64,
        pub x_px: i32,
        pub y_px: i32,
        pub shape_id: u8,
        pub hotspot_x_px: u16,
        pub hotspot_y_px: u16,
        pub cursor_scale: f32,
    }

    impl CursorUpdate {
        pub fn parse(buf: &[u8]) -> Option<Self> {
            if buf.len() < CURSOR_TOTAL_PACKET_BYTES { return None; }
            let off = PACKET_PREFIX_BYTES;
            Some(Self {
                seq: u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]),
                timestamp_us: u64::from_le_bytes([
                    buf[off+4], buf[off+5], buf[off+6], buf[off+7],
                    buf[off+8], buf[off+9], buf[off+10], buf[off+11],
                ]),
                x_px: i32::from_le_bytes([buf[off+12], buf[off+13], buf[off+14], buf[off+15]]),
                y_px: i32::from_le_bytes([buf[off+16], buf[off+17], buf[off+18], buf[off+19]]),
                shape_id: buf[off+20],
                hotspot_x_px: u16::from_le_bytes([buf[off+21], buf[off+22]]),
                hotspot_y_px: u16::from_le_bytes([buf[off+23], buf[off+24]]),
                cursor_scale: f32::from_bits(u32::from_le_bytes([buf[off+25], buf[off+26], buf[off+27], buf[off+28]])),
            })
        }
    }

    /// Video chunk header — per-packet fields (always valid).
    #[derive(Debug, Clone, Copy)]
    pub struct VideoChunkPerPacket {
        pub stream_id: u32,
        pub config_id: u32,
        pub frame_id: u32,
        pub chunk_id: u16,
        pub chunk_size: u16,
    }

    /// Video chunk header — per-frame fields (valid when chunk_id==0).
    #[derive(Debug, Clone, Copy)]
    pub struct VideoChunkPerFrame {
        pub timestamp_us: u64,
        pub is_keyframe: bool,
        pub codec: u8,
        pub width: u16,
        pub height: u16,
        pub total_chunks: u16,
        pub total_bytes: u32,
    }

    /// Parsed video chunk packet.
    #[derive(Debug)]
    pub struct VideoChunkPacket {
        pub per_packet: VideoChunkPerPacket,
        pub per_frame: Option<VideoChunkPerFrame>, // Some only when chunk_id==0
        pub payload: Vec<u8>,
    }

    impl VideoChunkPacket {
        /// Parse a complete UDP video packet (prefix already validated).
        /// Input: full packet bytes including prefix.
        pub fn parse(buf: &[u8]) -> Option<Self> {
            if buf.len() < VIDEO_TOTAL_HEADER_BYTES { return None; }
            let off = PACKET_PREFIX_BYTES;

            let per_packet = VideoChunkPerPacket {
                stream_id: u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]),
                config_id: u32::from_le_bytes([buf[off+4], buf[off+5], buf[off+6], buf[off+7]]),
                frame_id: u32::from_le_bytes([buf[off+8], buf[off+9], buf[off+10], buf[off+11]]),
                chunk_id: u16::from_le_bytes([buf[off+12], buf[off+13]]),
                chunk_size: u16::from_le_bytes([buf[off+14], buf[off+15]]),
            };

            let per_frame = if per_packet.chunk_id == 0 {
                let fo = off + 16; // per-frame fields start
                Some(VideoChunkPerFrame {
                    timestamp_us: u64::from_le_bytes([
                        buf[fo], buf[fo+1], buf[fo+2], buf[fo+3],
                        buf[fo+4], buf[fo+5], buf[fo+6], buf[fo+7],
                    ]),
                    is_keyframe: buf[fo+8] != 0,
                    codec: buf[fo+9],
                    width: u16::from_le_bytes([buf[fo+10], buf[fo+11]]),
                    height: u16::from_le_bytes([buf[fo+12], buf[fo+13]]),
                    total_chunks: u16::from_le_bytes([buf[fo+14], buf[fo+15]]),
                    total_bytes: u32::from_le_bytes([buf[fo+16], buf[fo+17], buf[fo+18], buf[fo+19]]),
                })
            } else {
                None
            };

            let payload_start = VIDEO_TOTAL_HEADER_BYTES;
            let payload_end = payload_start + per_packet.chunk_size as usize;
            if payload_end > buf.len() { return None; }

            Some(Self {
                per_packet,
                per_frame,
                payload: buf[payload_start..payload_end].to_vec(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::binary::*;
    use super::constants::*;

    #[test]
    fn constants_self_consistent() {
        assert_eq!(PACKET_PREFIX_BYTES, 6);
        assert_eq!(VIDEO_TOTAL_HEADER_BYTES, PACKET_PREFIX_BYTES + VIDEO_CHUNK_HEADER_BYTES);
        assert_eq!(MAX_VIDEO_PAYLOAD_BYTES, MAX_DATAGRAM_BYTES - VIDEO_TOTAL_HEADER_BYTES);
        assert_eq!(CURSOR_TOTAL_PACKET_BYTES, PACKET_PREFIX_BYTES + CURSOR_UPDATE_BYTES);
        assert_eq!(INPUT_TOTAL_PACKET_BYTES, PACKET_PREFIX_BYTES + INPUT_EVENT_BYTES);
    }

    #[test]
    fn constants_match_golden_file() {
        let json_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../proto/constants.json");
        let json_str = std::fs::read_to_string(json_path)
            .expect("proto/constants.json must exist");
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(v["PROTOCOL_VERSION"], PROTOCOL_VERSION as u64);
        assert_eq!(v["PACKET_PREFIX_BYTES"], PACKET_PREFIX_BYTES as u64);
        assert_eq!(v["VIDEO_CHUNK_HEADER_BYTES"], VIDEO_CHUNK_HEADER_BYTES as u64);
        assert_eq!(v["VIDEO_TOTAL_HEADER_BYTES"], VIDEO_TOTAL_HEADER_BYTES as u64);
        assert_eq!(v["MAX_DATAGRAM_BYTES"], MAX_DATAGRAM_BYTES as u64);
        assert_eq!(v["MAX_VIDEO_PAYLOAD_BYTES"], MAX_VIDEO_PAYLOAD_BYTES as u64);
        assert_eq!(v["CURSOR_UPDATE_BYTES"], CURSOR_UPDATE_BYTES as u64);
        assert_eq!(v["CURSOR_TOTAL_PACKET_BYTES"], CURSOR_TOTAL_PACKET_BYTES as u64);
        assert_eq!(v["INPUT_EVENT_BYTES"], INPUT_EVENT_BYTES as u64);
        assert_eq!(v["INPUT_TOTAL_PACKET_BYTES"], INPUT_TOTAL_PACKET_BYTES as u64);
        assert_eq!(v["PACKET_TYPE_VIDEO_CHUNK"], PACKET_TYPE_VIDEO_CHUNK as u64);
        assert_eq!(v["PACKET_TYPE_CURSOR_UPDATE"], PACKET_TYPE_CURSOR_UPDATE as u64);
        assert_eq!(v["PACKET_TYPE_INPUT_EVENT"], PACKET_TYPE_INPUT_EVENT as u64);
    }

    #[test]
    fn packet_prefix_valid_parse() {
        let buf = [0x52, 0x45, 0x53, 0x43, 1, 0]; // RESC, v1, video
        let prefix = PacketPrefix::parse(&buf).unwrap();
        assert!(prefix.is_valid());
        assert_eq!(prefix.packet_type, PACKET_TYPE_VIDEO_CHUNK);
    }

    #[test]
    fn packet_prefix_invalid_magic() {
        let buf = [0x00, 0x00, 0x00, 0x00, 1, 0];
        assert!(PacketPrefix::parse(&buf).is_none());
    }

    #[test]
    fn packet_prefix_too_short() {
        let buf = [0x52, 0x45, 0x53];
        assert!(PacketPrefix::parse(&buf).is_none());
    }

    #[test]
    fn video_chunk_roundtrip() {
        // Build a valid single-chunk video packet
        let mut buf = vec![0u8; VIDEO_TOTAL_HEADER_BYTES + 10];
        // PacketPrefix
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = PROTOCOL_VERSION;
        buf[5] = PACKET_TYPE_VIDEO_CHUNK;
        // Per-packet: stream_id=1, config_id=2, frame_id=3, chunk_id=0, chunk_size=10
        buf[6..10].copy_from_slice(&1u32.to_le_bytes());
        buf[10..14].copy_from_slice(&2u32.to_le_bytes());
        buf[14..18].copy_from_slice(&3u32.to_le_bytes());
        buf[18..20].copy_from_slice(&0u16.to_le_bytes());
        buf[20..22].copy_from_slice(&10u16.to_le_bytes());
        // Per-frame: timestamp=1000, keyframe=1, codec=0, 1920x1080, 1 chunk, 10 bytes
        buf[22..30].copy_from_slice(&1000u64.to_le_bytes());
        buf[30] = 1; // keyframe
        buf[31] = 0; // H.264
        buf[32..34].copy_from_slice(&1920u16.to_le_bytes());
        buf[34..36].copy_from_slice(&1080u16.to_le_bytes());
        buf[36..38].copy_from_slice(&1u16.to_le_bytes());
        buf[38..42].copy_from_slice(&10u32.to_le_bytes());
        // Payload
        buf[42..52].copy_from_slice(&[0xAA; 10]);

        let parsed = VideoChunkPacket::parse(&buf).unwrap();
        assert_eq!(parsed.per_packet.stream_id, 1);
        assert_eq!(parsed.per_packet.config_id, 2);
        assert_eq!(parsed.per_packet.frame_id, 3);
        assert_eq!(parsed.per_packet.chunk_id, 0);
        assert_eq!(parsed.per_packet.chunk_size, 10);
        let pf = parsed.per_frame.unwrap();
        assert_eq!(pf.timestamp_us, 1000);
        assert!(pf.is_keyframe);
        assert_eq!(pf.codec, 0);
        assert_eq!(pf.width, 1920);
        assert_eq!(pf.height, 1080);
        assert_eq!(pf.total_chunks, 1);
        assert_eq!(pf.total_bytes, 10);
        assert_eq!(parsed.payload, vec![0xAA; 10]);
    }

    #[test]
    fn cursor_update_roundtrip() {
        let mut buf = vec![0u8; CURSOR_TOTAL_PACKET_BYTES];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = PROTOCOL_VERSION;
        buf[5] = PACKET_TYPE_CURSOR_UPDATE;
        let off = PACKET_PREFIX_BYTES;
        buf[off..off+4].copy_from_slice(&42u32.to_le_bytes()); // seq
        buf[off+4..off+12].copy_from_slice(&99999u64.to_le_bytes()); // timestamp
        buf[off+12..off+16].copy_from_slice(&100i32.to_le_bytes()); // x
        buf[off+16..off+20].copy_from_slice(&200i32.to_le_bytes()); // y
        buf[off+20] = 1; // shape

        let parsed = CursorUpdate::parse(&buf).unwrap();
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.x_px, 100);
        assert_eq!(parsed.y_px, 200);
        assert_eq!(parsed.shape_id, 1);
    }

    #[test]
    fn protobuf_envelope_roundtrip() {
        use prost::Message;
        use super::resc_control;

        let envelope = resc_control::Envelope {
            session_id: 42,
            protocol_version: PROTOCOL_VERSION as u32,
            payload: Some(resc_control::envelope::Payload::RequestIdr(
                resc_control::RequestIdr {
                    stream_id: 1,
                    config_id: 2,
                    reason: 1,
                },
            )),
        };
        let bytes = envelope.encode_to_vec();
        let decoded = resc_control::Envelope::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.session_id, 42);
        assert_eq!(decoded.protocol_version, 1);
    }
}
