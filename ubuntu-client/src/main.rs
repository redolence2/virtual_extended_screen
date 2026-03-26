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

    /// Dump received frames to a .h264 file for validation
    #[arg(long)]
    dump_h264: Option<String>,
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

    // 4. Wait for StartStreaming, reply StreamingReady
    control.wait_for_start_streaming(mode_confirm.stream_id, mode_confirm.config_id).await?;

    // 5. Start video receiver
    let video_port = mode_confirm.video_port as u16;
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

    // 6. Frame consumer (Phase 3: log stats; Phase 4: decode+render)
    let dump_path = args.dump_h264.clone();
    let _consumer_handle = std::thread::Builder::new()
        .name("frame-consumer".into())
        .spawn(move || {
            let mut file = dump_path.as_ref().map(|p| {
                std::fs::File::create(p).expect("Failed to create dump file")
            });
            let mut count = 0u64;
            let mut bytes = 0u64;
            let start = std::time::Instant::now();

            while let Ok(frame) = frame_rx.recv() {
                count += 1;
                bytes += frame.data.len() as u64;

                if let Some(ref mut f) = file {
                    use std::io::Write;
                    f.write_all(&frame.data).ok();
                }

                if count == 1 {
                    log::info!(
                        "First frame: {}x{}, {}B, keyframe={}, ts={}us",
                        frame.width, frame.height, frame.data.len(),
                        frame.is_keyframe, frame.timestamp_us
                    );
                }

                if count % 60 == 0 {
                    let elapsed = start.elapsed().as_secs_f64();
                    let fps = count as f64 / elapsed;
                    log::info!(
                        "Frames: {}, {:.1} fps, {:.1} MB received",
                        count, fps, bytes as f64 / 1_048_576.0
                    );
                }
            }

            log::info!("Frame consumer: {} frames, {} bytes total", count, bytes);
        })?;

    log::info!("Streaming active. Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    log::info!("Shutting down...");

    // Cleanup happens when threads are dropped
    Ok(())
}
