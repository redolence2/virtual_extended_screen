use protocol::binary::{VideoChunkPerFrame, VideoChunkPerPacket};
use std::time::Instant;

/// Why a frame was dropped (for telemetry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropReason {
    Timeout,       // Assembly deadline exceeded
    Oversize,      // total_bytes or total_chunks exceeded limits
    MissingChunks, // Evicted with incomplete chunks
    Evicted,       // Evicted by newer frame needing the slot
}

/// Assembles video frames from UDP chunks. Preallocated, no per-packet heap allocs in hot path.
/// Tracks received chunks via bitset. Max 4 in-flight frames.
pub struct FrameAssembler {
    slots: [FrameSlot; 4],
    max_chunks_per_frame: u16,
    max_frame_bytes: u32,
    /// Assembly deadline in ms — frames older than this are expired.
    assembly_deadline_ms: u64,
    /// Stats
    pub frames_completed: u64,
    pub frames_dropped: u64,
    pub chunks_dropped: u64,
    pub timeout_drops: u64,
    pub oversize_drops: u64,
    pub eviction_drops: u64,
}

struct FrameSlot {
    active: bool,
    frame_id: u32,
    metadata: Option<VideoChunkPerFrame>,
    data: Vec<u8>,           // preallocated buffer (chunks stored at stride offsets)
    chunk_sizes: Vec<u16>,   // actual payload size per chunk
    // Chunk-receipt bitset sized from max_chunks (native-4K finding,
    // 2026-08-05: the old fixed [u64; 8] = 512 bits silently refused
    // chunk ids >= 512, so every ~753-chunk 4K keyframe timed out at
    // exactly 512/753 and the screen stayed black).
    received: Vec<u64>,
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
            chunk_sizes: vec![0u16; max_chunks],
            received: vec![0u64; (max_chunks + 63) / 64],
            chunks_received: 0,
            first_chunk_time: Instant::now(),
        }
    }

    fn reset(&mut self, frame_id: u32) {
        self.active = true;
        self.frame_id = frame_id;
        self.metadata = None;
        self.received.iter_mut().for_each(|w| *w = 0);
        self.chunks_received = 0;
        self.first_chunk_time = Instant::now();
        for s in self.chunk_sizes.iter_mut() { *s = 0; }
    }

    fn mark_chunk(&mut self, chunk_id: u16) -> bool {
        let idx = chunk_id as usize / 64;
        let bit = chunk_id as usize % 64;
        if idx >= self.received.len() { return false; }
        let was_set = (self.received[idx] >> bit) & 1 == 1;
        if !was_set {
            self.received[idx] |= 1u64 << bit;
            self.chunks_received += 1;
        }
        !was_set
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
    /// Client receive timestamp (A00_REMEDIATION_PLAN.md §4 item 6: "The
    /// client timestamps receipt when the complete encoded frame is
    /// assembled, before any drop-capable queue"). This crate has no
    /// dependency on `diagnostics`, so [`FrameAssembler::process_chunk`]
    /// fills in a `0` placeholder here — `net-transport`'s `VideoReceiver`
    /// (which does depend on `diagnostics`) overwrites it with the real
    /// `diagnostics::mono_us()` reading immediately upon receiving this
    /// frame back, before it goes anywhere near the bounded `sync_channel`.
    pub recv_ts_us: u64,
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
            assembly_deadline_ms: 100, // 100ms default, tight for real-time
            frames_completed: 0,
            frames_dropped: 0,
            chunks_dropped: 0,
            timeout_drops: 0,
            oversize_drops: 0,
            eviction_drops: 0,
        }
    }

    /// Process an incoming video chunk. Returns Some(AssembledFrame) when a frame is complete.
    pub fn process_chunk(
        &mut self,
        per_packet: &VideoChunkPerPacket,
        per_frame: Option<&VideoChunkPerFrame>,
        payload: &[u8],
    ) -> Option<AssembledFrame> {
        // Packet-level validation (always, regardless of chunk_id)
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

        // Frame-metadata validation (only when per_frame is present, i.e. chunk_id==0)
        if let Some(meta) = per_frame {
            // Oversize drops were silent until 2026-08-12, when an oversize
            // KEYFRAME (2.4-2.7MB vs a 2.08MB limit) black-screened a whole
            // run: the gated decoder never got an IDR and the only visible
            // symptom was a "frame_drops" counter ticking by one. Log it —
            // rate-limited, and never quietly for a keyframe, which is the
            // unrecoverable case.
            let oversize = meta.total_chunks > self.max_chunks_per_frame
                || meta.total_bytes > self.max_frame_bytes;
            if oversize {
                slot.active = false;
                self.frames_dropped += 1;
                self.oversize_drops += 1;
                if meta.is_keyframe || self.oversize_drops % 60 == 1 {
                    log::error!(
                        "OVERSIZE DROP{}: frame {} is {}B/{} chunks, limits {}B/{} chunks — \
                         raise host max_frame_bytes/max_total_chunks_per_frame{}",
                        if meta.is_keyframe { " (KEYFRAME — decoder cannot recover)" } else { "" },
                        per_packet.frame_id, meta.total_bytes, meta.total_chunks,
                        self.max_frame_bytes, self.max_chunks_per_frame,
                        if meta.is_keyframe { "; screen stays black until a smaller keyframe arrives" } else { "" },
                    );
                }
                return None;
            }
            slot.metadata = Some(*meta);

            // Discard out-of-range chunks received before metadata
            for cid in 0..self.max_chunks_per_frame {
                let idx = cid as usize / 64;
                let bit = cid as usize % 64;
                if idx < slot.received.len() && (slot.received[idx] >> bit) & 1 == 1 && cid >= meta.total_chunks {
                    slot.received[idx] &= !(1u64 << bit);
                    slot.chunks_received -= 1;
                }
            }
        }

        // Store payload at stride offset
        let cid = per_packet.chunk_id as usize;
        let stride = protocol::constants::MAX_VIDEO_PAYLOAD_BYTES;
        let offset = cid * stride;
        if offset + payload.len() <= slot.data.len() {
            slot.data[offset..offset + payload.len()].copy_from_slice(payload);
            slot.chunk_sizes[cid] = payload.len() as u16;
            slot.mark_chunk(per_packet.chunk_id);
        }

        // Check if complete
        if slot.is_complete() {
            let meta = slot.metadata.unwrap();
            let total_chunks = meta.total_chunks as usize;

            // Optimization: skip compaction for single-chunk frames (already contiguous)
            let frame_data = if total_chunks == 1 {
                let chunk_len = slot.chunk_sizes[0] as usize;
                slot.data[..chunk_len].to_vec()
            } else {
                let mut buf = Vec::with_capacity(meta.total_bytes as usize);
                for i in 0..total_chunks {
                    let chunk_offset = i * stride;
                    let chunk_len = slot.chunk_sizes[i] as usize;
                    buf.extend_from_slice(&slot.data[chunk_offset..chunk_offset + chunk_len]);
                }
                buf
            };

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
                // Overwritten by VideoReceiver::run() right after this
                // returns — see the field doc comment.
                recv_ts_us: 0,
            });
        }

        None
    }

    /// Expire frames that exceeded the assembly deadline.
    pub fn expire_stale(&mut self) {
        let now = Instant::now();
        let deadline = self.assembly_deadline_ms;
        for slot in &mut self.slots {
            if slot.active && now.duration_since(slot.first_chunk_time).as_millis() > deadline as u128 {
                log::debug!(
                    "Frame {} timed out after {}ms ({}/{} chunks)",
                    slot.frame_id,
                    deadline,
                    slot.chunks_received,
                    slot.metadata.map(|m| m.total_chunks).unwrap_or(0)
                );
                slot.active = false;
                self.frames_dropped += 1;
                self.timeout_drops += 1;
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
        self.eviction_drops += 1;
        self.slots[oldest].reset(frame_id);
        oldest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::binary::*;
    use protocol::constants::*;

    fn make_pp(frame_id: u32, chunk_id: u16, chunk_size: u16) -> VideoChunkPerPacket {
        VideoChunkPerPacket { stream_id: 1, config_id: 1, frame_id, chunk_id, chunk_size }
    }

    fn make_pf(total_chunks: u16, total_bytes: u32) -> VideoChunkPerFrame {
        VideoChunkPerFrame {
            timestamp_us: 1000, is_keyframe: true, codec: 0,
            width: 1920, height: 1080, total_chunks, total_bytes,
        }
    }

    #[test]
    fn single_chunk_frame_completes() {
        let mut asm = FrameAssembler::new(16, 100_000);
        let pp = make_pp(1, 0, 5);
        let pf = make_pf(1, 5);
        let payload = vec![0xAA; 5];

        let result = asm.process_chunk(&pp, Some(&pf), &payload);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(frame.frame_id, 1);
        assert_eq!(frame.data, vec![0xAA; 5]);
        assert_eq!(asm.frames_completed, 1);
    }

    #[test]
    fn out_of_order_chunks_assemble() {
        let mut asm = FrameAssembler::new(16, 100_000);
        let pf = make_pf(3, 15);

        // Send chunks 2, 0, 1
        let r = asm.process_chunk(&make_pp(1, 2, 5), None, &[0xCC; 5]);
        assert!(r.is_none());
        let r = asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]);
        assert!(r.is_none());
        let r = asm.process_chunk(&make_pp(1, 1, 5), None, &[0xBB; 5]);
        assert!(r.is_some());

        let frame = r.unwrap();
        // Data should be chunk0 ++ chunk1 ++ chunk2
        assert_eq!(&frame.data[0..5], &[0xAA; 5]);
        assert_eq!(&frame.data[5..10], &[0xBB; 5]);
        assert_eq!(&frame.data[10..15], &[0xCC; 5]);
    }

    #[test]
    fn missing_chunk_timeout() {
        let mut asm = FrameAssembler::new(16, 100_000);
        asm.assembly_deadline_ms = 0; // immediate timeout

        let pf = make_pf(3, 15);
        asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]);
        // Only 1 of 3 chunks — expire immediately
        std::thread::sleep(std::time::Duration::from_millis(1));
        asm.expire_stale();

        assert_eq!(asm.frames_dropped, 1);
        assert_eq!(asm.timeout_drops, 1);
    }

    #[test]
    fn oversize_frame_rejected() {
        let mut asm = FrameAssembler::new(16, 1000); // max 1000 bytes
        let pf = make_pf(1, 2000); // claims 2000 bytes

        let r = asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]);
        assert!(r.is_none());
        assert_eq!(asm.oversize_drops, 1);
    }

    #[test]
    fn oversize_chunk_count_rejected() {
        let mut asm = FrameAssembler::new(4, 100_000); // max 4 chunks
        let pf = make_pf(5, 25); // claims 5 chunks

        let r = asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]);
        assert!(r.is_none());
        assert_eq!(asm.oversize_drops, 1);
    }

    #[test]
    fn slot_eviction_on_full() {
        let mut asm = FrameAssembler::new(16, 100_000);
        // Fill all 4 slots with incomplete frames (frame_id 1-4)
        for fid in 1..=4u32 {
            let pf = make_pf(2, 10);
            asm.process_chunk(&make_pp(fid, 0, 5), Some(&pf), &[0xAA; 5]);
        }
        // Add frame 5 — should evict frame 1 (lowest id)
        let pf = make_pf(2, 10);
        asm.process_chunk(&make_pp(5, 0, 5), Some(&pf), &[0xAA; 5]);
        assert_eq!(asm.eviction_drops, 1);
    }

    #[test]
    fn duplicate_chunk_ignored() {
        let mut asm = FrameAssembler::new(16, 100_000);
        let pf = make_pf(2, 10);

        asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]);
        asm.process_chunk(&make_pp(1, 0, 5), Some(&pf), &[0xAA; 5]); // duplicate
        // Should still need chunk 1 to complete
        let r = asm.process_chunk(&make_pp(1, 1, 5), None, &[0xBB; 5]);
        assert!(r.is_some());
        assert_eq!(asm.frames_completed, 1);
    }
}
