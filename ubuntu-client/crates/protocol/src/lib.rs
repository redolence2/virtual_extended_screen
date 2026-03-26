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
