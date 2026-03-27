use anyhow::Result;
use jitter_buffer::{AssembledFrame, FrameAssembler};
use protocol::binary::{PacketPrefix, VideoChunkPacket};
use protocol::constants::*;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Shared stats counters — updated in real-time by receiver, read by stats reporter.
pub struct SharedReceiverStats {
    pub packets_received: AtomicU64,
    pub packets_dropped: AtomicU64,
    pub frames_completed: AtomicU64,
    pub frames_dropped: AtomicU64,
}

impl Default for SharedReceiverStats {
    fn default() -> Self {
        Self {
            packets_received: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            frames_completed: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
        }
    }
}

/// Receives chunked video over UDP, assembles frames, sends complete frames to decode.
pub struct VideoReceiver {
    socket: UdpSocket,
    assembler: FrameAssembler,
    stream_id: u32,
    config_id: u32,
    shared_stats: Arc<SharedReceiverStats>,
    // Local counters (avoid atomic overhead on hot path, flush periodically)
    local_packets_received: u64,
    local_packets_dropped: u64,
}

impl VideoReceiver {
    pub fn new(
        port: u16,
        stream_id: u32,
        config_id: u32,
        max_chunks_per_frame: u16,
        max_frame_bytes: u32,
        shared_stats: Arc<SharedReceiverStats>,
    ) -> Result<Self> {
        // Create socket with SO_REUSEADDR before bind (avoids "Address already in use")
        let raw_socket = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            if fd < 0 { anyhow::bail!("socket() failed"); }
            let one: i32 = 1;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR,
                &one as *const i32 as *const libc::c_void,
                std::mem::size_of::<i32>() as libc::socklen_t);
            let mut addr: libc::sockaddr_in = std::mem::zeroed();
            addr.sin_family = libc::AF_INET as u16;
            addr.sin_port = port.to_be();
            addr.sin_addr.s_addr = 0; // INADDR_ANY
            let ret = libc::bind(fd, &addr as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t);
            if ret < 0 { libc::close(fd); anyhow::bail!("bind() failed on port {}: {}", port, std::io::Error::last_os_error()); }
            fd
        };
        let socket: UdpSocket = unsafe { std::os::unix::io::FromRawFd::from_raw_fd(raw_socket) };
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;

        // SO_RCVBUF tuning for 4K60 (day one, per spec)
        let rcvbuf = 2 * 1024 * 1024; // 2MB
        if let Err(e) = set_rcvbuf(&socket, rcvbuf) {
            log::warn!("Failed to set SO_RCVBUF to {}B: {}", rcvbuf, e);
        }

        log::info!("Video receiver listening on UDP port {}", port);

        Ok(Self {
            socket,
            assembler: FrameAssembler::new(max_chunks_per_frame, max_frame_bytes),
            stream_id,
            config_id,
            shared_stats,
            local_packets_received: 0,
            local_packets_dropped: 0,
        })
    }

    /// Run the receive loop. Sends assembled frames to the provided channel.
    /// Blocking — run in a dedicated thread.
    /// Flush local counters to shared atomics (called every ~100 packets).
    fn flush_stats(&self) {
        self.shared_stats.packets_received.store(self.local_packets_received, Ordering::Relaxed);
        self.shared_stats.packets_dropped.store(self.local_packets_dropped, Ordering::Relaxed);
        self.shared_stats.frames_completed.store(self.assembler.frames_completed, Ordering::Relaxed);
        self.shared_stats.frames_dropped.store(self.assembler.frames_dropped, Ordering::Relaxed);
    }

    pub fn run(&mut self, frame_tx: mpsc::SyncSender<AssembledFrame>) {
        let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];
        let mut last_expire = Instant::now();

        loop {
            // Expire stale frames every 10ms
            if last_expire.elapsed() > Duration::from_millis(10) {
                self.assembler.expire_stale();
                last_expire = Instant::now();
            }

            let n = match self.socket.recv(&mut buf) {
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    log::error!("UDP recv error: {}", e);
                    break;
                }
            };

            if n < VIDEO_TOTAL_HEADER_BYTES {
                self.local_packets_dropped += 1;
                continue;
            }

            // Validate prefix
            let prefix = match PacketPrefix::parse(&buf[..n]) {
                Some(p) => p,
                None => { self.local_packets_dropped += 1; continue; }
            };

            if prefix.version != PROTOCOL_VERSION {
                self.local_packets_dropped += 1;
                continue;
            }

            if prefix.packet_type != PACKET_TYPE_VIDEO_CHUNK {
                self.local_packets_dropped += 1;
                continue;
            }

            // Parse video chunk
            let chunk = match VideoChunkPacket::parse(&buf[..n]) {
                Some(c) => c,
                None => { self.local_packets_dropped += 1; continue; }
            };

            // Filter by stream_id and config_id
            if chunk.per_packet.stream_id != self.stream_id
                || chunk.per_packet.config_id != self.config_id
            {
                self.local_packets_dropped += 1;
                continue;
            }

            self.local_packets_received += 1;

            // Flush to shared atomics every 100 packets
            if self.local_packets_received % 100 == 0 {
                self.flush_stats();
            }

            if self.local_packets_received == 1 {
                log::info!(
                    "First video packet: frame_id={}, chunk_id={}, size={}B, stream_id={}",
                    chunk.per_packet.frame_id, chunk.per_packet.chunk_id,
                    chunk.per_packet.chunk_size, chunk.per_packet.stream_id
                );
                if let Some(ref pf) = chunk.per_frame {
                    log::info!(
                        "  Frame meta: {}x{}, total_chunks={}, total_bytes={}, keyframe={}",
                        pf.width, pf.height, pf.total_chunks, pf.total_bytes, pf.is_keyframe
                    );
                }
            }

            if self.local_packets_received % 100 == 0 {
                log::info!(
                    "Recv stats: {} packets, {} dropped, {} assembled, {} frame_drops",
                    self.local_packets_received, self.local_packets_dropped,
                    self.assembler.frames_completed, self.assembler.frames_dropped
                );
            }

            // Feed to assembler
            if let Some(frame) = self.assembler.process_chunk(
                &chunk.per_packet,
                chunk.per_frame.as_ref(),
                &chunk.payload,
            ) {
                // Smart queue policy (Item 5 from review):
                // Try non-blocking send first. If full, decide by frame type.
                match frame_tx.try_send(frame) {
                    Ok(_) => {}
                    Err(mpsc::TrySendError::Full(pending_frame)) => {
                        if pending_frame.is_keyframe {
                            // Never drop keyframes — block until space available
                            let _ = frame_tx.send(pending_frame);
                        } else {
                            // Drop non-keyframe when queue full
                            self.assembler.frames_dropped += 1;
                            log::debug!("Queue full, dropped frame {} (non-keyframe)", pending_frame.frame_id);
                        }
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        log::info!("Frame channel disconnected, stopping");
                        break;
                    }
                }
            }
        }

        // Final flush so stats reporter sees final values
        self.flush_stats();
        log::info!(
            "Video receiver stopped: {} packets, {} dropped, {} completed frames, {} dropped frames",
            self.local_packets_received, self.local_packets_dropped,
            self.assembler.frames_completed, self.assembler.frames_dropped
        );
    }

    pub fn update_stream_config(&mut self, stream_id: u32, config_id: u32) {
        self.stream_id = stream_id;
        self.config_id = config_id;
    }
}

#[cfg(target_os = "linux")]
fn set_rcvbuf(socket: &UdpSocket, size: i32) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        )
    };
    if ret != 0 { anyhow::bail!("setsockopt failed: {}", std::io::Error::last_os_error()); }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_rcvbuf(_socket: &UdpSocket, _size: i32) -> Result<()> {
    Ok(()) // no-op on non-Linux for now
}
