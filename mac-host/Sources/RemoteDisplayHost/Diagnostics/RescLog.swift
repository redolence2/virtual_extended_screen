import Foundation

/// Monotonic clock helper for diagnostics records. Never wall-clock — only
/// meaningful for computing durations within a single process's lifetime.
enum RescClock {
    /// Monotonic microseconds since an arbitrary per-process origin.
    static func monoUs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1000
    }
}

/// JSONL structured logger for the RESC host (IMPLEMENTATION_PLAN_V11.md §11.1).
///
/// Writes one JSON object per line to `~/Library/Logs/RESC/host.jsonl`
/// (permissions 0600), rotating at 10 MiB into `host.jsonl.1 … host.jsonl.4`
/// (5 files total; the oldest is deleted on rotation). `event` records are
/// buffered on a serial queue and flushed to disk every 32 records or 1 s;
/// `fatal` always flushes synchronously before returning, so a crash right
/// after logging a fatal record cannot lose it.
///
/// Callers must never pass secrets or raw frame/payload bytes in `fields` —
/// this logger performs no redaction of its own.
final class RescLog {
    static let shared = RescLog()

    private static let maxFileBytes: UInt64 = 10 * 1024 * 1024
    private static let maxBackups = 4 // host.jsonl.1 ... host.jsonl.4
    private static let newline = Data([0x0A])

    private static let isoFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private let queue = DispatchQueue(label: "com.resc.resclog")
    private let logDir: URL
    private let logFile: URL
    private var fileHandle: FileHandle?
    private var flushTimer: DispatchSourceTimer?
    private var pendingCount = 0
    private var sessionRunId: UInt64?
    private var profileHash: String?

    private init() {
        logDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/RESC", isDirectory: true)
        logFile = logDir.appendingPathComponent("host.jsonl")
        openForAppend()
        scheduleFlushTimer()
    }

    /// Sets the global fields included in every subsequent record. Intended
    /// to be called once per process, as each value becomes known (e.g.
    /// `session_run_id` after handshake promotion, `profile_hash` once the
    /// active profile is resolved). Either argument may be omitted.
    func setContext(sessionRunId: UInt64? = nil, profileHash: String? = nil) {
        queue.async {
            if let sessionRunId { self.sessionRunId = sessionRunId }
            if let profileHash { self.profileHash = profileHash }
        }
    }

    /// Logs a structured event. Buffered — not guaranteed on disk until the
    /// next periodic flush.
    func event(_ event: String, component: String, fields: [String: Any] = [:]) {
        let tsMono = RescClock.monoUs()
        let tsWall = Self.isoFormatter.string(from: Date())
        queue.async {
            var record = self.baseFields(tsMono: tsMono, tsWall: tsWall, component: component)
            record["event"] = event
            for (key, value) in fields { record[key] = value }
            self.write(record)
            self.pendingCount += 1
            if self.pendingCount >= 32 { self.flush() }
        }
    }

    /// Logs a fatal record and flushes synchronously before returning.
    func fatal(code: Int, component: String, nativeDomain: String?, nativeCode: Int64?,
               summary: String, fields: [String: Any] = [:]) {
        let tsMono = RescClock.monoUs()
        let tsWall = Self.isoFormatter.string(from: Date())
        queue.sync {
            var record = self.baseFields(tsMono: tsMono, tsWall: tsWall, component: component)
            record["event"] = "fatal"
            record["code"] = code
            record["native_domain"] = nativeDomain ?? NSNull()
            record["native_code"] = nativeCode ?? NSNull()
            record["summary"] = summary
            for (key, value) in fields { record[key] = value }
            self.write(record)
            self.flush()
        }
    }

    /// Synchronous flush (A00_REMEDIATION_PLAN.md §5 R3a): blocks the caller
    /// until every record buffered so far is written and `synchronizeFile`d
    /// on the logger's own serial queue — the same guarantee `fatal()`
    /// already gives its own record, exposed here for callers (Doctor.swift)
    /// that log via the normal buffered `event()` but still need a specific
    /// record (`doctor_complete`) to be durable before the process exits.
    /// `event()` alone is not enough: it is buffered (flushed every 32
    /// records or 1s) and can lose a record if the process exits first —
    /// the known bug this closes.
    func flushNow() {
        queue.sync {
            self.flush()
        }
    }

    // MARK: - Record assembly (queue-confined)

    private func baseFields(tsMono: UInt64, tsWall: String, component: String) -> [String: Any] {
        var record: [String: Any] = [
            "ts_mono_us": tsMono,
            "ts_wall": tsWall,
            "component": component,
        ]
        if let sessionRunId { record["session_run_id"] = sessionRunId }
        if let profileHash { record["profile_hash"] = profileHash }
        return record
    }

    // MARK: - File I/O (queue-confined)

    private func openForAppend() {
        let fm = FileManager.default
        try? fm.createDirectory(at: logDir, withIntermediateDirectories: true)
        if !fm.fileExists(atPath: logFile.path) {
            fm.createFile(atPath: logFile.path, contents: nil)
        }
        try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: logFile.path)
        fileHandle = FileHandle(forWritingAtPath: logFile.path)
        fileHandle?.seekToEndOfFile()
    }

    private func write(_ record: [String: Any]) {
        rotateIfNeeded()
        guard JSONSerialization.isValidJSONObject(record),
              let data = try? JSONSerialization.data(withJSONObject: record, options: []) else {
            let event = (record["event"] as? String) ?? "unknown"
            let fallback = "{\"event\":\"log_encode_error\",\"orig_event\":\"\(event)\"}\n"
            fileHandle?.write(fallback.data(using: .utf8) ?? Data())
            return
        }
        fileHandle?.write(data)
        fileHandle?.write(Self.newline)
    }

    private func flush() {
        fileHandle?.synchronizeFile()
        pendingCount = 0
    }

    /// Rotates `host.jsonl` → `.1` → `.2` → `.3` → `.4` (oldest `.4` deleted)
    /// when the active file has grown past `maxFileBytes`.
    private func rotateIfNeeded() {
        let attrs = try? FileManager.default.attributesOfItem(atPath: logFile.path)
        let size = (attrs?[.size] as? NSNumber)?.uint64Value ?? 0
        guard size > Self.maxFileBytes else { return }

        fileHandle?.closeFile()
        let fm = FileManager.default
        try? fm.removeItem(at: logDir.appendingPathComponent("host.jsonl.\(Self.maxBackups)"))
        var i = Self.maxBackups
        while i > 1 {
            let src = logDir.appendingPathComponent("host.jsonl.\(i - 1)")
            if fm.fileExists(atPath: src.path) {
                try? fm.moveItem(at: src, to: logDir.appendingPathComponent("host.jsonl.\(i)"))
            }
            i -= 1
        }
        try? fm.moveItem(at: logFile, to: logDir.appendingPathComponent("host.jsonl.1"))
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
