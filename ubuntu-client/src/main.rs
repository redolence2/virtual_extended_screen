use anyhow::Result;
use clap::Parser;
use jitter_buffer::AssembledFrame;
use protocol::binary::{CursorUpdate, PacketPrefix};
use protocol::constants::*;
use std::sync::mpsc;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
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

/// Shared receiver stats for real telemetry (Item 9 from review).
#[derive(Default)]
struct ReceiverStats {
    packets_received: AtomicU64,
    packets_dropped: AtomicU64,
    frames_completed: AtomicU64,
    frames_dropped: AtomicU64,
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

    // Bounded queue: small to minimize latency (Item 5 from review).
    // Receiver uses smart drop policy: keyframes always kept.
    let (frame_tx, frame_rx) = mpsc::sync_channel::<AssembledFrame>(8);

    // Shared stats counters (Item 9: real telemetry for adaptive bitrate)
    let recv_stats = Arc::new(ReceiverStats::default());
    let recv_stats_writer = recv_stats.clone();
    let recv_stats_reader = recv_stats.clone();

    let _recv_handle = std::thread::Builder::new()
        .name("video-recv".into())
        .spawn(move || {
            let mut receiver = net_transport::video_receiver::VideoReceiver::new(
                video_port, stream_id, config_id, max_chunks, max_frame,
            ).expect("Failed to create video receiver");
            receiver.run(frame_tx);
            // Write final stats
            recv_stats_writer.packets_received.store(receiver.packets_received, Ordering::Relaxed);
            recv_stats_writer.packets_dropped.store(receiver.packets_dropped, Ordering::Relaxed);
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

    // 6b. Stats reporter — sends real telemetry to host every 1s (Item 9 from review)
    tokio::spawn(async move {
        let mut prev_recv = 0u64;
        let mut prev_drop = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let recv = recv_stats_reader.packets_received.load(Ordering::Relaxed);
            let drop = recv_stats_reader.packets_dropped.load(Ordering::Relaxed);
            let f_drop = recv_stats_reader.frames_dropped.load(Ordering::Relaxed);
            let f_done = recv_stats_reader.frames_completed.load(Ordering::Relaxed);

            // Compute rates over last 1s interval
            let interval_recv = recv.saturating_sub(prev_recv);
            let interval_drop = drop.saturating_sub(prev_drop);
            let loss_rate = if interval_recv + interval_drop > 0 {
                interval_drop as f32 / (interval_recv + interval_drop) as f32
            } else { 0.0 };
            let frame_drop_rate = if f_done + f_drop > 0 {
                f_drop as f32 / (f_done + f_drop) as f32
            } else { 0.0 };

            prev_recv = recv;
            prev_drop = drop;

            if let Err(_) = control.send_stats(loss_rate, frame_drop_rate, 0).await {
                log::warn!("Stats send failed (control channel closed)");
                break;
            }
        }
    });

    // 7. Decode + render + input thread (SDL2 must be on one thread)
    let dump_path = args.dump_h264.clone();
    let headless = args.headless;
    let display_idx = args.display;
    let no_flash = args.no_flash;
    let cursor_state_reader = cursor_state.clone();
    let host_addr_for_input = host_addr.clone();
    let input_udp_port = mode_confirm.input_udp_port as u16;

    let _decode_render_handle = std::thread::Builder::new()
        .name("decode-render".into())
        .spawn(move || {
            // Determine codec from ModeConfirm (0=H.264, 1=HEVC)
            let codec_id = mode_confirm.codec as u8;
            let mut decoder = match video_decode::VideoDecoder::new(codec_id) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("Decoder init failed for codec {}: {}", codec_id, e);
                    // Fallback to H.264 if HEVC fails
                    if codec_id != 0 {
                        log::info!("Falling back to H.264 decoder");
                        match video_decode::VideoDecoder::new(0) {
                            Ok(d) => d,
                            Err(e2) => { log::error!("H.264 fallback also failed: {}", e2); return; }
                        }
                    } else { return; }
                }
            };

            // Init SDL2 (needed for both renderer and input)
            let sdl = sdl2::init().expect("SDL init");
            let video = sdl.video().expect("SDL video");
            let mut event_pump = sdl.event_pump().expect("SDL event pump");

            let mut renderer_opt = if !headless {
                match renderer::Renderer::new(display_idx, stream_width, stream_height, !no_flash) {
                    Ok(r) => Some(r),
                    Err(e) => { log::warn!("Renderer init failed: {} (headless)", e); None }
                }
            } else {
                None
            };

            let mut cursor_renderer = renderer::CursorRenderer::new();

            // Input capture (Phase 6)
            let mut input = input_capture::InputCapture::new(
                &host_addr_for_input, input_udp_port, stream_width, stream_height
            );

            let mut dump_file = dump_path.as_ref().map(|p| {
                std::fs::File::create(p).expect("Failed to create dump file")
            });

            let start = std::time::Instant::now();
            let mut frame_count = 0u64;
            let mut decode_total_us = 0u64;
            let mut has_frame = false;
            let mut new_video_frame; // set per loop iteration

            loop {
                new_video_frame = false;

                // Collect ALL available frames from queue, DECODE all (maintains
                // HEVC reference chain), but only RENDER the last good one.
                let mut frames_to_decode: Vec<AssembledFrame> = Vec::new();
                match frame_rx.recv_timeout(Duration::from_millis(8)) {
                    Ok(frame) => {
                        frames_to_decode.push(frame);
                        while let Ok(more) = frame_rx.try_recv() {
                            frames_to_decode.push(more);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        log::info!("Frame channel disconnected");
                        break;
                    }
                }

                if !frames_to_decode.is_empty() {
                    let batch_size = frames_to_decode.len();

                    for (i, assembled) in frames_to_decode.iter().enumerate() {
                        let is_last = i == batch_size - 1;

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
                                    if decoded.planes[0].is_empty() { continue; }
                                    frame_count += 1;
                                    has_frame = true;

                                    // Only render the LAST frame in the batch (latest content)
                                    if is_last {
                                        new_video_frame = true;
                                        if let Some(ref mut r) = renderer_opt {
                                            let _ = r.update_frame(decoded);
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
                    }

                    if frame_count > 0 && frame_count % 60 == 0 {
                        let elapsed = start.elapsed().as_secs_f64();
                        let fps = frame_count as f64 / elapsed;
                        let avg_ms = (decode_total_us as f64 / frame_count as f64) / 1000.0;
                        log::info!("Decoded: {} frames, {:.1} fps, avg decode {:.1}ms", frame_count, fps, avg_ms);
                    }
                }

                // Process SDL2 events (input capture)
                for event in event_pump.poll_iter() {
                    use sdl2::event::Event;
                    use sdl2::keyboard::Mod;
                    match event {
                        Event::Quit { .. } => { return; }
                        Event::KeyDown { scancode: Some(sc), keycode, keymod, .. } => {
                            if let Some(_key_out) = input.process_key(
                                sc as u32, keycode.map(|k| k.into_i32() as u32).unwrap_or(0),
                                true, keymod.bits() as u16
                            ) {
                                // TODO: send KeyEvent over TCP control channel
                            }
                        }
                        Event::KeyUp { scancode: Some(sc), keycode, keymod, .. } => {
                            if let Some(_key_out) = input.process_key(
                                sc as u32, keycode.map(|k| k.into_i32() as u32).unwrap_or(0),
                                false, keymod.bits() as u16
                            ) {
                                // TODO: send KeyEvent over TCP control channel
                            }
                        }
                        Event::MouseMotion { x, y, .. } => {
                            input.send_mouse_move(x, y);
                        }
                        Event::MouseButtonDown { x, y, mouse_btn, .. } => {
                            let btn = match mouse_btn {
                                sdl2::mouse::MouseButton::Left => 0,
                                sdl2::mouse::MouseButton::Right => 1,
                                sdl2::mouse::MouseButton::Middle => 2,
                                _ => 0,
                            };
                            input.send_mouse_down(x, y, btn);
                        }
                        Event::MouseButtonUp { x, y, mouse_btn, .. } => {
                            let btn = match mouse_btn {
                                sdl2::mouse::MouseButton::Left => 0,
                                sdl2::mouse::MouseButton::Right => 1,
                                sdl2::mouse::MouseButton::Middle => 2,
                                _ => 0,
                            };
                            input.send_mouse_up(x, y, btn);
                        }
                        Event::MouseWheel { x, y, .. } => {
                            input.send_scroll(x as i16, y as i16);
                        }
                        _ => {}
                    }
                }

                // Handle grab/release hotkeys
                if input.grab_pending {
                    input.grab_pending = false;
                    input.grab();
                    sdl.mouse().set_relative_mouse_mode(true); // grab mouse
                }
                if input.release_pending {
                    input.release_pending = false;
                    input.release();
                    sdl.mouse().set_relative_mouse_mode(false); // release mouse
                }

                // Only re-render when something changed (not every 8ms)
                if has_frame {
                    // Check if cursor moved
                    let cx = cursor_state_reader.x.load(Ordering::Relaxed);
                    let cy = cursor_state_reader.y.load(Ordering::Relaxed);
                    let cs = cursor_state_reader.shape.load(Ordering::Relaxed) as u8;

                    let cursor_moved = cx != cursor_renderer.x || cy != cursor_renderer.y;
                    let need_render = new_video_frame || cursor_moved;

                    if need_render {
                        if let Some(ref mut r) = renderer_opt {
                            if input.ownership == input_capture::InputOwnership::RemoteControlGrabbed {
                                let mouse = event_pump.mouse_state();
                                cursor_renderer.update(mouse.x(), mouse.y(), 0);
                            } else {
                                if cx >= 0 && cy >= 0 {
                                    cursor_renderer.update(cx, cy, cs);
                                } else {
                                    cursor_renderer.visible = false;
                                }
                            }
                            r.present_with_cursor(&cursor_renderer);
                        }
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
