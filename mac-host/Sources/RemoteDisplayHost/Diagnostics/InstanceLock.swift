import Foundation

/// Per-profile advisory instance lock (IMPLEMENTATION_PLAN_V11.md §3).
///
/// Exactly one host process may hold a given profile's lock. A second
/// instance for the same profile must fail fast and exit cleanly with
/// `INSTANCE_LOCK_HELD` (FatalCode 20, proto/control_v3.proto) — it must
/// never kill the process that already holds the lock.
enum InstanceLock {

    /// Holds the lock fd open for the process lifetime; the flock is
    /// released automatically by the kernel when the fd closes (normal exit
    /// or crash).
    private static var heldFD: Int32?

    /// Attempts to acquire the advisory lock for `profileId`. Returns
    /// `false` if another process already holds it.
    @discardableResult
    static func acquire(profileId: String) -> Bool {
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/RESC", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("\(profileId).lock").path

        let fd = open(path, O_CREAT | O_RDWR, 0o600)
        guard fd >= 0 else {
            logResult(profileId: profileId, acquired: false, reason: "open_failed", errnoValue: errno)
            return false
        }

        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else {
            let savedErrno = errno
            close(fd)
            logResult(profileId: profileId, acquired: false, reason: "flock_held", errnoValue: savedErrno)
            return false
        }

        heldFD = fd
        logResult(profileId: profileId, acquired: true, reason: nil, errnoValue: nil)
        return true
    }

    private static func logResult(profileId: String, acquired: Bool, reason: String?, errnoValue: Int32?) {
        var fields: [String: Any] = ["acquired": acquired, "profile_id": profileId]
        if let reason { fields["reason"] = reason }
        if let errnoValue { fields["errno"] = errnoValue }
        RescLog.shared.event("instance_lock", component: "instance_lock", fields: fields)
    }
}
