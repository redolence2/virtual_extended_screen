//! Shared decoder-backend construction and emit/drain primitives for the two
//! closed `decoder_backend` configuration IDs (`docs/WIRE.md` §7:
//! `cuvid-lowdelay` / `sw1-lowdelay`). Extracted from the original
//! `decoder-experiment` binary (IMPLEMENTATION_PLAN_V11.md §12 "A0.0 decoder
//! experiment") so `harness-receiver` (§12 "A0 measurement harness") and the
//! client doctor (§11.4) can open byte-identical decoders and share the same
//! emit-path primitives without duplicating the native construction
//! sequence or its ERR-03 recovery helpers.
//!
//! [`open_decoder`] is the only construction entry point: given a
//! `decoder_backend` string, it either returns a working [`DecoderHandle`]
//! constructed byte-for-byte per the WIRE.md §7 table, or an error that
//! downcasts to [`BackendOpenError`] identifying which native call (or which
//! invalid id) failed. There is no fallback between the two IDs, or to any
//! third option, ever (`CONTRACT_ERRATA.md` ERR-02).

use anyhow::Result;

/// Owns the opened decoder plus (for `cuvid-lowdelay`) the CUDA hw device
/// context buffer ref, freed on drop. Mirrors the ownership pattern
/// `crates/video-decode/src/lib.rs` uses for its own (independent) decoder.
pub struct DecoderHandle {
    /// The opened FFmpeg decoder context — `send_packet`/`receive_frame`
    /// (via [`receive_one`]) drive it directly.
    pub decoder: ffmpeg_next::decoder::Video,
    hw_device_ctx: Option<*mut ffmpeg_sys_next::AVBufferRef>,
    /// True for `cuvid-lowdelay`. Callers must run every emitted frame
    /// through [`transfer_hw_frame`] before reading its pixel data when this
    /// is set (docs/WIRE.md §7's GPU→CPU `av_hwframe_transfer_data` step) —
    /// CPU-backed frames from `sw1-lowdelay` need no such transfer.
    pub is_hw: bool,
}

impl Drop for DecoderHandle {
    fn drop(&mut self) {
        if let Some(ref mut ctx) = self.hw_device_ctx {
            unsafe {
                ffmpeg_sys_next::av_buffer_unref(ctx);
            }
        }
    }
}

/// A native-call (or invalid-id) failure from [`open_decoder`], carrying
/// enough structure for callers to build a `REQUIRED_NATIVE_API` report
/// (docs/WIRE.md §7) without re-parsing the anyhow chain: `failing_call` is
/// the C API name (or `"decoder_backend"` for an id-validation failure) and
/// `detail` is the failure text.
#[derive(Debug, Clone)]
pub struct BackendOpenError {
    pub failing_call: String,
    pub detail: String,
}

impl std::fmt::Display for BackendOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.failing_call, self.detail)
    }
}

impl std::error::Error for BackendOpenError {}

/// Opens the decoder for `backend_id` exactly per the `docs/WIRE.md` §7 row.
/// `backend_id` must be exactly `"cuvid-lowdelay"` or `"sw1-lowdelay"` — the
/// two closed configuration IDs. The A0.0 placeholder `"TBD-A00"` and any
/// other id are rejected with the `CONTRACT_ERRATA.md` ERR-02 wording; there
/// is no fallback between the two real IDs, or to any third option, ever.
/// On failure, `err.downcast_ref::<BackendOpenError>()` gives the failing
/// call name + detail.
pub fn open_decoder(backend_id: &str) -> Result<DecoderHandle> {
    let result = match backend_id {
        "cuvid-lowdelay" => build_cuvid(),
        "sw1-lowdelay" => build_sw1(),
        "TBD-A00" => Err((
            "decoder_backend".to_string(),
            "\"TBD-A00\" is the docs/WIRE.md §9 canonical-profile placeholder; \
             it is accepted only by the placeholder canonicalization fixture \
             and A0.0 measurement tooling. A normal handshake or final-profile \
             doctor rejects it; the decoder doctor takes one explicit \
             candidate from the two closed IDs (cuvid-lowdelay, sw1-lowdelay) \
             and logs it (CONTRACT_ERRATA.md ERR-02)."
                .to_string(),
        )),
        other => Err((
            "decoder_backend".to_string(),
            format!(
                "unknown decoder_backend '{other}'; must be cuvid-lowdelay or \
                 sw1-lowdelay (the two closed docs/WIRE.md §7 IDs) — no \
                 fallback between them or to any third option \
                 (CONTRACT_ERRATA.md ERR-02)"
            ),
        )),
    };
    result.map_err(|(failing_call, detail)| BackendOpenError { failing_call, detail }.into())
}

fn build_cuvid() -> std::result::Result<DecoderHandle, (String, String)> {
    // CUDA hw device context, `av_hwdevice_ctx_create` defaults.
    let mut hw_device_ctx: *mut ffmpeg_sys_next::AVBufferRef = std::ptr::null_mut();
    let ret = unsafe {
        ffmpeg_sys_next::av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            ffmpeg_sys_next::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret < 0 {
        return Err(("av_hwdevice_ctx_create".to_string(), format!("ret={ret}")));
    }

    // avcodec_find_decoder_by_name("hevc_cuvid")
    let name = "hevc_cuvid\0";
    let codec_ptr =
        unsafe { ffmpeg_sys_next::avcodec_find_decoder_by_name(name.as_ptr() as *const i8) };
    if codec_ptr.is_null() {
        unsafe {
            ffmpeg_sys_next::av_buffer_unref(&mut hw_device_ctx);
        }
        return Err((
            "avcodec_find_decoder_by_name".to_string(),
            "hevc_cuvid decoder not found".to_string(),
        ));
    }

    let codec = unsafe { ffmpeg_next::codec::codec::Codec::wrap(codec_ptr as *mut _) };
    let mut context = ffmpeg_next::codec::context::Context::new_with_codec(codec);

    unsafe {
        let ctx_ptr = context.as_mut_ptr();
        (*ctx_ptr).hw_device_ctx = ffmpeg_sys_next::av_buffer_ref(hw_device_ctx);
        // AV_CODEC_FLAG_LOW_DELAY set before open.
        (*ctx_ptr).flags |= ffmpeg_sys_next::AV_CODEC_FLAG_LOW_DELAY as i32;
        // Explicit surface pool size.
        (*ctx_ptr).extra_hw_frames = 8;
    }

    let decoder = match context.decoder().video() {
        Ok(d) => d,
        Err(e) => {
            unsafe {
                ffmpeg_sys_next::av_buffer_unref(&mut hw_device_ctx);
            }
            return Err(("avcodec_open2".to_string(), e.to_string()));
        }
    };

    Ok(DecoderHandle {
        decoder,
        hw_device_ctx: Some(hw_device_ctx),
        is_hw: true,
    })
}

fn build_sw1() -> std::result::Result<DecoderHandle, (String, String)> {
    // find_decoder_by_name("hevc") — safe wrapper around
    // avcodec_find_decoder_by_name.
    let codec = ffmpeg_next::decoder::find_by_name("hevc").ok_or_else(|| {
        (
            "avcodec_find_decoder_by_name".to_string(),
            "hevc decoder not found".to_string(),
        )
    })?;

    let mut context = ffmpeg_next::codec::context::Context::new_with_codec(codec);

    // thread_count = 1, thread_type none — no frame-threading.
    context.set_threading(ffmpeg_next::threading::Config {
        kind: ffmpeg_next::threading::Type::None,
        count: 1,
        ..Default::default()
    });

    unsafe {
        let ctx_ptr = context.as_mut_ptr();
        (*ctx_ptr).flags |= ffmpeg_sys_next::AV_CODEC_FLAG_LOW_DELAY as i32;
    }

    let decoder = context
        .decoder()
        .video()
        .map_err(|e| ("avcodec_open2".to_string(), e.to_string()))?;

    Ok(DecoderHandle {
        decoder,
        hw_device_ctx: None,
        is_hw: false,
    })
}

/// Outcome of one classified `receive_frame` call.
pub enum ReceiveOutcome {
    /// A frame was written into the caller's frame buffer.
    Frame,
    /// `EAGAIN` — the decoder needs more input before it can emit again.
    Again,
    /// Clean end of stream.
    Eof,
}

/// Calls `decoder.receive_frame(frame)` once and classifies the result
/// (docs/WIRE.md §7/§8): `Ok(())` ⇒ [`ReceiveOutcome::Frame`] (written into
/// `frame`); `EAGAIN` ⇒ [`ReceiveOutcome::Again`]; a real
/// `ffmpeg_next::Error::Eof` ⇒ [`ReceiveOutcome::Eof`]. Any other error is
/// returned as `Err` with a formatted detail string — per
/// `IMPLEMENTATION_PLAN_V11.md` §7, a real drain error is always fatal to
/// the run, so this never tries to classify further.
pub fn receive_one(
    decoder: &mut ffmpeg_next::decoder::Video,
    frame: &mut ffmpeg_next::frame::Video,
) -> std::result::Result<ReceiveOutcome, String> {
    match decoder.receive_frame(frame) {
        Ok(()) => Ok(ReceiveOutcome::Frame),
        Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
            Ok(ReceiveOutcome::Again)
        }
        Err(ffmpeg_next::Error::Eof) => Ok(ReceiveOutcome::Eof),
        Err(e) => Err(format!("receive_frame fatal error: {e}")),
    }
}

/// GPU→CPU transfer for the `cuvid-lowdelay` backend (docs/WIRE.md §7):
/// `av_hwframe_transfer_data` into a fresh software frame. Only needed when
/// [`DecoderHandle::is_hw`] is true. A transfer failure means the backend
/// did not actually produce usable output, so it is always fatal — callers
/// should treat `Err` the same as any other drain error.
pub fn transfer_hw_frame(
    frame: &ffmpeg_next::frame::Video,
) -> std::result::Result<ffmpeg_next::frame::Video, String> {
    let mut sw_frame = ffmpeg_next::frame::Video::empty();
    let ret = unsafe {
        ffmpeg_sys_next::av_hwframe_transfer_data(sw_frame.as_mut_ptr(), frame.as_ptr(), 0)
    };
    if ret < 0 {
        return Err(format!(
            "av_hwframe_transfer_data failed (ret={ret}) for pts={:?}",
            frame.pts()
        ));
    }
    Ok(sw_frame)
}

/// ERR-03 ordinal recovery (docs/WIRE.md §8): `AVFrame.pts`, falling back to
/// `best_effort_timestamp`.
pub fn recovered_ordinal(frame: &ffmpeg_next::frame::Video) -> Option<i64> {
    frame.pts().or_else(|| frame.timestamp())
}
