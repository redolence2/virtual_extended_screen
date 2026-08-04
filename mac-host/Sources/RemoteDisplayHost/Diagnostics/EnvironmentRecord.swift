import Foundation

/// Emits the one-time startup environment record (IMPLEMENTATION_PLAN_V11.md
/// §11.2) that the host logs before doing anything else. Captures enough
/// build/OS evidence to diagnose issues on an unlisted or upgraded macOS
/// without guessing — never crashes even if git or a sysctl is unavailable.
enum EnvironmentRecord {

    /// The wire protocol this host currently speaks. The v3 profile-based
    /// contract (IMPLEMENTATION_PLAN_V11.md §4, proto/control_v3.proto) is
    /// not wired into HostSession yet — this reflects the running v1 wire
    /// (see ProtocolConstants.swift). Update once the host speaks v3.
    private static let runningProtocolVersion = ProtocolConstants.protocolVersion

    static func emit() {
        let repoPath = repositoryPath()
        let dirty = buildDirty(repoPath: repoPath)
        let osBuild = kernOSVersion()

        RescLog.shared.event("startup_environment", component: "environment", fields: [
            "build_commit": buildCommit(repoPath: repoPath),
            "build_dirty": dirty ?? NSNull(),
            "protocol_version": runningProtocolVersion,
            "os_version": ProcessInfo.processInfo.operatingSystemVersionString,
            "os_build": osBuild ?? NSNull(),
            "cpu_arch": machineArchitecture(),
        ])
    }

    // MARK: - Build identity

    /// Repo root, discovered from this file's own compile-time path:
    /// `<repo>/mac-host/Sources/RemoteDisplayHost/Diagnostics/EnvironmentRecord.swift`.
    private static func repositoryPath() -> String {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Diagnostics/
            .deletingLastPathComponent() // RemoteDisplayHost/
            .deletingLastPathComponent() // Sources/
            .deletingLastPathComponent() // mac-host/
            .path
    }

    private static func buildCommit(repoPath: String) -> String {
        if let env = ProcessInfo.processInfo.environment["RESC_BUILD_COMMIT"], !env.isEmpty {
            return env
        }
        if let head = runGit(["rev-parse", "HEAD"], repoPath: repoPath), !head.isEmpty {
            return head
        }
        return "unknown"
    }

    /// `nil` (logged as JSON null) when git itself could not be run.
    private static func buildDirty(repoPath: String) -> Bool? {
        guard let status = runGit(["status", "--porcelain"], repoPath: repoPath) else { return nil }
        return !status.isEmpty
    }

    private static func runGit(_ arguments: [String], repoPath: String) -> String? {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        task.arguments = ["-C", repoPath] + arguments
        let stdout = Pipe()
        task.standardOutput = stdout
        task.standardError = Pipe()
        do {
            try task.run()
        } catch {
            return nil // git unavailable — do not crash
        }
        task.waitUntilExit()
        guard task.terminationStatus == 0 else { return nil }
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        return String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - OS / hardware identity

    /// `sysctl kern.osversion` — the exact build string (e.g. "26A5388g"),
    /// finer-grained than `ProcessInfo.operatingSystemVersionString`.
    private static func kernOSVersion() -> String? {
        var size = 0
        guard sysctlbyname("kern.osversion", nil, &size, nil, 0) == 0, size > 0 else { return nil }
        var buffer = [CChar](repeating: 0, count: size)
        guard sysctlbyname("kern.osversion", &buffer, &size, nil, 0) == 0 else { return nil }
        return String(cString: buffer)
    }

    private static func machineArchitecture() -> String {
        var uts = utsname()
        uname(&uts)
        return withUnsafeBytes(of: &uts.machine) { raw -> String in
            String(cString: raw.baseAddress!.assumingMemoryBound(to: CChar.self))
        }
    }
}
