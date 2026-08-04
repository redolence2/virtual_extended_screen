import Foundation
import CoreMedia
import CoreVideo
import RescCore

// RESC A0 measurement harness — sender side (IMPLEMENTATION_PLAN_V11.md §12
// A0.0 "the A0 measurement harness"; §6 encoder lifecycle; §7 flow control;
// docs/WIRE.md §4 frame records). Disposable measurement rig, NOT part of
// the shipping host/client: drives the real HEVC encoder with a paced 60Hz
// synthetic NV12 source through a stop-and-wait ACK transport to a harness
// receiver, to measure whether `flow_window_frames` 1 (or 2) sustains 60Hz
// on the real encoder + network path (v11 §12: "harness window trial →
// flow_window_frames"). Its own executable target (depends on RescCore
// only) so it can link VideoEncoder without dragging in the host's
// ScreenCaptureKit/CGVirtualDisplay-heavy executable target, which cannot be
// depended on anyway (RemoteDisplayHost is itself an executable).
//
// This tool has no access to RescLog/nativeCheck (Diagnostics/*.swift lives
// in the RemoteDisplayHost executable target, not RescCore) — it reports
// native-call failures with plain prints, matching this codebase's existing
// style for raw POSIX-socket-level code (VideoSender.swift, InputReceiver.swift
// both do the same). The mandated JSON report at the end is this tool's
// structured evidence output.

print("[HARNESS] RESC A0 harness sender starting...")

// MARK: - CLI (manual parse, matching RemoteDisplayHost/main.swift's style)

let cliArgs = CommandLine.arguments
func argValue(_ flag: String) -> String? {
    if let idx = cliArgs.firstIndex(of: flag), idx + 1 < cliArgs.count { return cliArgs[idx + 1] }
    return nil
}

guard let connectHost = argValue("--connect") else {
    print("[HARNESS] Usage: resc-harness-sender --connect <ip> [--port 9871] [--window 1|2] [--seconds N] [--bitrate bps] [--json-out path]")
    exit(1)
}
let connectPort: UInt16 = argValue("--port").flatMap(UInt16.init) ?? 9871
let window: Int = argValue("--window").flatMap(Int.init) ?? 1
guard window == 1 || window == 2 else {
    print("[HARNESS] --window must be 1 or 2 (got \(argValue("--window") ?? "?"))")
    exit(1)
}
let seconds: Int = argValue("--seconds").flatMap(Int.init) ?? 20
guard seconds > 0 else {
    print("[HARNESS] --seconds must be positive (got \(argValue("--seconds") ?? "?"))")
    exit(1)
}
let bitrateBps: UInt32 = argValue("--bitrate").flatMap(UInt32.init) ?? 20_000_000
let jsonOutPath: String? = argValue("--json-out")

print("[HARNESS] connect=\(connectHost):\(connectPort) window=\(window) seconds=\(seconds) bitrate=\(bitrateBps)bps")

// Profile encode size (v11 §2 canonical profile — portrait 1080x1920@60).
let profileWidth: Int32 = 1080
let profileHeight: Int32 = 1920
let profileFps: Double = 60.0

// MARK: - Continuous-monotonic clock (µs)
//
// Mirrors RescClockBridge.continuousNowUs() in
// RemoteDisplayHost/Diagnostics/RescClockBridge.swift — duplicated here in
// miniature because this target depends only on RescCore, not the
// RemoteDisplayHost executable that owns the Diagnostics/ helpers. Not worth
// promoting a whole diagnostics module for one function on a disposable rig;
// same technique (mach_continuous_time + timebase scaling), same semantics
// as docs/WIRE.md §4's "host continuous-monotonic µs epoch".
let harnessTimebase: mach_timebase_info_data_t = {
    var info = mach_timebase_info_data_t()
    mach_timebase_info(&info)
    return info
}()

func continuousNowUs() -> UInt64 {
    let ticks = mach_continuous_time()
    let numer = UInt64(harnessTimebase.numer)
    let denom = UInt64(harnessTimebase.denom)
    guard denom != 0 else { return ticks / 1000 }
    let ns = (ticks / denom) * numer + (ticks % denom) * numer / denom
    return ns / 1000
}

// MARK: - Frame record wire format (docs/WIRE.md §4)
//
// Implemented inline here rather than shared — a future pass should hoist
// this into RescCore alongside VideoEncoder so the real host/client and this
// harness share one implementation.

func appendLE(_ data: inout Data, _ value: UInt32) {
    var v = value.littleEndian
    data.append(Data(bytes: &v, count: 4))
}
func appendLE(_ data: inout Data, _ value: UInt64) {
    var v = value.littleEndian
    data.append(Data(bytes: &v, count: 8))
}

/// Builds one 32-byte frame-record header + Annex-B payload (docs/WIRE.md
/// §4): magic `56 46`, headerLen 32, flags (bit0 = keyframe), frameOrdinal
/// (u64 LE), captureSeq (u32 LE), contentCaptureTs_us (u64 LE), reserved=0
/// (u32 LE), payloadLen (u32 LE), then the Annex-B AU itself.
func buildFrameRecord(ordinal: UInt64, flags: UInt8, captureSeq: UInt32,
                       contentCaptureTsUs: UInt64, payload: Data) -> Data {
    var record = Data(capacity: 32 + payload.count)
    record.append(contentsOf: [0x56, 0x46])          // magic "VF"
    record.append(32)                                 // headerLen
    record.append(flags)
    appendLE(&record, ordinal)                         // frameOrdinal (u64 LE)
    appendLE(&record, captureSeq)                       // captureSeq (u32 LE)
    appendLE(&record, contentCaptureTsUs)                // contentCaptureTs_us (u64 LE)
    appendLE(&record, UInt32(0))                          // reserved
    appendLE(&record, UInt32(payload.count))               // payloadLen
    record.append(payload)
    return record
}

// MARK: - Flow-control pump (stop-and-wait, latest-wins pending slot)

/// Blocking full-write of `data` to `fd`. Returns true iff every byte was
/// written (EINTR retried; any other short write/error is a failure).
func writeFull(_ fd: Int32, _ data: Data) -> Bool {
    data.withUnsafeBytes { buf -> Bool in
        guard let base = buf.baseAddress else { return false }
        var totalWritten = 0
        while totalWritten < buf.count {
            let n = write(fd, base + totalWritten, buf.count - totalWritten)
            if n > 0 {
                totalWritten += n
            } else if n < 0, errno == EINTR {
                continue
            } else {
                return false
            }
        }
        return true
    }
}

/// Owns the stop-and-wait flow-control state: the outstanding (sent,
/// un-ACKed) ledger, the single latest-wins pending-send slot (plan v11 §6's
/// `latestPendingCapture` pattern, one level up — post-encode instead of
/// post-capture, since this harness has no separate capture stage), and the
/// metric samples the final report needs.
///
/// Both the encoder-callback thread (`submitEncoded`) and the ACK-reader
/// thread (`handleAck`, promoting a pending record once a slot frees up) can
/// produce a record that needs writing to the socket. The actual
/// `write(2)` therefore happens *inside* `reserveAndSend`, itself always
/// called while `lock` is held — deliberately, not an oversight: if the
/// write happened after releasing the lock (as a ticket handed back to the
/// caller), two threads could each get a ticket and race their `write()`
/// calls, letting a higher ordinal's bytes hit the wire before a lower
/// ordinal's. Holding the lock across one local/LAN TCP write is cheap
/// (window is 1-2 by design — this is a stop-and-wait rig) and is what
/// guarantees frame records land on the wire in strict ordinal order.
final class HarnessPump: @unchecked Sendable {
    struct EncodedRecord {
        let payload: Data
        let isKeyframe: Bool
        let captureSeq: UInt32
        let contentCaptureTsUs: UInt64
        let encodeMs: Double
    }

    private struct Outstanding {
        var submitTimeUs: UInt64?
    }

    private let lock = NSLock()
    private let window: Int
    private let socketFd: Int32

    private var nextOrdinal: UInt64 = 1
    private var outstanding: [UInt64: Outstanding] = [:]
    private var pending: EncodedRecord?
    private(set) var stopped = false

    private var sentCount = 0
    private var pendingReplacedCount = 0
    private var ackOrderViolationCount = 0
    private var rttSamplesMs: [Double] = []
    private var encodeSamplesMs: [Double] = []
    private var byteSamples: [Double] = []

    init(window: Int, socketFd: Int32) {
        self.window = window
        self.socketFd = socketFd
    }

    /// Encoder-callback thread. Sends immediately if there is window room,
    /// else replaces `pending` (latest-wins) and counts the replacement.
    func submitEncoded(_ record: EncodedRecord) {
        lock.lock()
        defer { lock.unlock() }
        guard !stopped else { return }
        if outstanding.count < window {
            reserveAndSend(record)
        } else {
            if pending != nil { pendingReplacedCount += 1 }
            pending = record
        }
    }

    /// ACK-reader thread. Validates `ordinal` names the oldest outstanding
    /// record (oldest-first freeing only, v11 §7), records RTT, and — on
    /// success — promotes `pending` (if any) into a new send. Returns false
    /// on an order violation (caller should stop reading and end the run).
    @discardableResult
    func handleAck(ordinal: UInt64, nowUs: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let oldest = outstanding.keys.min(), ordinal == oldest else {
            ackOrderViolationCount += 1
            stopped = true
            return false
        }
        if let entry = outstanding.removeValue(forKey: ordinal), let submitTimeUs = entry.submitTimeUs {
            rttSamplesMs.append(Double(nowUs &- submitTimeUs) / 1000.0)
        }
        if let next = pending {
            pending = nil
            reserveAndSend(next)
        }
        return true
    }

    /// Queue-confined (`lock` held by both callers above): assigns the next
    /// ordinal, reserves its outstanding slot, builds and writes the wire
    /// record, and — only once the write actually completes — stamps the
    /// submit time RTT is measured from. encode_ms/bytes samples are taken
    /// here too (both already known, independent of the ACK round trip).
    private func reserveAndSend(_ record: EncodedRecord) {
        let ordinal = nextOrdinal
        nextOrdinal += 1
        let flags: UInt8 = record.isKeyframe ? 0x01 : 0x00
        outstanding[ordinal] = Outstanding(submitTimeUs: nil)
        sentCount += 1
        encodeSamplesMs.append(record.encodeMs)
        let recordBytes = 32 + record.payload.count
        byteSamples.append(Double(recordBytes))

        let wireData = buildFrameRecord(ordinal: ordinal, flags: flags, captureSeq: record.captureSeq,
                                         contentCaptureTsUs: record.contentCaptureTsUs, payload: record.payload)
        if writeFull(socketFd, wireData) {
            outstanding[ordinal]?.submitTimeUs = continuousNowUs()
        } else {
            print("[HARNESS] socket write failed/short (ordinal=\(ordinal), \(recordBytes)B): errno=\(errno)")
        }
    }

    var isStopped: Bool {
        lock.lock(); defer { lock.unlock() }
        return stopped
    }

    var outstandingCount: Int {
        lock.lock(); defer { lock.unlock() }
        return outstanding.count
    }

    struct Snapshot {
        let framesSent: Int
        let framesAcked: Int
        let pendingReplaced: Int
        let ackOrderViolation: Int
        let rttMs: [Double]
        let encodeMs: [Double]
        let bytes: [Double]
    }

    func snapshot() -> Snapshot {
        lock.lock(); defer { lock.unlock() }
        return Snapshot(framesSent: sentCount, framesAcked: rttSamplesMs.count,
                         pendingReplaced: pendingReplacedCount, ackOrderViolation: ackOrderViolationCount,
                         rttMs: rttSamplesMs, encodeMs: encodeSamplesMs, bytes: byteSamples)
    }
}

// MARK: - TCP connect (POSIX, symmetric with VideoSender.swift's raw-socket style)

let sockFd = socket(AF_INET, SOCK_STREAM, 0)
guard sockFd >= 0 else {
    print("[HARNESS] socket() failed: errno=\(errno)")
    exit(3)
}

var destAddr = sockaddr_in()
destAddr.sin_family = sa_family_t(AF_INET)
destAddr.sin_port = connectPort.bigEndian
guard inet_pton(AF_INET, connectHost, &destAddr.sin_addr) == 1 else {
    print("[HARNESS] invalid --connect address: \(connectHost)")
    exit(1)
}

let connectStatus = withUnsafePointer(to: &destAddr) { ptr in
    ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
        connect(sockFd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
    }
}
guard connectStatus == 0 else {
    print("[HARNESS] connect() to \(connectHost):\(connectPort) failed: errno=\(errno) (\(String(cString: strerror(errno))))")
    exit(3)
}

var nodelayFlag: Int32 = 1
let nodelayStatus = setsockopt(sockFd, Int32(IPPROTO_TCP), TCP_NODELAY, &nodelayFlag, socklen_t(MemoryLayout<Int32>.size))
if nodelayStatus != 0 {
    print("[HARNESS] WARNING: setsockopt(TCP_NODELAY) failed: errno=\(errno)")
}
print("[HARNESS] Connected to \(connectHost):\(connectPort), TCP_NODELAY=\(nodelayStatus == 0)")

// MARK: - Pump + encoder wiring

let pump = HarnessPump(window: window, socketFd: sockFd)

// captureSeq -> contentCaptureTsUs, populated at generation time and
// consumed by the encoder output callback (correlated via the PTS value,
// which carries captureSeq — see the generation loop below).
let captureMetaLock = NSLock()
var captureMetaBySeq: [UInt32: UInt64] = [:]

var encoderConfig = VideoEncoder.Config(width: profileWidth, height: profileHeight, fps: profileFps, codec: .hevc)
encoderConfig.bitrateBps = bitrateBps

let encoder = VideoEncoder(config: encoderConfig) { annexBData, isKeyframe, pts, encodeDurationMs in
    let seq = UInt32(pts.value)
    captureMetaLock.lock()
    let capTs = captureMetaBySeq.removeValue(forKey: seq) ?? continuousNowUs()
    captureMetaLock.unlock()

    let record = HarnessPump.EncodedRecord(
        payload: annexBData, isKeyframe: isKeyframe, captureSeq: seq,
        contentCaptureTsUs: capTs, encodeMs: encodeDurationMs
    )
    pump.submitEncoded(record)
}

do {
    try encoder.start()
} catch {
    print("[HARNESS] Encoder start failed: \(error)")
    close(sockFd)
    exit(3)
}

// MARK: - ACK reader thread
//
// 12-byte ACK records: magic 'A' 'K' (0x41 0x4B), u16 reserved, u64
// frame_ordinal LE — the harness-receiver's own private ACK framing (not a
// normative WIRE.md record).

final class AckReaderState: @unchecked Sendable {
    private let lock = NSLock()
    private var _running = true
    var running: Bool {
        get { lock.lock(); defer { lock.unlock() }; return _running }
        set { lock.lock(); _running = newValue; lock.unlock() }
    }
}
let ackState = AckReaderState()

let ackThread = Thread {
    var buf = [UInt8](repeating: 0, count: 12)
    while ackState.running {
        var filled = 0
        var stop = false
        while filled < 12 {
            let n = buf.withUnsafeMutableBytes { ptr -> Int in
                guard let base = ptr.baseAddress else { return -1 }
                return recv(sockFd, base.advanced(by: filled), 12 - filled, 0)
            }
            if n > 0 {
                filled += n
            } else if n == 0 {
                print("[HARNESS] ACK connection closed by peer")
                stop = true
                break
            } else if errno == EINTR {
                continue
            } else {
                print("[HARNESS] ACK recv error: errno=\(errno)")
                stop = true
                break
            }
        }
        if stop { ackState.running = false; break }

        guard buf[0] == 0x41, buf[1] == 0x4B else {
            print("[HARNESS] ACK bad magic (\(buf[0]),\(buf[1])) — stopping")
            ackState.running = false
            break
        }
        // buf[2..<4] reserved (u16) — private harness framing, not validated.
        var ordinal: UInt64 = 0
        for i in 0..<8 { ordinal |= UInt64(buf[4 + i]) << (8 * i) }

        let now = continuousNowUs()
        if !pump.handleAck(ordinal: ordinal, nowUs: now) {
            print("[HARNESS] ACK order violation: got ordinal=\(ordinal), not oldest outstanding — stopping")
            ackState.running = false
        }
    }
}
ackThread.name = "com.resc.harness-ack-reader"
ackThread.start()

// MARK: - Synthetic 60Hz NV12 frame source (horizontal bars, phase-shifting)

func makePixelBuffer() -> CVPixelBuffer? {
    var pb: CVPixelBuffer?
    let attrs: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary]
    let status = CVPixelBufferCreate(kCFAllocatorDefault, Int(profileWidth), Int(profileHeight),
                                      kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, attrs as CFDictionary, &pb)
    guard status == kCVReturnSuccess else {
        print("[HARNESS] CVPixelBufferCreate failed: \(status)")
        return nil
    }
    return pb
}

/// Fills the Y plane with horizontal bars whose phase shifts every frame
/// (enough entropy that HEVC frames are non-trivial) and the UV plane with
/// a flat mid-gray. `CVPixelBufferLockBaseAddress`/`Unlock` bracket the memsets.
func fillBarPattern(_ pixelBuffer: CVPixelBuffer, phase: Int) {
    guard CVPixelBufferLockBaseAddress(pixelBuffer, []) == kCVReturnSuccess else { return }
    defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, []) }

    let barHeight = 32
    if let yBase = CVPixelBufferGetBaseAddressOfPlane(pixelBuffer, 0) {
        let bytesPerRow = CVPixelBufferGetBytesPerRowOfPlane(pixelBuffer, 0)
        let height = CVPixelBufferGetHeightOfPlane(pixelBuffer, 0)
        for row in 0..<height {
            let bar = ((row + phase) / barHeight) % 2
            let value: Int32 = bar == 0 ? 0x40 : 0xC0
            memset(yBase.advanced(by: row * bytesPerRow), value, bytesPerRow)
        }
    }
    if let uvBase = CVPixelBufferGetBaseAddressOfPlane(pixelBuffer, 1) {
        let bytesPerRow = CVPixelBufferGetBytesPerRowOfPlane(pixelBuffer, 1)
        let height = CVPixelBufferGetHeightOfPlane(pixelBuffer, 1)
        memset(uvBase, 0x80, bytesPerRow * height)
    }
}

// MARK: - Main pacing loop (60Hz, deadlines computed from a fixed start
// reference so per-iteration truncation cannot compound into drift)

encoder.forceKeyframe()

var sourceFrameCounter: UInt32 = 0
let runStartUs = continuousNowUs()
let runEndUs = runStartUs + UInt64(seconds) * 1_000_000

while continuousNowUs() < runEndUs {
    guard let pb = makePixelBuffer() else {
        print("[HARNESS] CVPixelBufferCreate failed mid-run — stopping generation")
        break
    }
    fillBarPattern(pb, phase: Int(sourceFrameCounter) * 4)

    let seq = sourceFrameCounter
    sourceFrameCounter &+= 1
    captureMetaLock.lock()
    captureMetaBySeq[seq] = continuousNowUs()
    captureMetaLock.unlock()

    let pts = CMTime(value: CMTimeValue(seq), timescale: Int32(profileFps))
    encoder.encode(pixelBuffer: pb, presentationTime: pts)

    if pump.isStopped {
        print("[HARNESS] Pump stopped (ACK order violation) — ending generation early")
        break
    }

    let nextDeadlineUs = runStartUs + UInt64((Double(sourceFrameCounter) * 1_000_000.0 / profileFps).rounded())
    let nowUs = continuousNowUs()
    if nextDeadlineUs > nowUs {
        Thread.sleep(forTimeInterval: Double(nextDeadlineUs - nowUs) / 1_000_000.0)
    }
}

print("[HARNESS] Generation complete (\(sourceFrameCounter) source frames) — draining outstanding ACKs...")
let drainDeadlineUs = continuousNowUs() + 2_000_000 // 2s grace
while pump.outstandingCount > 0, continuousNowUs() < drainDeadlineUs, !pump.isStopped {
    Thread.sleep(forTimeInterval: 0.01)
}

ackState.running = false
shutdown(sockFd, SHUT_RDWR) // unblock the ACK reader thread's blocking recv()
close(sockFd)
encoder.stop()

// MARK: - Report

let snap = pump.snapshot()
let achievedFps = Double(snap.framesSent) / Double(seconds)
let sustained60Hz = achievedFps >= 59.0

func percentiles(_ samples: [Double]) -> [String: Double] {
    guard !samples.isEmpty else { return ["p50": 0, "p95": 0, "max": 0] }
    let sorted = samples.sorted()
    func at(_ p: Double) -> Double {
        let idx = min(sorted.count - 1, max(0, Int((Double(sorted.count) * p).rounded(.up)) - 1))
        return sorted[idx]
    }
    return ["p50": at(0.50), "p95": at(0.95), "max": sorted.last ?? 0]
}

// Note: ack_order_violation (snap.ackOrderViolation) is tracked per the spec
// (§ACK reader thread) but is not part of the harness_report_v1 field list
// below; it is surfaced via the console prints above instead of widening the
// JSON schema unilaterally.
let report: [String: Any] = [
    "harness_report_v": 1,
    "window": window,
    "seconds": seconds,
    "frames_sent": snap.framesSent,
    "frames_acked": snap.framesAcked,
    "achieved_fps": achievedFps,
    "pending_replaced": snap.pendingReplaced,
    "rtt_ms": percentiles(snap.rttMs),
    "encode_ms": percentiles(snap.encodeMs),
    "bytes": percentiles(snap.bytes),
    "sustained_60hz": sustained60Hz,
]

if let prettyData = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]),
   let prettyString = String(data: prettyData, encoding: .utf8) {
    print(prettyString)
}

if let path = jsonOutPath {
    if let compactData = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]) {
        do {
            try compactData.write(to: URL(fileURLWithPath: path))
            print("[HARNESS] Report written to \(path)")
        } catch {
            print("[HARNESS] Failed to write --json-out (\(path)): \(error)")
        }
    }
}

print("[HARNESS] Done. frames_sent=\(snap.framesSent) frames_acked=\(snap.framesAcked) " +
      "pending_replaced=\(snap.pendingReplaced) ack_order_violation=\(snap.ackOrderViolation) " +
      "achieved_fps=\(String(format: "%.1f", achievedFps)) sustained_60hz=\(sustained60Hz)")

exit(0)
