use anyhow::Result;
use clap::Parser;
use jitter_buffer::AssembledFrame;
use protocol::binary::{CursorUpdate, PacketPrefix};
use protocol::constants::*;
use std::sync::mpsc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "remote-display-client", about = "RESC Ubuntu client")]
struct Args {
    /// Host IP address (skip mDNS discovery)
    #[arg(short = 'H', long)]
    host: Option<String>,

    /// Control port
    #[arg(short, long, default_value_t = 9870)]
    port: u16,

    /// Preferred width
    #[arg(long, default_value_t = 1920)]
    width: u32,

    /// Preferred height
    #[arg(long, default_value_t = 1080)]
    height: u32,

    /// SDL2 display index for rendering
    #[arg(long, default_value_t = 0)]
    display: i32,

    /// Skip SDL2 flash test
    #[arg(long)]
    no_flash: bool,

    /// Dump received H.264 to file (before decode)
    #[arg(long)]
    dump_h264: Option<String>,

    /// Headless mode (no SDL2 rendering, just receive + decode)
    #[arg(long)]
    headless: bool,
}

/// Shared cursor state (written by cursor receiver thread, read by render thread).
struct SharedCursorState {
    x: AtomicI32,
    y: AtomicI32,
    shape: AtomicU32, // u8 stored as u32 for atomic
    seq: AtomicU32,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    // 1. Discover host
    let (host_addr, control_port) = if let Some(ref host) = args.host {
        (host.clone(), args.port)
    } else {
        log::info!("Discovering RESC host via mDNS...");
        match net_transport::discovery::discover_host(Duration::from_secs(10))? {
            Some(h) => (h.host, h.port),
            None => {
                log::error!("No RESC host found. Use --host <ip> to specify manually.");
                std::process::exit(1);
            }
        }
    };

    log::info!("Connecting to {}:{}", host_addr, control_port);

    // 2. TCP control channel
    let mut control = net_transport::control_channel::ControlChannel::connect(
        &host_addr, control_port
    ).await?;

    // 3. Mode negotiation
    let mode_confirm = control.negotiate_mode(args.width, args.height, 60000).await?;
    let stream_width = mode_confirm.stream_width;
    let stream_height = mode_confirm.stream_height;

    // 4. Wait for StartStreaming, reply StreamingReady
    control.wait_for_start_streaming(mode_confirm.stream_id, mode_confirm.config_id).await?;

    // 5. Start video receiver
    let video_port = mode_confirm.video_port as u16;
    let cursor_port = mode_confirm.cursor_udp_port as u16;
    let max_chunks = mode_confirm.max_total_chunks_per_frame as u16;
    let max_frame = mode_confirm.max_frame_bytes;
    let stream_id = mode_confirm.stream_id;
    let config_id = mode_confirm.config_id;

    let (frame_tx, frame_rx) = mpsc::sync_channel::<AssembledFrame>(2);

    let _recv_handle = std::thread::Builder::new()
        .name("video-recv".into())
        .spawn(move || {
            let mut receiver = net_transport::video_receiver::VideoReceiver::new(
                video_port, stream_id, config_id, max_chunks, max_frame,
            ).expect("Failed to create video receiver");
            receiver.run(frame_tx);
        })?;

    // 6. Start cursor receiver (shared atomic state)
    let cursor_state = Arc::new(SharedCursorState {
        x: AtomicI32::new(-1),
        y: AtomicI32::new(-1),
        shape: AtomicU32::new(0),
        seq: AtomicU32::new(0),
    });

    let cursor_state_writer = cursor_state.clone();
    let _cursor_handle = std::thread::Builder::new()
        .name("cursor-recv".into())
        .spawn(move || {
            let socket = match std::net::UdpSocket::bind(format!("0.0.0.0:{}", cursor_port)) {
                Ok(s) => s,
                Err(e) => { log::error!("Cursor UDP bind failed on port {}: {}", cursor_port, e); return; }
            };
            socket.set_read_timeout(Some(Duration::from_millis(100))).ok();
            log::info!("Cursor receiver listening on UDP port {}", cursor_port);

            let mut buf = [0u8; CURSOR_TOTAL_PACKET_BYTES + 16];
            loop {
                let n = match socket.recv(&mut buf) {
                    Ok(n) => n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => break,
                };

                if n < CURSOR_TOTAL_PACKET_BYTES { continue; }

                let prefix = match PacketPrefix::parse(&buf[..n]) {
                    Some(p) if p.is_valid() && p.packet_type == PACKET_TYPE_CURSOR_UPDATE => p,
                    _ => continue,
                };

                if let Some(update) = CursorUpdate::parse(&buf[..n]) {
                    // Latest-seq-wins
                    let prev_seq = cursor_state_writer.seq.load(Ordering::Relaxed);
                    if update.seq > prev_seq || (prev_seq > 0xFFFF0000 && update.seq < 0x0000FFFF) {
                        cursor_state_writer.x.store(update.x_px, Ordering::Relaxed);
                        cursor_state_writer.y.store(update.y_px, Ordering::Relaxed);
                        cursor_state_writer.shape.store(update.shape_id as u32, Ordering::Relaxed);
                        cursor_state_writer.seq.store(update.seq, Ordering::Relaxed);
                    }
                }
            }
        })?;

    // 7. Decode + render thread
    let dump_path = args.dump_h264.clone();
    let headless = args.headless;
    let display_idx = args.display;
    let no_flash = args.no_flash;
    let cursor_state_reader = cursor_state.clone();

    let _decode_render_handle = std::thread::Builder::new()
        .name("decode-render".into())
        .spawn(move || {
            let mut decoder = match video_decode::H264Decoder::new() {
                Ok(d) => d,
                Err(e) => { log::error!("Decoder init failed: {}", e); return; }
            };

            let mut renderer_opt = if !headless {
                match renderer::Renderer::new(display_idx, stream_width, stream_height, !no_flash) {
                    Ok(r) => Some(r),
                    Err(e) => { log::warn!("Renderer init failed: {} (headless)", e); None }
                }
            } else {
                None
            };

            let mut cursor_renderer = renderer::CursorRenderer::new();

            let mut dump_file = dump_path.as_ref().map(|p| {
                std::fs::File::create(p).expect("Failed to create dump file")
            });

            let start = std::time::Instant::now();
            let mut frame_count = 0u64;
            let mut decode_total_us = 0u64;
            let mut has_frame = false; // true once we've rendered at least one video frame

            loop {
                // Try to get a new video frame (non-blocking with short timeout)
                match frame_rx.recv_timeout(Duration::from_millis(8)) {
                    Ok(assembled) => {
                        if let Some(ref mut f) = dump_file {
                            use std::io::Write;
                            f.write_all(&assembled.data).ok();
                        }

                        let decode_start = std::time::Instant::now();
                        match decoder.decode(&assembled.data, assembled.timestamp_us) {
                            Ok(decoded_frames) => {
                                let decode_us = decode_start.elapsed().as_micros() as u64;
                                decode_total_us += decode_us;

                                for decoded in &decoded_frames {
                                    frame_count += 1;
                                    has_frame = true;

                                    if let Some(ref mut r) = renderer_opt {
                                        if let Err(e) = r.update_frame(decoded) {
                                            log::warn!("Render error: {}", e);
                                            continue;
                                        }
                                    }

                                    if frame_count == 1 {
                                        log::info!(
                                            "First decoded frame: {}x{}, decode {:.1}ms",
                                            decoded.width, decoded.height, decode_us as f64 / 1000.0
                                        );
                                    }
                                }
                            }
                            Err(e) => { log::warn!("Decode error: {}", e); }
                        }

                        if frame_count > 0 && frame_count % 60 == 0 {
                            let elapsed = start.elapsed().as_secs_f64();
                            let fps = frame_count as f64 / elapsed;
                            let avg_ms = (decode_total_us as f64 / frame_count as f64) / 1000.0;
                            log::info!("Decoded: {} frames, {:.1} fps, avg decode {:.1}ms", frame_count, fps, avg_ms);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // No new frame — that's fine, just re-render cursor
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        log::info!("Frame channel disconnected");
                        break;
                    }
                }

                // Always draw cursor on top of latest frame at ~120Hz
                if has_frame {
                    if let Some(ref mut r) = renderer_opt {
                        let cx = cursor_state_reader.x.load(Ordering::Relaxed);
                        let cy = cursor_state_reader.y.load(Ordering::Relaxed);
                        let cs = cursor_state_reader.shape.load(Ordering::Relaxed) as u8;
                        if cx >= 0 && cy >= 0 {
                            cursor_renderer.update(cx, cy, cs);
                        }
                        r.present_with_cursor(&cursor_renderer);
                    }
                }
            }
            log::info!("Decode/render stopped: {} frames", frame_count);
        })?;

    log::info!("Streaming active. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    log::info!("Shutting down...");
    Ok(())
}
