import Foundation
import CoreGraphics
import CoreMedia
import CoreVideo
import VideoToolbox
import VirtualDisplayBridge
import RescCore

/// `--doctor` mode (IMPLEMENTATION_PLAN_V11.md §11.4): a standalone,
/// non-streaming probe of every load-bearing native dependency the host
/// needs, run on demand (`remote-display-host --doctor`) instead of only
/// being discovered the hard way during a real session. Every native call
/// is checked and logged via `nativeCheck`/`RescLog` (§11.3 — no silent
/// fallback), and the same facts are assembled into a JSON report written
/// to stdout and to `~/Library/Logs/RESC/doctor_host.json`.
///
/// Exit codes (v11 §11.4): 0 = pass, 2 = environment, 3 = native-API
/// failure. (Code 4, peer-diagnostic failure, belongs to `--diagnose-peer`
/// mode, which this doctor does not implement.) `environment`'s failure
/// mode is narrow — it only fails if the OS build string genuinely cannot
/// be read (`sysctlbyname` itself fails) — every other check here is a
/// native-API probe and maps a failure to 3.
enum HostDoctor {

    private static let component = "doctor"

    // Profile display/encoder settings (v11 §2 canonical profile — portrait
    // 1080x1920@60; docs/WIRE.md §9 placeholder bytes).
    private static let profileWidth: Int32 = 1080
    private static let profileHeight: Int32 = 1920
    private static let profileFps: Double = 60.0
    private static let profileBitrateBps: UInt32 = 20_000_000

    private static let exitPass: Int32 = 0
    private static let exitEnvironment: Int32 = 2
    private static let exitNative: Int32 = 3

    // NSError domain used by the CGVirtualDisplay private bridge
    // (Sources/VirtualDisplay/CGVirtualDisplayBridge.m `kErrorDomain`).
    // VirtualDisplayManager.create's throw (DisplayError.creationFailed(String))
    // keeps only the description, not the original NSError's domain/code — so
    // this doctor logs the domain from this known constant plus the message
    // text rather than restructuring VirtualDisplayManager.swift to plumb a
    // numeric code through (out of scope: reuse, do not restructure).
    private static let virtualDisplayErrorDomain = "com.resc.virtualdisplay"

    // MARK: - Entry point

    static func run() -> Int32 {
        var checks: [[String: Any]] = []
        var exitCode = exitPass

        // a. environment
        let env = environmentCheck()
        checks.append(env.entry)
        exitCode = max(exitCode, env.exitContribution)

        // b. cgvirtualdisplay_api
        let api = cgVirtualDisplayApiCheck()
        checks.append(api.entry)
        exitCode = max(exitCode, api.exitContribution)

        // c. virtual_display_create
        let display = virtualDisplayCreateCheck()
        checks.append(display.entry)
        exitCode = max(exitCode, display.exitContribution)

        // d/e/f. encoder_create, encode_bundled_frame, ra_verification
        let encoderResult = encoderChecks()
        checks.append(contentsOf: encoderResult.entries)
        exitCode = max(exitCode, encoderResult.exitContribution)

        // g. corebrightness_nightshift — never contributes to exitCode.
        checks.append(nightShiftCheck())

        // h. write the report
        writeReport(checks: checks, exitCode: exitCode)

        return exitCode
    }

    // MARK: - a. environment

    private static func environmentCheck() -> (entry: [String: Any], exitContribution: Int32) {
        let osVersionString = ProcessInfo.processInfo.operatingSystemVersionString
        let osBuild = doctorKernOSVersion()
        let cpuArch = doctorMachineArchitecture()
        let ok = osBuild != nil

        nativeCheck(component, "sysctlbyname(kern.osversion)", ok: ok,
                    detail: "environment characterization (EnvironmentRecord.swift's approach, duplicated here)",
                    extra: ["os_version": osVersionString, "cpu_arch": cpuArch])

        let entry: [String: Any] = [
            "name": "environment",
            "ok": ok,
            "os_version": osVersionString,
            "os_build": osBuild ?? NSNull(),
            "cpu_arch": cpuArch,
            "auth_mode": "trusted_lan_none",
        ]
        return (entry, ok ? exitPass : exitEnvironment)
    }

    /// `sysctl kern.osversion` — the exact build string (e.g. "26A5388g").
    /// Mirrors EnvironmentRecord.swift's private helper of the same purpose;
    /// duplicated here rather than exposed, per "reuse EnvironmentRecord's
    /// approach" (that helper is file-private and EnvironmentRecord.swift is
    /// not to be restructured for this).
    private static func doctorKernOSVersion() -> String? {
        var size = 0
        guard sysctlbyname("kern.osversion", nil, &size, nil, 0) == 0, size > 0 else { return nil }
        var buffer = [CChar](repeating: 0, count: size)
        guard sysctlbyname("kern.osversion", &buffer, &size, nil, 0) == 0 else { return nil }
        return String(cString: buffer)
    }

    private static func doctorMachineArchitecture() -> String {
        var uts = utsname()
        uname(&uts)
        return withUnsafeBytes(of: &uts.machine) { raw -> String in
            String(cString: raw.baseAddress!.assumingMemoryBound(to: CChar.self))
        }
    }

    // MARK: - b. cgvirtualdisplay_api

    private static func cgVirtualDisplayApiCheck() -> (entry: [String: Any], exitContribution: Int32) {
        let available = CGVirtualDisplayBridge.isAPIAvailable()
        nativeCheck(component, "CGVirtualDisplayBridge.isAPIAvailable", ok: available,
                    detail: "NSClassFromString(CGVirtualDisplay) lookup")

        var entry: [String: Any] = ["name": "cgvirtualdisplay_api", "ok": available]
        if !available {
            entry["native_domain"] = virtualDisplayErrorDomain
            entry["detail"] = "CGVirtualDisplay class not found — private API unavailable on this OS"
        }
        return (entry, available ? exitPass : exitNative)
    }

    // MARK: - c. virtual_display_create

    private static func virtualDisplayCreateCheck() -> (entry: [String: Any], exitContribution: Int32) {
        let requested: [String: Any] = ["width": profileWidth, "height": profileHeight, "refresh_hz": profileFps]
        let manager = VirtualDisplayManager()
        do {
            let handle = try manager.create(width: Int(profileWidth), height: Int(profileHeight), refreshRate: Int(profileFps))
            nativeCheck(component, "VirtualDisplayManager.create", ok: true, detail: "profile display created",
                        extra: ["display_id": handle.lastKnownDisplayID])
            manager.destroy()
            nativeCheck(component, "VirtualDisplayManager.destroy", ok: true, detail: "profile display destroyed")

            let entry: [String: Any] = [
                "name": "virtual_display_create", "ok": true,
                "requested": requested,
                "observed": ["display_id": handle.lastKnownDisplayID],
            ]
            return (entry, exitPass)
        } catch {
            nativeCheck(component, "VirtualDisplayManager.create", ok: false, detail: "\(error)")
            let entry: [String: Any] = [
                "name": "virtual_display_create", "ok": false,
                "requested": requested,
                "native_domain": virtualDisplayErrorDomain,
                "detail": "\(error)",
            ]
            return (entry, exitNative)
        }
    }

    // MARK: - d/e/f. encoder_create, encode_bundled_frame, ra_verification

    private static func encoderChecks() -> (entries: [[String: Any]], exitContribution: Int32) {
        var entries: [[String: Any]] = []
        var exitContribution = exitPass

        var config = VideoEncoder.Config(width: profileWidth, height: profileHeight, fps: profileFps, codec: .hevc)
        config.bitrateBps = profileBitrateBps
        // keyframeIntervalSeconds intentionally left at VideoEncoder.Config's
        // own default (1.0) — the spec forbids changing VideoEncoder.swift's
        // behavior beyond the vtSession accessor.
        let requestedSummary: [String: Any] = [
            "codec": "HEVC", "width": profileWidth, "height": profileHeight,
            "fps": profileFps, "bitrate_bps": profileBitrateBps,
        ]

        var capturedAU: Data?
        var capturedIsKeyframe = false
        let outputSemaphore = DispatchSemaphore(value: 0)

        let encoder = VideoEncoder(config: config) { annexBData, isKeyframe, _, _ in
            capturedAU = annexBData
            capturedIsKeyframe = isKeyframe
            outputSemaphore.signal()
        }

        do {
            try encoder.start()
        } catch {
            nativeCheck(component, "VideoEncoder.start", ok: false, detail: "\(error)")
            entries.append(["name": "encoder_create", "ok": false, "requested": requestedSummary, "detail": "\(error)"])
            entries.append(["name": "encode_bundled_frame", "ok": false, "detail": "skipped: encoder_create failed"])
            entries.append(["name": "ra_verification", "ok": false, "detail": "skipped: encoder_create failed"])
            encoder.stop()
            return (entries, exitNative)
        }
        nativeCheck(component, "VideoEncoder.start", ok: true, detail: "profile encoder started")

        guard let vt = encoder.vtSession else {
            // Unreachable in practice (start() just succeeded), but the
            // accessor is Optional — handle it rather than force-unwrap.
            nativeCheck(component, "VideoEncoder.vtSession", ok: false, detail: "session nil after successful start()")
            entries.append(["name": "encoder_create", "ok": false, "requested": requestedSummary,
                             "detail": "vtSession unavailable after start()"])
            entries.append(["name": "encode_bundled_frame", "ok": false, "detail": "skipped: encoder_create failed"])
            entries.append(["name": "ra_verification", "ok": false, "detail": "skipped: encoder_create failed"])
            encoder.stop()
            return (entries, exitNative)
        }

        // Required property read-back: requested-vs-observed for every
        // load-bearing property (v11 §6, §11.3).
        let realTime = propertyReadBack(vt, key: kVTCompressionPropertyKey_RealTime, requestedBool: true, name: "RealTime")
        let allowReorder = propertyReadBack(vt, key: kVTCompressionPropertyKey_AllowFrameReordering,
                                             requestedBool: false, name: "AllowFrameReordering")
        let bitrate = propertyReadBack(vt, key: kVTCompressionPropertyKey_AverageBitRate,
                                        requestedUInt32: profileBitrateBps, name: "AverageBitRate")
        let profileLevel = propertyReadBack(vt, key: kVTCompressionPropertyKey_ProfileLevel,
                                             requestedString: kVTProfileLevel_HEVC_Main_AutoLevel as String, name: "ProfileLevel")

        let allMatched = (realTime["match"] as? Bool ?? false)
            && (allowReorder["match"] as? Bool ?? false)
            && (bitrate["match"] as? Bool ?? false)
            && (profileLevel["match"] as? Bool ?? false)

        entries.append([
            "name": "encoder_create", "ok": allMatched,
            "requested": requestedSummary,
            "properties": [
                "RealTime": realTime, "AllowFrameReordering": allowReorder,
                "AverageBitRate": bitrate, "ProfileLevel": profileLevel,
            ],
        ])
        if !allMatched { exitContribution = exitNative }

        // e. encode_bundled_frame
        guard let pixelBuffer = makeBundledPixelBuffer() else {
            entries.append(["name": "encode_bundled_frame", "ok": false, "detail": "CVPixelBufferCreate failed"])
            entries.append(["name": "ra_verification", "ok": false, "detail": "skipped: no pixel buffer"])
            encoder.stop()
            return (entries, exitNative)
        }

        encoder.forceKeyframe()
        let encodeSubmitUs = RescClock.monoUs()
        encoder.encode(pixelBuffer: pixelBuffer, presentationTime: CMTime(value: 0, timescale: Int32(profileFps)))
        let waitResult = outputSemaphore.wait(timeout: .now() + 2.0)
        let waitedMs = Double(RescClock.monoUs() - encodeSubmitUs) / 1000.0

        let encodeOk = waitResult == .success && capturedAU != nil && !(capturedAU?.isEmpty ?? true)
        nativeCheck(component, "VideoEncoder.encode(bundled)", ok: encodeOk,
                    detail: waitResult == .timedOut ? "output callback did not fire within 2s" : "encode + callback observed",
                    extra: ["bytes": capturedAU?.count ?? 0, "is_keyframe": capturedIsKeyframe, "wait_ms": waitedMs])
        entries.append([
            "name": "encode_bundled_frame", "ok": encodeOk,
            "timed_out": waitResult == .timedOut,
            "bytes": capturedAU?.count ?? 0,
            "is_keyframe": capturedIsKeyframe,
        ])
        if !encodeOk { exitContribution = exitNative }

        // f. ra_verification — NAL-scan logic lives in RescCore
        // (RAVerification.swift) so FixtureCheck can exercise it against
        // synthetic sequences too; behavior/JSON fields here are unchanged.
        if let au = capturedAU, encodeOk {
            let summary = scanAnnexB(au)
            let raOk: Bool
            switch validateSessionFirst(summary) {
            case .success: raOk = true
            case .failure: raOk = false
            }

            let raFields: [String: Any] = [
                "ok": raOk, "nal_types_found": summary.types,
                "has_vps": summary.hasVPS, "has_sps": summary.hasSPS, "has_pps": summary.hasPPS,
                "has_idr": summary.hasIDR, "has_cra_instead_of_idr": summary.hasCRA && !summary.hasIDR,
            ]
            RescLog.shared.event("ra_verification", component: component, fields: raFields)

            var entry = raFields
            entry["name"] = "ra_verification"
            entries.append(entry)
            if !raOk { exitContribution = exitNative }
        } else {
            entries.append(["name": "ra_verification", "ok": false, "detail": "skipped: no encoded AU available"])
            exitContribution = exitNative
        }

        encoder.stop()
        return (entries, exitContribution)
    }

    /// Raw VTSessionCopyProperty call. This SDK's CommandLineTools-only
    /// VideoToolbox import has no audited `CF_RETURNS_RETAINED` annotation
    /// for `valueOut` (confirmed via VideoToolbox.bridgesupport: the arg is
    /// `^v`, untyped `void*`), so the naive `var v: CFTypeRef?; &v` idiom
    /// compiles but trips an ARC-safety warning. `withUnsafeMutablePointer`
    /// gives a properly scoped, ARC-tracked typed pointer that converts to
    /// the raw-pointer parameter cleanly, with no warning.
    private static func copyProperty(_ session: VTCompressionSession, key: CFString) -> (status: OSStatus, value: CFTypeRef?) {
        var value: CFTypeRef?
        let status = withUnsafeMutablePointer(to: &value) { ptr -> OSStatus in
            VTSessionCopyProperty(session, key: key, allocator: kCFAllocatorDefault, valueOut: ptr)
        }
        return (status, value)
    }

    /// Bool-valued VTSessionCopyProperty read-back (RealTime, AllowFrameReordering).
    private static func propertyReadBack(_ session: VTCompressionSession, key: CFString,
                                          requestedBool: Bool, name: String) -> [String: Any] {
        let (status, value) = copyProperty(session, key: key)
        let observed = (value as? NSNumber)?.boolValue
        let statusOk = nativeCheck(component, "VTSessionCopyProperty(\(name))", status: status,
                                    extra: ["requested": requestedBool, "observed": observed as Any? ?? NSNull()])
        let matched = statusOk && observed == requestedBool
        return ["requested": requestedBool, "observed": observed as Any? ?? NSNull(), "match": matched]
    }

    /// UInt32-valued VTSessionCopyProperty read-back (AverageBitRate).
    private static func propertyReadBack(_ session: VTCompressionSession, key: CFString,
                                          requestedUInt32: UInt32, name: String) -> [String: Any] {
        let (status, value) = copyProperty(session, key: key)
        let observed = (value as? NSNumber)?.uint32Value
        let statusOk = nativeCheck(component, "VTSessionCopyProperty(\(name))", status: status,
                                    extra: ["requested": requestedUInt32, "observed": observed as Any? ?? NSNull()])
        let matched = statusOk && observed == requestedUInt32
        return ["requested": requestedUInt32, "observed": observed as Any? ?? NSNull(), "match": matched]
    }

    /// String-valued VTSessionCopyProperty read-back (ProfileLevel).
    private static func propertyReadBack(_ session: VTCompressionSession, key: CFString,
                                          requestedString: String, name: String) -> [String: Any] {
        let (status, value) = copyProperty(session, key: key)
        let observed = value as? String
        let statusOk = nativeCheck(component, "VTSessionCopyProperty(\(name))", status: status,
                                    extra: ["requested": requestedString, "observed": observed ?? "nil"])
        let matched = statusOk && observed == requestedString
        return ["requested": requestedString, "observed": observed ?? NSNull(), "match": matched]
    }

    /// Builds the one bundled NV12 frame encoded for `encode_bundled_frame`
    /// / `ra_verification`: profile-sized, Y=0x80 / UV=0x80 (mid-gray —
    /// content doesn't matter, only that VideoToolbox accepts and encodes
    /// it), locked/unlocked around the memset per CVPixelBuffer's contract.
    private static func makeBundledPixelBuffer() -> CVPixelBuffer? {
        var pixelBuffer: CVPixelBuffer?
        let attrs: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary]
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault, Int(profileWidth), Int(profileHeight),
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, attrs as CFDictionary, &pixelBuffer
        )
        guard nativeCheck(component, "CVPixelBufferCreate", status: status, expected: kCVReturnSuccess),
              let pb = pixelBuffer else { return nil }

        let lockStatus = CVPixelBufferLockBaseAddress(pb, [])
        guard nativeCheck(component, "CVPixelBufferLockBaseAddress", status: lockStatus, expected: kCVReturnSuccess) else {
            return nil
        }
        defer {
            let unlockStatus = CVPixelBufferUnlockBaseAddress(pb, [])
            nativeCheck(component, "CVPixelBufferUnlockBaseAddress", status: unlockStatus, expected: kCVReturnSuccess)
        }

        if let yBase = CVPixelBufferGetBaseAddressOfPlane(pb, 0) {
            let bytesPerRow = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
            let height = CVPixelBufferGetHeightOfPlane(pb, 0)
            memset(yBase, 0x80, bytesPerRow * height)
        }
        if let uvBase = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
            let bytesPerRow = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
            let height = CVPixelBufferGetHeightOfPlane(pb, 1)
            memset(uvBase, 0x80, bytesPerRow * height)
        }
        return pb
    }

    // MARK: - g. corebrightness_nightshift (retained, non-required)

    private static func nightShiftCheck() -> [String: Any] {
        // RESCGetNightShiftStrength() (VirtualDisplayBridge/CGVirtualDisplayBridge.m,
        // also used by NightShiftMonitor.swift) lazily loads the CoreBrightness
        // private framework bundle on its first call, then does the class
        // lookup + instantiate + one status read internally. Call it FIRST so
        // the bundle is guaranteed loaded before this doctor's own class
        // lookup below — otherwise a doctor run that never touched Night
        // Shift before could see a false "class not found" purely from load
        // ordering, not an actual probe failure.
        let strength = RESCGetNightShiftStrength()
        let classFound = NSClassFromString("CBBlueLightClient") != nil
        nativeCheck(component, "RESCGetNightShiftStrength", ok: classFound,
                    detail: "CBBlueLightClient class lookup (post-probe) + instantiate + one status read",
                    extra: ["observed_strength": strength])

        if classFound {
            return ["name": "corebrightness_nightshift", "ok": true, "observed_strength": strength]
        }
        return ["name": "corebrightness_nightshift", "ok": false, "feature_disabled": true,
                "observed_strength": strength, "detail": "CBBlueLightClient class not found"]
    }

    // MARK: - h. write the report

    private static func writeReport(checks: [[String: Any]], exitCode: Int32) {
        let isoFormatter: ISO8601DateFormatter = {
            let f = ISO8601DateFormatter()
            f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            return f
        }()
        let report: [String: Any] = [
            "doctor_report_v": 1,
            "side": "host",
            "profile_id": CanonicalProfile.profileId,
            "ts_wall": isoFormatter.string(from: Date()),
            "checks": checks,
            "exit_code": exitCode,
        ]

        if let prettyData = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]),
           let prettyString = String(data: prettyData, encoding: .utf8) {
            print(prettyString)
        }

        let logDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/RESC", isDirectory: true)
        try? FileManager.default.createDirectory(at: logDir, withIntermediateDirectories: true)
        let reportPath = logDir.appendingPathComponent("doctor_host.json")
        if let compactData = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]) {
            do {
                try compactData.write(to: reportPath, options: .atomic)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: reportPath.path)
            } catch {
                print("[RESC] doctor: failed to write \(reportPath.path): \(error)")
            }
        }

        RescLog.shared.event("doctor_complete", component: component, fields: [
            "exit_code": exitCode,
            "checks_summary": checks.map { ["name": $0["name"] as? String ?? "?", "ok": $0["ok"] as? Bool ?? false] },
        ])
    }
}
