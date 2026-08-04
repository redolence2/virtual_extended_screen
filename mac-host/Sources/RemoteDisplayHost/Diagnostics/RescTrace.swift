import Foundation
import CryptoKit
import RescCore

/// Per-frame JSONL trace writer for the A0/A0.0 measurement mode
/// (IMPLEMENTATION_PLAN_V11.md §10, §12 A0.0). Entirely gated behind the
/// `RESC_TRACE=1` environment variable — disabled, this type touches no
/// files and every call site is a cheap no-op.
///
/// `RescLog` (RescLog.swift, IMPLEMENTATION_PLAN_V11.md §11.1) is explicit
/// that its JSONL log must **never** carry per-packet records. This file is
/// the deliberate exception: joining capture→encode→send timestamps and
/// answered clock pings one record at a time is the entire point of A0
/// trace mode, and it never runs unless a human opts in via the env var —
/// normal streaming writes nothing here.
final class RescTrace {
    static let shared = RescTrace()

    /// True iff `RESC_TRACE=1` is set in the process environment. Read once
    /// — environment variables do not change over a process's lifetime.
    static let enabled: Bool = ProcessInfo.processInfo.environment["RESC_TRACE"] == "1"

    private static let maxFileBytes: UInt64 = 10 * 1024 * 1024
    private static let maxBackups = 4 // host-trace.jsonl.1 ... host-trace.jsonl.4
    private static let flushCount = 64
    private static let newline = Data([0x0A])

    private let queue = DispatchQueue(label: "com.resc.resctrace")
    private let traceDir: URL
    private let traceFile: URL
    /// Generated once, here in `init()` — see `makeRunToken()`.
    private let runToken: String
    private var fileHandle: FileHandle?
    private var flushTimer: DispatchSourceTimer?
    private var pendingCount = 0
    /// Lifetime counts of `"frame"`/`"pong"` records written by this trace —
    /// queue-confined like `pendingCount`, incremented in
    /// `enqueue(_:kind:)`. Back `counts` and the `finish`/`finishAborted`
    /// footer fields so a shutdown path never needs its own parallel tally
    /// (A00_COMPLETION_REPORT_AMENDED_response.md v2 §2 F4).
    private var framesSentCount: UInt64 = 0
    private var pongsCount: UInt64 = 0

    private init() {
        traceDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/RESC", isDirectory: true)
        traceFile = traceDir.appendingPathComponent("host-trace.jsonl")
        runToken = Self.makeRunToken()
        guard Self.enabled else { return }
        openForAppend()
        scheduleFlushTimer()
    }

    /// Stable per-process identifier for this trace, written into the
    /// `trace_complete` footer's `run_token` field so a live-gate joiner can
    /// tell which host process instance produced a given footer
    /// (A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 3). Any
    /// stable per-process value works — this hashes the process id together
    /// with a continuous-clock read taken once here, at trace open, the same
    /// way `CanonicalProfile.hash8` derives `profile_hash`: first 8 bytes of
    /// SHA-256 over the two values, hex-encoded to exactly 16 lowercase
    /// characters.
    private static func makeRunToken() -> String {
        let seed = Data("\(ProcessInfo.processInfo.processIdentifier):\(RescClockBridge.continuousNowUs())".utf8)
        let digestPrefix = Data(SHA256.hash(data: seed).prefix(8))
        return digestPrefix.map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - Public API

    /// Writes one `"frame"` record binding the wire `frameID` to the exact
    /// capture identity of the frame that produced it. The join is exact,
    /// not latest-wins: `identity` is the same value the encoder submit
    /// carried through its per-submit context and recovered in its output
    /// callback for this exact frame (A00_REMEDIATION_PLAN.md §4 items 4–5).
    /// `identity` is `nil` only for callers with no capture identity to give
    /// (e.g. HarnessSender's synthetic source) — the five identity fields
    /// are written `null` in that case. No-op unless `enabled`.
    func frameSent(frameID: UInt32, identity: FrameIdentity?, bytes: Int, isKeyframe: Bool,
                   encodeOutTsUs: UInt64, sendTsUs: UInt64) {
        guard Self.enabled else { return }
        var record: [String: Any] = [
            "t": "frame",
            "frame_id": frameID,
            "encode_out_ts_us": encodeOutTsUs,
            "send_ts_us": sendTsUs,
            "bytes": bytes,
            "kf": isKeyframe,
        ]
        record["generation"] = identity?.generation ?? NSNull()
        record["capture_seq"] = identity?.captureSeq ?? NSNull()
        record["capture_ts_us"] = identity?.captureTsUs ?? NSNull()
        record["ts_source"] = identity.map { Self.tsSourceString($0.tsSource) } ?? NSNull()
        record["uncertainty_us"] = identity?.uncertaintyUs ?? NSNull()
        enqueue(record, kind: .frame)
    }

    /// `CaptureTsSource` → the frozen trace schema's `ts_source` string.
    private static func tsSourceString(_ source: CaptureTsSource) -> String {
        switch source {
        case .sckPts: return "sck_pts"
        case .callbackFallback: return "callback_fallback"
        }
    }

    /// Writes one `"pong"` record each time the host answers a `ClockPing`
    /// (HostSession's clock-ping responder). No-op unless `enabled`.
    func clockSample(seq: UInt32, t1: UInt64, t2: UInt64, t3: UInt64) {
        guard Self.enabled else { return }
        enqueue([
            "t": "pong",
            "seq": seq,
            "t1": t1,
            "t2": t2,
            "t3": t3,
        ], kind: .pong)
    }

    // MARK: - Trace termination (A00_COMPLETION_REPORT_AMENDED_response_review.md
    // amendment 3 / A00_COMPLETION_REPORT_AMENDED_response.md v2 §2 F4 — the
    // C3 clean/aborted trace-termination protocol)

    /// This trace's current lifetime `"frame"`/`"pong"` record counts, safe
    /// to read from any thread. Lets a shutdown path pass exact totals into
    /// `finish(status:framesSent:pongs:)` without keeping its own parallel
    /// counters.
    var counts: (framesSent: UInt64, pongs: UInt64) {
        queue.sync { (self.framesSentCount, self.pongsCount) }
    }

    /// Appends the trace's one terminal footer record —
    /// `{"t":"trace_complete","run_token":<16-hex string>,"status":status,
    /// "frames_sent":N,"pongs":N}` — then synchronously flushes and fsyncs
    /// (`synchronizeFile()`) the trace file before returning. Runs on
    /// `queue.sync`, not the fire-and-forget `enqueue(_:kind:)` every other
    /// record uses: a shutdown path calls this immediately before `exit()`,
    /// so there is no later flush-timer tick to depend on — the footer must
    /// be durable by the time this call returns. No-op unless `enabled`.
    func finish(status: String, framesSent: UInt64, pongs: UInt64) {
        guard Self.enabled else { return }
        queue.sync {
            self.write([
                "t": "trace_complete",
                "run_token": self.runToken,
                "status": status,
                "frames_sent": framesSent,
                "pongs": pongs,
            ])
            self.flush()
        }
    }

    /// Best-effort trace-abort marker for a fatal path that fires while
    /// tracing is active: writes the same footer as `finish` with
    /// `status: "aborted"`, using this trace's own internally tracked
    /// frame/pong counts (`counts`) so the fatal call site does not need to
    /// carry its own running totals just to call this. No-op unless
    /// `enabled`; like `finish`, it flushes synchronously before returning.
    func finishAborted() {
        guard Self.enabled else { return }
        let c = counts
        finish(status: "aborted", framesSent: c.framesSent, pongs: c.pongs)
    }

    // MARK: - File I/O (queue-confined)

    /// Which lifetime counter `enqueue(_:kind:)` should bump alongside
    /// writing a record — backs the `finish`/`finishAborted` frame/pong
    /// totals above.
    private enum RecordKind {
        case frame
        case pong
    }

    private func enqueue(_ record: [String: Any], kind: RecordKind) {
        queue.async {
            self.write(record)
            switch kind {
            case .frame: self.framesSentCount += 1
            case .pong: self.pongsCount += 1
            }
            self.pendingCount += 1
            if self.pendingCount >= Self.flushCount { self.flush() }
        }
    }

    private func openForAppend() {
        let fm = FileManager.default
        try? fm.createDirectory(at: traceDir, withIntermediateDirectories: true)
        if !fm.fileExists(atPath: traceFile.path) {
            fm.createFile(atPath: traceFile.path, contents: nil)
        }
        try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: traceFile.path)
        fileHandle = FileHandle(forWritingAtPath: traceFile.path)
        fileHandle?.seekToEndOfFile()
    }

    private func write(_ record: [String: Any]) {
        rotateIfNeeded()
        guard JSONSerialization.isValidJSONObject(record),
              let data = try? JSONSerialization.data(withJSONObject: record, options: []) else {
            return
        }
        fileHandle?.write(data)
        fileHandle?.write(Self.newline)
    }

    private func flush() {
        fileHandle?.synchronizeFile()
        pendingCount = 0
    }

    /// Rotates `host-trace.jsonl` → `.1` → `.2` → `.3` → `.4` (oldest `.4`
    /// deleted) when the active file has grown past `maxFileBytes`. Mirrors
    /// `RescLog`'s rotation (RescLog.swift).
    private func rotateIfNeeded() {
        let attrs = try? FileManager.default.attributesOfItem(atPath: traceFile.path)
        let size = (attrs?[.size] as? NSNumber)?.uint64Value ?? 0
        guard size > Self.maxFileBytes else { return }

        fileHandle?.closeFile()
        let fm = FileManager.default
        try? fm.removeItem(at: traceDir.appendingPathComponent("host-trace.jsonl.\(Self.maxBackups)"))
        var i = Self.maxBackups
        while i > 1 {
            let src = traceDir.appendingPathComponent("host-trace.jsonl.\(i - 1)")
            if fm.fileExists(atPath: src.path) {
                try? fm.moveItem(at: src, to: traceDir.appendingPathComponent("host-trace.jsonl.\(i)"))
            }
            i -= 1
        }
        try? fm.moveItem(at: traceFile, to: traceDir.appendingPathComponent("host-trace.jsonl.1"))
        openForAppend()
    }

    private func scheduleFlushTimer() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 1.0, repeating: 1.0)
        timer.setEventHandler { [weak self] in self?.flush() }
        timer.resume()
        flushTimer = timer
    }
}
