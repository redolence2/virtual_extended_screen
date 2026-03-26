use anyhow::Result;
use clap::Parser;
use jitter_buffer::AssembledFrame;
use std::sync::mpsc;
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
    let max_chunks = mode_confirm.max_total_chunks_per_frame as u16;
    let max_frame = mode_confirm.max_frame_bytes;
    let stream_id = mode_confirm.stream_id;
    let config_id = mode_confirm.config_id;

    // Frame channel: receiver → decode/render (cap=2, latest wins)
    let (frame_tx, frame_rx) = mpsc::sync_channel::<AssembledFrame>(2);

    let _recv_handle = std::thread::Builder::new()
        .name("video-recv".into())
        .spawn(move || {
            let mut receiver = net_transport::video_receiver::VideoReceiver::new(
                video_port, stream_id, config_id, max_chunks, max_frame,
            ).expect("Failed to create video receiver");
            receiver.run(frame_tx);
        })?;

    // 6. Decode + render thread (must run on main thread for SDL2 on some platforms)
    let dump_path = args.dump_h264.clone();
    let headless = args.headless;
    let display_idx = args.display;
    let no_flash = args.no_flash;

    // SDL2 must be on the main thread on Linux/Xorg, so we spawn tokio work elsewhere
    // and keep decode+render on the current thread after tokio yields.
    let decode_render_handle = std::thread::Builder::new()
        .name("decode-render".into())
        .spawn(move || {
            // Init decoder
            let mut decoder = match video_decode::H264Decoder::new() {
                Ok(d) => d,
                Err(e) => { log::error!("Decoder init failed: {}", e); return; }
            };

            // Init renderer (unless headless)
            let mut renderer = if !headless {
                match renderer::Renderer::new(display_idx, stream_width, stream_height, !no_flash) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        log::warn!("Renderer init failed: {} (continuing headless)", e);
                        None
                    }
                }
            } else {
                None
            };

            // H.264 dump file
            let mut dump_file = dump_path.as_ref().map(|p| {
                std::fs::File::create(p).expect("Failed to create dump file")
            });

            let start = std::time::Instant::now();
            let mut frame_count = 0u64;
            let mut decode_total_us = 0u64;

            while let Ok(assembled) = frame_rx.recv() {
                // Dump raw H.264 if requested
                if let Some(ref mut f) = dump_file {
                    use std::io::Write;
                    f.write_all(&assembled.data).ok();
                }

                // Decode
                let decode_start = std::time::Instant::now();
                match decoder.decode(&assembled.data, assembled.timestamp_us) {
                    Ok(decoded_frames) => {
                        let decode_us = decode_start.elapsed().as_micros() as u64;
                        decode_total_us += decode_us;

                        for decoded in &decoded_frames {
                            frame_count += 1;

                            // Render
                            if let Some(ref mut r) = renderer {
                                if let Err(e) = r.render_frame(decoded) {
                                    log::warn!("Render error: {}", e);
                                }
                            }

                            if frame_count == 1 {
                                log::info!(
                                    "First decoded frame: {}x{}, decode {:.1}ms",
                                    decoded.width, decoded.height,
                                    decode_us as f64 / 1000.0
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Decode error: {}", e);
                    }
                }

                if frame_count > 0 && frame_count % 60 == 0 {
                    let elapsed = start.elapsed().as_secs_f64();
                    let fps = frame_count as f64 / elapsed;
                    let avg_decode_ms = (decode_total_us as f64 / frame_count as f64) / 1000.0;
                    log::info!(
                        "Decoded: {} frames, {:.1} fps, avg decode {:.1}ms",
                        frame_count, fps, avg_decode_ms
                    );
                }
            }

            log::info!("Decode/render stopped: {} frames total", frame_count);
        })?;

    log::info!("Streaming active. Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    log::info!("Shutting down...");
    drop(decode_render_handle);

    Ok(())
}
