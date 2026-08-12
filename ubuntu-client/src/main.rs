mod doctor;

use anyhow::Result;
use clap::Parser;
use jitter_buffer::AssembledFrame;
use protocol::binary::{CursorUpdate, PacketPrefix};
use protocol::constants::*;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default `--sample` path: `$HOME/resc/sample_1080x1920.h265`. A function
/// (not a string literal) because clap's `default_value_t` evaluates it at
/// parse time, so `$HOME` is read from this process's actual environment
/// rather than baked in at compile time.
fn default_sample_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/resc/sample_1080x1920.h265")
}

/// Set (relaxed store only — the only thing safe to do from a signal
/// handler) by [`handle_sigterm`]; polled by the decode-render loop to
/// trigger the trace-mode clean-shutdown sequence
/// (`A00_COMPLETION_REPORT_AMENDED_response_review.md` amendment 3 /
/// `A00_COMPLETION_REPORT_AMENDED_response.md` v2 §2 F4.3). Kept
/// dependency-free (`libc::signal` + `AtomicBool`, no `signal-hook`) per the
/// FROZEN DESIGN — without this, SIGTERM keeps its default disposition
/// (instant kill), which is exactly why `tools/r4_live_gate.sh`'s old
/// `pkill`/`kill` termination used to truncate the trace file mid-write.
static SIGTERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_sig: libc::c_int) {
    SIGTERM_RECEIVED.store(true, Ordering::Relaxed);
}

/// True only while the decode-render thread's main loop could actually be
/// polling `SIGTERM_RECEIVED` itself (once per ~1ms iteration) — i.e. while
/// a [`DecodeLoopAliveGuard`] is alive. False before that loop starts
/// (still resolving mDNS/connecting/negotiating — none of those network
/// calls have a built-in timeout, so they can block indefinitely) and after
/// it ends (e.g. the SDL window was closed, or a decoder-init failure
/// returned early). This is not part of the FROZEN DESIGN's trace-mode
/// shutdown sequence itself; it exists because installing a SIGTERM handler
/// at all (required so that sequence can run) overrides SIGTERM's default
/// kill-immediately disposition for the *whole* process lifetime — without
/// this flag plus the fallback watchdog task in `main()`, a client stuck in
/// one of those windows would become unkillable short of SIGKILL, which is
/// strictly worse than pre-C3 behavior in a window the FROZEN DESIGN never
/// actually needed to change (there is no ledger/trace state to gracefully
/// flush in either window — nothing has started yet, or the loop's own exit
/// path already ran).
static DECODE_LOOP_ALIVE: AtomicBool = AtomicBool::new(false);

/// RAII handle for [`DECODE_LOOP_ALIVE`]: sets it true on construction, and
/// — critically — resets it to false on every exit from the decode-render
/// loop's scope, including an early `return` (SDL quit / Ctrl+Alt+Q) or an
/// unexpected panic unwind, not just normal completion. A manual reset at
/// each return site would miss any path added later; `Drop` cannot.
struct DecodeLoopAliveGuard;

impl DecodeLoopAliveGuard {
    fn new() -> Self {
        DECODE_LOOP_ALIVE.store(true, Ordering::Relaxed);
        DecodeLoopAliveGuard
    }
}

impl Drop for DecodeLoopAliveGuard {
    fn drop(&mut self) {
        DECODE_LOOP_ALIVE.store(false, Ordering::Relaxed);
    }
}

/// Hard cap on the decode-side receipt ledger (below). A capacity overflow
/// is an identity failure, never a silent evict.
const IDENTITY_LEDGER_CAP: usize = 1024;

/// Decode-side receipt ledger (`A00_COMPLETION_REPORT_AMENDED_review.md`
/// finding 1 / `A00_COMPLETION_REPORT_AMENDED_response_review.md` amendment
/// 2): submits one admitted frame's `frame_id -> recv_ts_us` receipt
/// immediately before it reaches the decoder. `recv_ts_us` itself stays
/// stamped at assembly completion in `net-transport`'s `VideoReceiver`
/// (untouched here) — this ledger only remembers that stamp long enough for
/// the emission that eventually recovers `frame_id`'s PTS to look it back
/// up by identity, rather than inheriting whatever frame_id happened to
/// trigger the `decode()` call that emitted it (the exact bug the review
/// caught: "Frame 0 is paired with frame 4's assembly-completion
/// timestamp"). A duplicate wire `frame_id` already pending, or a full
/// ledger, is an identity failure (counted and traced) rather than a silent
/// overwrite/evict — the run is invalidated via the trace footer, never by
/// exiting mid-loop. Queue-dropped frames (the intentional non-keyframe
/// drop in `video_receiver.rs`) never reach this function at all, so they
/// never create a ledger entry. No-op when tracing is disabled — non-trace
/// mode pays no bookkeeping cost and its behavior is unchanged.
fn ledger_submit(
    ledger: &mut HashMap<u64, u64>,
    trace: &diagnostics::trace::ClientTrace,
    identity_failures: &mut u64,
    assembled: &AssembledFrame,
) {
    if !trace.enabled() {
        return;
    }
    let key = assembled.frame_id as u64;
    if ledger.contains_key(&key) {
        *identity_failures += 1;
        trace.identity_failure(serde_json::json!({
            "reason": "duplicate_submit",
            "wire_frame_id": assembled.frame_id,
            "recovered_frame_id": null,
        }));
    } else if ledger.len() >= IDENTITY_LEDGER_CAP {
        *identity_failures += 1;
        trace.identity_failure(serde_json::json!({
            "reason": "cap_overflow",
            "wire_frame_id": assembled.frame_id,
            "recovered_frame_id": null,
        }));
    } else {
        ledger.insert(key, assembled.recv_ts_us);
    }
}

/// Resolves one emitted decoded frame's receipt from the ledger populated by
/// [`ledger_submit`] and writes its trace record. The `frame` record's
/// `ts_recv_us` is the LEDGER value keyed by this emission's OWN
/// `recovered_frame_id` — never `decode_trigger_frame_id` (the current
/// `decode()` call's input id, carried through informationally only; a
/// record whose trigger differs from its recovered identity still joins
/// with its ledger-correct `ts_recv_us` — see `tools/join_trace.py`'s
/// `--selftest`). A missing `recovered_frame_id`, or one absent from the
/// ledger (already resolved, or never submitted), is an identity failure —
/// counted and traced instead of a `frame` record, so a mismatched pairing
/// can never silently appear as a normal join. No-op when tracing is
/// disabled.
fn ledger_resolve(
    ledger: &mut HashMap<u64, u64>,
    trace: &diagnostics::trace::ClientTrace,
    identity_failures: &mut u64,
    decode_trigger_frame_id: u32,
    recovered_frame_id: Option<u64>,
) {
    if !trace.enabled() {
        return;
    }
    match recovered_frame_id {
        None => {
            *identity_failures += 1;
            trace.identity_failure(serde_json::json!({
                "reason": "unrecovered_pts",
                "wire_frame_id": decode_trigger_frame_id,
                "recovered_frame_id": null,
            }));
        }
        Some(rid) => match ledger.remove(&rid) {
            Some(ts_recv_us) => {
                trace.frame(serde_json::json!({
                    "recovered_frame_id": rid,
                    "decode_trigger_frame_id": decode_trigger_frame_id,
                    "ts_recv_us": ts_recv_us,
                    "ts_decode_done_us": diagnostics::mono_us(),
                }));
            }
            None => {
                *identity_failures += 1;
                trace.identity_failure(serde_json::json!({
                    "reason": "missing_ledger_entry",
                    "wire_frame_id": decode_trigger_frame_id,
                    "recovered_frame_id": rid,
                }));
            }
        },
    }
}

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

    /// Run the client doctor (IMPLEMENTATION_PLAN_V11.md §11.4) and exit —
    /// probes environment/decoder/SDL without starting real streaming.
    #[arg(long)]
    doctor: bool,

    /// Decoder backend candidate for `--doctor`'s backend_open/decode_sample
    /// checks (docs/WIRE.md §7). One explicit candidate, no fallback
    /// (CONTRACT_ERRATA.md ERR-02).
    #[arg(long, default_value = "sw1-lowdelay")]
    doctor_backend: String,

    /// Bundled HEVC AU sample used by `--doctor`'s decode_sample check.
    #[arg(long, default_value_t = default_sample_path())]
    sample: String,
}

// Re-export from net-transport (single source of truth for shared stats)
use net_transport::video_receiver::SharedReceiverStats;

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
    protocol::constants::log_and_verify();

    // Install the SIGTERM handler before anything else, so a graceful-
    // termination request (tools/r4_live_gate.sh) is never lost to the
    // default kill-immediately disposition — see SIGTERM_RECEIVED's doc
    // comment. A bare flag set; the decode-render loop polls it and does
    // the actual shutdown work.
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as *const () as libc::sighandler_t);
    }

    // Fallback watchdog for DECODE_LOOP_ALIVE's two uncovered windows (see
    // its doc comment): if SIGTERM arrives while nothing else is polling
    // it, exit immediately — matching the pre-C3 default disposition —
    // rather than leaving the process unkillable short of SIGKILL now that
    // installing the handler above has overridden that default. Runs for
    // the process lifetime; negligible cost (one atomic load per 100ms)
    // once the decode-render loop is up and handling SIGTERM itself.
    tokio::spawn(async {
        loop {
            if SIGTERM_RECEIVED.load(Ordering::Relaxed) && !DECODE_LOOP_ALIVE.load(Ordering::Relaxed) {
                std::process::exit(0);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let log = diagnostics::RescLog::global();
    diagnostics::environment::emit(log);
    if !diagnostics::instance_lock::acquire("moyunfei-desk-1") {
        eprintln!("RESC client: another instance holds the profile lock — exiting");
        std::process::exit(20); // 20 = INSTANCE_LOCK_HELD (v3 FatalCode)
    }

    let args = Args::parse();

    // `--doctor` mode (IMPLEMENTATION_PLAN_V11.md §11.4): probe-only run,
    // never starts real streaming. Exits here — nothing below this point
    // executes. Mirrors mac-host's main.swift ordering (env emit + instance
    // lock acquired first, then the doctor branch).
    if args.doctor {
        let exit_code = doctor::run(&args.doctor_backend, std::path::Path::new(&args.sample));
        std::process::exit(exit_code);
    }

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
    // Depth 2, not 4 (native-4K trace finding, 2026-08-05: recv->decode
    // p50 117ms while decode itself is 5ms — the channel was a standing
    // latency reservoir whenever the decode/render loop runs below the
    // FramePacer's 60fps). At depth 2 the assembler sheds instead, which
    // costs smoothness, not latency.
    let (frame_tx, frame_rx) = mpsc::sync_channel::<AssembledFrame>(2);

    // Shared stats — receiver updates atomics in real-time, stats reporter reads them
    let recv_stats = Arc::new(SharedReceiverStats::default());
    let recv_stats_reader = recv_stats.clone();
    // A third handle for the decode thread's trace-mode shutdown footer's
    // `queue_drops` field (below): accessible via this same Arc-clone
    // pattern the stats reporter already uses, so included rather than
    // omitted — see the footer-building comment for exactly what it counts.
    let recv_stats_for_decode = recv_stats.clone();

    let _recv_handle = std::thread::Builder::new()
        .name("video-recv".into())
        .spawn(move || {
            let mut receiver = net_transport::video_receiver::VideoReceiver::new(
                video_port, stream_id, config_id, max_chunks, max_frame, recv_stats,
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

    // IDR request channel: decode thread → stats reporter → same control channel
    let (idr_tx, mut idr_rx) = tokio::sync::mpsc::channel::<i32>(4);
    let idr_stream_id = stream_id;
    let idr_config_id = config_id;

    // Warm filter strength (set by Mac's Night Shift via control channel)
    let warm_strength = Arc::new(std::sync::atomic::AtomicU32::new(0)); // f32 bits stored as u32
    let warm_strength_writer = warm_strength.clone();
    let warm_strength_reader = warm_strength.clone();

    // 6b. Bidirectional control channel: stats/IDR out, DisplaySettings in
    tokio::spawn(async move {
        let mut prev_recv = 0u64;
        let mut prev_drop = 0u64;
        let mut prev_f_done = 0u64;
        let mut prev_f_drop = 0u64;
        let mut stats_interval = tokio::time::interval(Duration::from_millis(100));

        // A0.0 trace-mode clock sync (IMPLEMENTATION_PLAN_V11.md §10),
        // additive and only active when RESC_TRACE=1 — see crates/diagnostics
        // trace.rs/clocksync.rs. `clock_ping_interval` always ticks; the send
        // itself and all pong handling are guarded by `trace.enabled()`.
        let trace = diagnostics::trace::ClientTrace::global();
        let mut clock_sync = diagnostics::clocksync::ClockSync::new();
        let mut clock_ping_interval = tokio::time::interval(Duration::from_secs(10));
        let mut clock_ping_seq: u32 = 0;

        loop {
            tokio::select! {
                // Receive messages from host (DisplaySettings for Night Shift;
                // ClockPong for A0.0 trace-mode clock sync)
                msg = control.recv() => {
                    match msg {
                        Ok(envelope) => {
                            match envelope.payload {
                                Some(protocol::resc_control::envelope::Payload::DisplaySettings(ds)) => {
                                    let bits = ds.warm_strength.to_bits();
                                    warm_strength_writer.store(bits, Ordering::Relaxed);
                                    log::info!("Night Shift: warm_strength={:.0}%", ds.warm_strength * 100.0);
                                }
                                Some(protocol::resc_control::envelope::Payload::ClockPong(pong)) if trace.enabled() => {
                                    let t4 = diagnostics::mono_us();
                                    if let Some(sample) = clock_sync.on_pong(
                                        pong.t1_mono_us, pong.t2_mono_us, pong.t3_mono_us, t4, pong.seq
                                    ) {
                                        trace.clock(sample.offset_us, sample.delay_us, sample.uncertainty_us, sample.seq);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            log::warn!("Control recv error: {} (host may have disconnected)", e);
                            break;
                        }
                    }
                }
                // IDR requests from decoder
                reason = idr_rx.recv() => {
                    if let Some(reason) = reason {
                        if let Err(e) = control.send_request_idr(idr_stream_id, idr_config_id, reason).await {
                            log::warn!("IDR request send failed: {}", e);
                        }
                    }
                }
                // A0.0 trace-mode ClockPing, every 10s (RESC_TRACE=1 only)
                _ = clock_ping_interval.tick() => {
                    if trace.enabled() {
                        clock_ping_seq = clock_ping_seq.wrapping_add(1);
                        let t1 = diagnostics::mono_us();
                        if let Err(e) = control.send_clock_ping(t1, clock_ping_seq).await {
                            log::warn!("ClockPing send failed: {}", e);
                        }
                    }
                }
                // Periodic stats send
                _ = stats_interval.tick() => {
                    let recv = recv_stats_reader.packets_received.load(Ordering::Relaxed);
                    let drop = recv_stats_reader.packets_dropped.load(Ordering::Relaxed);
                    let f_done = recv_stats_reader.frames_completed.load(Ordering::Relaxed);
                    let f_drop = recv_stats_reader.frames_dropped.load(Ordering::Relaxed);

                    let int_recv = recv.saturating_sub(prev_recv);
                    let int_drop = drop.saturating_sub(prev_drop);
                    let int_f_done = f_done.saturating_sub(prev_f_done);
                    let int_f_drop = f_drop.saturating_sub(prev_f_drop);

                    if int_recv > 0 || int_drop > 0 || int_f_done > 0 || int_f_drop > 0 {
                        let loss_rate = if int_recv + int_drop > 0 {
                            int_drop as f32 / (int_recv + int_drop) as f32
                        } else { 0.0 };
                        let frame_drop_rate = if int_f_done + int_f_drop > 0 {
                            int_f_drop as f32 / (int_f_done + int_f_drop) as f32
                        } else { 0.0 };

                        prev_recv = recv;
                        prev_drop = drop;
                        prev_f_done = f_done;
                        prev_f_drop = f_drop;

                        if let Err(_) = control.send_stats(loss_rate, frame_drop_rate, 0).await {
                            log::warn!("Stats send failed");
                            break;
                        }
                    }
                }
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

    // Decode and render are SEPARATE threads joined by a newest-wins
    // mailbox (native-4K, 2026-08-06): fused, the ~16.7ms vsync-blocked
    // present serialized with 4K decode capped the loop near 30fps and
    // kept a standing queue upstream (the content-latency the owner felt
    // while the cursor overlay channel stayed instant). Split, decode
    // sustains full rate in parallel with presents — the pattern proven in
    // ubuntu_receiver. SDL stays whole in the render thread (renderer +
    // event pump + input, SDL's one-thread rule); ALL identity/ledger/
    // trace-footer machinery stays in the decode thread (C3 semantics
    // unchanged); presents cross via an atomic for the footer.
    struct FrameMailbox {
        // (newest + its publication instant, closed)
        inner: std::sync::Mutex<(Option<(video_decode::DecodedFrame, std::time::Instant)>, bool)>,
        cvar: std::sync::Condvar,
        /// Attribution (audit review §8.2): frames overwritten before pickup.
        sheds: std::sync::atomic::AtomicU64,
    }
    impl FrameMailbox {
        fn new() -> Self {
            Self {
                inner: std::sync::Mutex::new((None, false)),
                cvar: std::sync::Condvar::new(),
                sheds: std::sync::atomic::AtomicU64::new(0),
            }
        }
        fn put(&self, f: video_decode::DecodedFrame) {
            let mut g = self.inner.lock().unwrap();
            // Newest-wins: an overwritten frame is a shed — smoothness cost
            // only; its identity was already resolved at emission in the
            // decode thread, so the ledger/trace contract is untouched.
            if g.0.is_some() {
                self.sheds.fetch_add(1, Ordering::Relaxed);
            }
            g.0 = Some((f, std::time::Instant::now()));
            self.cvar.notify_one();
        }
        #[allow(dead_code)]
        fn close(&self) {
            let mut g = self.inner.lock().unwrap();
            g.1 = true;
            self.cvar.notify_one();
        }
        /// Newest frame (plus how long it sat between publication and this
        /// pickup — the audit review's mailbox-wait attribution) if one is
        /// (or becomes) available within `wait`; also reports the closed flag.
        fn take_timeout(
            &self,
            wait: Duration,
        ) -> (Option<(video_decode::DecodedFrame, Duration)>, bool) {
            let mut g = self.inner.lock().unwrap();
            if g.0.is_none() && !g.1 {
                let (ng, _timeout) = self.cvar.wait_timeout(g, wait).unwrap();
                g = ng;
            }
            (g.0.take().map(|(f, at)| (f, at.elapsed())), g.1)
        }
    }
    let mailbox = std::sync::Arc::new(FrameMailbox::new());
    let presents_shared = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let render_failures_shared = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let codec_id = mode_confirm.codec as u8;

    // 7a. Decode thread: decoder + receipt ledger + identity + IDR
    // requests + the C3 trace-mode shutdown protocol.
    let _decode_handle = {
        let mailbox = std::sync::Arc::clone(&mailbox);
        let presents_shared = std::sync::Arc::clone(&presents_shared);
        let render_failures_shared = std::sync::Arc::clone(&render_failures_shared);
        std::thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
            let mut decoder = match video_decode::VideoDecoder::new(codec_id) {
                Ok(d) => d,
                Err(e) => {
                    // Fail fast — a silent H.264 decoder on a negotiated HEVC
                    // stream decodes nothing and presents as a black screen
                    // (W0 review §3: any such fallback invalidates the
                    // session). A thread-local `return` would also leave the
                    // rest of the client running headless.
                    log::error!(
                        "FATAL: decoder init failed for negotiated codec {} ({}): {} — exiting",
                        codec_id,
                        if codec_id == 1 { "HEVC" } else { "H.264" },
                        e
                    );
                    std::process::exit(1);
                }
            };

            let mut dump_file = dump_path.as_ref().map(|p| {
                std::fs::File::create(p).expect("Failed to create dump file")
            });

            let start = std::time::Instant::now();
            let mut frame_count = 0u64;
            let mut decode_total_us = 0u64;
            // A0.0 trace-mode per-frame log (RESC_TRACE=1 only; see
            // crates/diagnostics/src/trace.rs). Cheap no-op otherwise.
            let trace = diagnostics::trace::ClientTrace::global();

            // C3 decode-side receipt ledger + counters (see `ledger_submit`/
            // `ledger_resolve`'s doc comments): all trace-mode-only
            // bookkeeping.
            let mut ledger: HashMap<u64, u64> = HashMap::new();
            let mut identity_failures: u64 = 0;
            let mut decode_submitted: u64 = 0;
            // Last frame_id actually submitted to the decoder — used only as
            // `decode_trigger_frame_id` for tail frames recovered by
            // `decoder.flush()` during shutdown, which (being triggered by
            // EOF, not a fresh wire packet) has no input id of its own.
            let mut last_submitted_frame_id: u32 = 0;

            // From here on, this loop itself is polling SIGTERM_RECEIVED
            // every ~1ms (below) — hand shutdown-watch duty over from
            // main()'s fallback task for as long as that remains true. See
            // DECODE_LOOP_ALIVE's doc comment.
            let _decode_loop_alive_guard = DecodeLoopAliveGuard::new();

            loop {
                // Collect ALL available frames from the queue, DECODE all
                // (maintains the reference chain); every renderable emission
                // is published to the newest-wins mailbox.
                let mut frames_to_decode: Vec<AssembledFrame> = Vec::new();
                let mut shutting_down = false;
                match frame_rx.recv_timeout(Duration::from_millis(1)) {
                    Ok(frame) => {
                        frames_to_decode.push(frame);
                        while let Ok(more) = frame_rx.try_recv() {
                            frames_to_decode.push(more);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        log::info!("Frame channel disconnected");
                        shutting_down = true;
                    }
                }
                if SIGTERM_RECEIVED.load(Ordering::Relaxed) {
                    shutting_down = true;
                }

                // C3 trace-mode clean/aborted shutdown protocol
                // (A00_COMPLETION_REPORT_AMENDED_response_review.md
                // amendment 3 / A00_COMPLETION_REPORT_AMENDED_response.md v2
                // §2 F4.3): triggered by SIGTERM or the frame channel
                // disconnecting. Stop taking new frames, drain whatever is
                // already admitted, flush the decoder's tail through the
                // same ledger/emission path, then seal the trace with a
                // clean/aborted footer. Exits the process directly — SIGTERM
                // handling here bypasses main()'s SIGINT-only
                // `ctrl_c().await`, so this thread returning on its own
                // would leave the rest of the process running. Non-trace
                // mode's observable behavior is unchanged (prompt
                // termination, no draining/flush/footer) even though the
                // mechanism is now this explicit exit rather than the
                // (now-overridden) default SIGTERM disposition.
                if shutting_down {
                    if trace.enabled() {
                        log::info!("Trace-mode clean shutdown: draining admitted queue");
                        // Whatever recv_timeout already pulled this
                        // iteration (if any) is the start of "whatever is
                        // already admitted"; drain the rest non-blockingly —
                        // never wait for a frame that hasn't arrived yet.
                        while let Ok(more) = frame_rx.try_recv() {
                            frames_to_decode.push(more);
                        }
                        for assembled in &frames_to_decode {
                            decode_submitted += 1;
                            last_submitted_frame_id = assembled.frame_id;
                            ledger_submit(&mut ledger, trace, &mut identity_failures, assembled);
                            match decoder.decode(
                                &assembled.data, assembled.frame_id, assembled.timestamp_us, assembled.is_keyframe,
                            ) {
                                Ok(decoded_frames) => {
                                    for decoded in &decoded_frames {
                                        // Identity FIRST, render-skip second (C7 gate
                                        // finding 3, 2026-08-05): a suppressed emission
                                        // (empty planes — the legacy gray/corrupt filter)
                                        // still consumed its receipt; skipping before the
                                        // resolve stranded 29 entries as phantom
                                        // pending_identities and aborted the footer.
                                        frame_count += 1;
                                        ledger_resolve(
                                            &mut ledger, trace, &mut identity_failures,
                                            assembled.frame_id, decoded.recovered_frame_id,
                                        );
                                        if decoded.planes[0].is_empty() { continue; }
                                    }
                                }
                                Err(e) => log::warn!("Shutdown drain decode error: {}", e),
                            }
                        }

                        log::info!("Trace-mode clean shutdown: decoder EOF/tail flush");
                        match decoder.flush() {
                            Ok(tail_frames) => {
                                for decoded in &tail_frames {
                                    // Identity first, render-skip second — see the
                                    // drain loop above (C7 gate finding 3).
                                    frame_count += 1;
                                    ledger_resolve(
                                        &mut ledger, trace, &mut identity_failures,
                                        last_submitted_frame_id, decoded.recovered_frame_id,
                                    );
                                    if decoded.planes[0].is_empty() { continue; }
                                }
                            }
                            Err(e) => log::warn!("Decoder flush failed: {}", e),
                        }

                        let pending_identities = ledger.len() as u64;
                        let status = if pending_identities == 0 && identity_failures == 0 { "clean" } else { "aborted" };
                        trace.finish(status, serde_json::json!({
                            "pending_identities": pending_identities,
                            "identity_failures": identity_failures,
                            // Aggregate assembler-level drop count (timeout/
                            // oversize/eviction) PLUS the queue-full drop —
                            // net-transport/jitter-buffer (out of C3's
                            // touch scope) expose only this combined
                            // counter, not a queue-specific one.
                            "queue_drops": recv_stats_for_decode.frames_dropped.load(Ordering::Relaxed),
                            "decode_submitted": decode_submitted,
                            "decode_emitted": frame_count,
                            // Written by the render thread at the actual
                            // present call site; read once here at seal time.
                            "presents": presents_shared.load(Ordering::Relaxed),
                        }));
                        log::info!(
                            "Trace-mode shutdown complete: status={} pending_identities={} identity_failures={} render_failures={}",
                            status, pending_identities, identity_failures,
                            render_failures_shared.load(Ordering::Relaxed)
                        );
                        std::process::exit(if status == "clean" { 0 } else { 1 });
                    } else {
                        log::info!("Decode stopped (shutdown signal): {} frames", frame_count);
                        std::process::exit(0);
                    }
                }

                if !frames_to_decode.is_empty() {
                    for assembled in frames_to_decode.iter() {
                        if let Some(ref mut f) = dump_file {
                            use std::io::Write;
                            f.write_all(&assembled.data).ok();
                        }

                        // Decode-side receipt ledger (A00_COMPLETION_REPORT_AMENDED_review.md
                        // finding 1 / A00_COMPLETION_REPORT_AMENDED_response_review.md
                        // amendment 2): the ledger entry for THIS admitted
                        // frame goes in immediately before it reaches the
                        // decoder — see ledger_submit's doc comment.
                        decode_submitted += 1;
                        last_submitted_frame_id = assembled.frame_id;
                        ledger_submit(&mut ledger, trace, &mut identity_failures, assembled);

                        let decode_start = std::time::Instant::now();
                        match decoder.decode(&assembled.data, assembled.frame_id, assembled.timestamp_us, assembled.is_keyframe) {
                            Ok(decoded_frames) => {
                                // Check for pending IDR request from decoder
                                if let Some(reason) = decoder.pending_idr_reason.take() {
                                    let reason_code = match reason {
                                        video_decode::IDRReason::DecodeError => 2,
                                        video_decode::IDRReason::CorruptFrame => 2,
                                        video_decode::IDRReason::ReferenceLoss => 3,
                                    };
                                    let _ = idr_tx.try_send(reason_code);
                                }
                                let decode_us = decode_start.elapsed().as_micros() as u64;
                                decode_total_us += decode_us;

                                for decoded in decoded_frames {
                                    // Identity FIRST, render-skip second (C7 gate
                                    // finding 3, 2026-08-05): a suppressed emission
                                    // (empty planes — the legacy gray/corrupt filter)
                                    // still consumed its receipt-ledger entry; the old
                                    // order stranded every suppressed frame's identity
                                    // as phantom pending_identities.
                                    frame_count += 1;
                                    ledger_resolve(
                                        &mut ledger, trace, &mut identity_failures,
                                        assembled.frame_id, decoded.recovered_frame_id,
                                    );
                                    if decoded.planes[0].is_empty() { continue; }

                                    if frame_count == 1 {
                                        log::info!(
                                            "First decoded frame: {}x{}, decode {:.1}ms",
                                            decoded.width, decoded.height, decode_us as f64 / 1000.0
                                        );
                                    }

                                    // (ledger_resolve for this frame ran at the TOP of
                                    // this loop body, before the suppressed-frame skip —
                                    // one record per EMITTED frame, resolving its OWN
                                    // identity, with decode_trigger_frame_id carried
                                    // separately.)
                                    mailbox.put(decoded);
                                }
                            }
                            Err(e) => {
                                log::warn!("Decode error: {}", e);
                                if let Some(reason) = decoder.pending_idr_reason.take() {
                                    let reason_code = match reason {
                                        video_decode::IDRReason::DecodeError => 2,
                                        video_decode::IDRReason::CorruptFrame => 2,
                                        video_decode::IDRReason::ReferenceLoss => 3,
                                    };
                                    let _ = idr_tx.try_send(reason_code);
                                }
                            }
                        }
                    }

                    if frame_count > 0 && frame_count % 60 == 0 {
                        let elapsed = start.elapsed().as_secs_f64();
                        let fps = frame_count as f64 / elapsed;
                        let avg_ms = (decode_total_us as f64 / frame_count as f64) / 1000.0;
                        log::info!("Decoded: {} frames, {:.1} fps, avg decode {:.1}ms", frame_count, fps, avg_ms);
                    }
                }
            }
        })?
    };

    // 7b. Render + input thread (SDL2 must stay whole on one thread:
    // renderer, event pump, cursor overlay, input capture).
    let _render_handle = {
        let mailbox = std::sync::Arc::clone(&mailbox);
        let presents_shared = std::sync::Arc::clone(&presents_shared);
        let render_failures_shared = std::sync::Arc::clone(&render_failures_shared);
        std::thread::Builder::new()
            .name("render-input".into())
            .spawn(move || {
            let sdl = sdl2::init().expect("SDL init");
            let _video = sdl.video().expect("SDL video");
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

            // Detect xrandr rotation + set canvas dimensions for coordinate mapping
            if let Some(ref r) = renderer_opt {
                let (cw, ch) = r.canvas_size();
                input.canvas_width = cw;
                input.canvas_height = ch;
                if r.is_rotated() {
                    input.rotated = true;
                    cursor_renderer.rotated = true;
                    log::info!("Rotation detected: stream {}x{} → canvas {}x{} (scaled)",
                              stream_width, stream_height, cw, ch);
                }
            }

            let mut has_frame = false;
            // Recovered identity (A00_REMEDIATION_PLAN.md §4 item 7) of the
            // most recently uploaded decoded frame — i.e. whatever
            // `r.update_frame()` last copied into the persistent texture.
            // Read at the present call site (§4 item 8) since
            // `present_with_cursor()` re-presents this same texture on
            // cursor-only redraws too, with no frame reference of its own.
            let mut last_uploaded_recovered_id: Option<u64> = None;
            let trace = diagnostics::trace::ClientTrace::global();
            // Attribution (audit review §8.2): publication→pickup wait.
            let mut mb_wait_us: u64 = 0;
            let mut mb_wait_n: u64 = 0;

            loop {
                // Update warm filter from Night Shift control message
                if let Some(ref mut r) = renderer_opt {
                    let bits = warm_strength_reader.load(Ordering::Relaxed);
                    r.warm_strength = f32::from_bits(bits);
                }

                // Newest decoded frame, waiting at most ~2ms so the event
                // pump and cursor-only redraws stay responsive without video.
                let (taken, closed) = mailbox.take_timeout(Duration::from_millis(2));
                if closed && taken.is_none() {
                    log::info!("Render stopped: decode side closed the mailbox");
                    return;
                }
                let newest = taken.map(|(f, waited)| {
                    mb_wait_us += waited.as_micros() as u64;
                    mb_wait_n += 1;
                    if mb_wait_n % 300 == 0 {
                        log::info!(
                            "Mailbox: pickup wait avg {:.1}ms (n={}), sheds={}",
                            mb_wait_us as f64 / mb_wait_n as f64 / 1000.0,
                            mb_wait_n,
                            mailbox.sheds.load(Ordering::Relaxed)
                        );
                    }
                    f
                });

                let mut new_video_frame = false;
                if let Some(ref decoded) = newest {
                    // A00_COMPLETION_REPORT_AMENDED_review.md finding 1
                    // (presentation-identity bug): new_video_frame and
                    // last_uploaded_recovered_id advance ONLY on a
                    // successful upload — the present record must never
                    // claim a frame whose upload failed.
                    if let Some(ref mut r) = renderer_opt {
                        match r.update_frame(decoded) {
                            Ok(()) => {
                                new_video_frame = true;
                                has_frame = true;
                                last_uploaded_recovered_id = decoded.recovered_frame_id;
                            }
                            Err(e) => {
                                render_failures_shared.fetch_add(1, Ordering::Relaxed);
                                log::warn!("Frame upload failed: {}", e);
                                if trace.enabled() {
                                    trace.render_failure(serde_json::json!({
                                        "recovered_frame_id": decoded.recovered_frame_id,
                                    }));
                                }
                            }
                        }
                    }
                }

                // Process SDL2 events (input capture)
                for event in event_pump.poll_iter() {
                    use sdl2::event::Event;
                    use sdl2::keyboard::Mod;
                    match event {
                        // Quit exits the whole process now (the old fused
                        // thread's `return` also ended decoding; a lone
                        // render-thread return would leave decode running
                        // headless).
                        Event::Quit { .. } => { std::process::exit(0); }
                        // Ctrl+Alt+Q: quit the application (works in fullscreen without terminal)
                        Event::KeyDown { scancode: Some(sc), keymod, .. }
                            if sc == sdl2::keyboard::Scancode::Q
                            && keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD)
                            && keymod.intersects(Mod::LALTMOD | Mod::RALTMOD)
                        => {
                            log::info!("Ctrl+Alt+Q pressed, quitting");
                            std::process::exit(0);
                        }
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

                // Only re-render when something changed
                if has_frame {
                    let cx = cursor_state_reader.x.load(Ordering::Relaxed);
                    let cy = cursor_state_reader.y.load(Ordering::Relaxed);
                    let cs = cursor_state_reader.shape.load(Ordering::Relaxed) as u8;

                    let cursor_moved = cx != cursor_renderer.x || cy != cursor_renderer.y;
                    // Also track local mouse movement
                    let mouse = event_pump.mouse_state();
                    let local_moved = mouse.x() != cursor_renderer.x || mouse.y() != cursor_renderer.y;
                    let need_render = new_video_frame || cursor_moved || local_moved;

                    if need_render {
                        if let Some(ref mut r) = renderer_opt {
                            if input.ownership == input_capture::InputOwnership::RemoteControlGrabbed {
                                cursor_renderer.update(mouse.x(), mouse.y(), 0);
                            } else if cx >= 0 && cy >= 0 {
                                // Mac cursor is on the virtual display — use its position
                                cursor_renderer.update(cx, cy, cs);
                            } else {
                                // Mac cursor not on virtual display — use local mouse as fallback
                                cursor_renderer.update(mouse.x(), mouse.y(), 0);
                            }
                            r.present_with_cursor(&cursor_renderer, new_video_frame);

                            // A00_REMEDIATION_PLAN.md §4 item 8 (schema FROZEN):
                            // stamped immediately adjacent to the successful
                            // presentation call itself — not at update_frame's
                            // upload-scheduling time. Gated on new_video_frame
                            // (not just need_render) so this fires once per
                            // genuinely new presented frame, not on every
                            // cursor-only redraw of the same last-uploaded
                            // texture (present_with_cursor has no frame
                            // argument of its own — cursor moves alone also
                            // reach this call). null recovered_frame_id is
                            // written as-is when identity recovery failed —
                            // the joiner counts that as a rejected sample.
                            if new_video_frame && trace.enabled() {
                                presents_shared.fetch_add(1, Ordering::Relaxed);
                                trace.present(serde_json::json!({
                                    "recovered_frame_id": last_uploaded_recovered_id,
                                    "ts_present_us": diagnostics::mono_us(),
                                }));
                            }
                        }
                    }
                }
            }
        })?
    };

    log::info!("Streaming active. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    log::info!("Shutting down...");
    Ok(())
}
