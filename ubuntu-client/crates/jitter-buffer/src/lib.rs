use protocol::binary::{VideoChunkPerFrame, VideoChunkPerPacket};
use std::time::Instant;

/// Assembles video frames from UDP chunks. Preallocated, no per-packet heap allocs in hot path.
/// Tracks received chunks via bitset. Max 4 in-flight frames.
pub struct FrameAssembler {
    slots: [FrameSlot; 4], // max 4 in-flight frames
    max_chunks_per_frame: u16,
    max_frame_bytes: u32,
    /// Stats
    pub frames_completed: u64,
    pub frames_dropped: u64,
    pub chunks_dropped: u64,
}

struct FrameSlot {
    active: bool,
    frame_id: u32,
    metadata: Option<VideoChunkPerFrame>,
    data: Vec<u8>,       // preallocated contiguous buffer
    chunk_offsets: Vec<u32>, // offset[chunk_id] = byte offset in data
    received: [u64; 4],  // bitset for up to 256 chunks
    chunks_received: u16,
    first_chunk_time: Instant,
}

impl FrameSlot {
    fn new(max_frame_bytes: usize, max_chunks: usize) -> Self {
        Self {
            active: false,
            frame_id: 0,
            metadata: None,
            data: vec![0u8; max_frame_bytes],
            chunk_offsets: vec![0u32; max_chunks],
            received: [0u64; 4],
            chunks_received: 0,
            first_chunk_time: Instant::now(),
        }
    }

    fn reset(&mut self, frame_id: u32) {
        self.active = true;
        self.frame_id = frame_id;
        self.metadata = None;
        self.received = [0u64; 4];
        self.chunks_received = 0;
        self.first_chunk_time = Instant::now();
    }

    fn mark_chunk(&mut self, chunk_id: u16) -> bool {
        let idx = chunk_id as usize / 64;
        let bit = chunk_id as usize % 64;
        if idx >= 4 { return false; }
        let was_set = (self.received[idx] >> bit) & 1 == 1;
        if !was_set {
            self.received[idx] |= 1u64 << bit;
            self.chunks_received += 1;
        }
        !was_set // returns true if newly marked
    }

    fn is_complete(&self) -> bool {
        if let Some(ref meta) = self.metadata {
            self.chunks_received >= meta.total_chunks
        } else {
            false
        }
    }
}

/// An assembled frame ready for decode.
pub struct AssembledFrame {
    pub frame_id: u32,
    pub timestamp_us: u64,
    pub is_keyframe: bool,
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

impl FrameAssembler {
    pub fn new(max_chunks_per_frame: u16, max_frame_bytes: u32) -> Self {
        let slots = std::array::from_fn(|_| {
            FrameSlot::new(max_frame_bytes as usize, max_chunks_per_frame as usize)
        });
        Self {
            slots,
            max_chunks_per_frame,
            max_frame_bytes,
            frames_completed: 0,
            frames_dropped: 0,
            chunks_dropped: 0,
        }
    }

    /// Process an incoming video chunk. Returns Some(AssembledFrame) when a frame is complete.
    pub fn process_chunk(
        &mut self,
        per_packet: &VideoChunkPerPacket,
        per_frame: Option<&VideoChunkPerFrame>,
        payload: &[u8],
    ) -> Option<AssembledFrame> {
        // Validation: chunk_size vs payload
        if payload.len() > protocol::constants::MAX_VIDEO_PAYLOAD_BYTES {
            self.chunks_dropped += 1;
            return None;
        }
        if per_packet.chunk_id >= self.max_chunks_per_frame {
            self.chunks_dropped += 1;
            return None;
        }

        // Find or allocate slot for this frame_id
        let slot_idx = self.find_or_allocate_slot(per_packet.frame_id);
        let slot = &mut self.slots[slot_idx];

        // Store metadata from chunk 0
        if let Some(meta) = per_frame {
            if meta.total_chunks > self.max_chunks_per_frame {
                slot.active = false;
                self.frames_dropped += 1;
                return None;
            }
            if meta.total_bytes > self.max_frame_bytes {
                slot.active = false;
                self.frames_dropped += 1;
                return None;
            }
            slot.metadata = Some(*meta);

            // Discard out-of-range chunks received before metadata
            // (validation rule 6 from spec)
            for cid in 0..self.max_chunks_per_frame {
                let idx = cid as usize / 64;
                let bit = cid as usize % 64;
                if idx < 4 && (slot.received[idx] >> bit) & 1 == 1 && cid >= meta.total_chunks {
                    slot.received[idx] &= !(1u64 << bit);
                    slot.chunks_received -= 1;
                }
            }
        }

        // Store payload at correct offset
        let offset = per_packet.chunk_id as usize * protocol::constants::MAX_VIDEO_PAYLOAD_BYTES;
        if offset + payload.len() <= slot.data.len() {
            slot.data[offset..offset + payload.len()].copy_from_slice(payload);
            slot.chunk_offsets[per_packet.chunk_id as usize] = offset as u32;
            slot.mark_chunk(per_packet.chunk_id);
        }

        // Check if complete
        if slot.is_complete() {
            let meta = slot.metadata.unwrap();
            let total = meta.total_bytes as usize;
            let frame_data = slot.data[..total].to_vec();
            slot.active = false;
            self.frames_completed += 1;

            return Some(AssembledFrame {
                frame_id: per_packet.frame_id,
                timestamp_us: meta.timestamp_us,
                is_keyframe: meta.is_keyframe,
                codec: meta.codec,
                width: meta.width,
                height: meta.height,
                data: frame_data,
            });
        }

        None
    }

    /// Check for timed-out frames (>30ms since first chunk).
    pub fn expire_stale(&mut self) {
        let now = Instant::now();
        for slot in &mut self.slots {
            if slot.active && now.duration_since(slot.first_chunk_time).as_millis() > 30 {
                log::debug!("Frame {} timed out ({} chunks received)",
                           slot.frame_id, slot.chunks_received);
                slot.active = false;
                self.frames_dropped += 1;
            }
        }
    }

    fn find_or_allocate_slot(&mut self, frame_id: u32) -> usize {
        // Check existing
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.active && slot.frame_id == frame_id {
                return i;
            }
        }

        // Find empty
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.active {
                self.slots[i].reset(frame_id);
                return i;
            }
        }

        // Evict oldest (lowest frame_id)
        let oldest = self.slots.iter().enumerate()
            .min_by_key(|(_, s)| s.frame_id)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.frames_dropped += 1;
        self.slots[oldest].reset(frame_id);
        oldest
    }
}
