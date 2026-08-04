import Foundation

// Logs evidence of load-bearing native API calls (IMPLEMENTATION_PLAN_V11.md
// §11.3): every VTSessionSetProperty, ScreenCaptureKit call, CGVirtualDisplay
// call, socket call, etc. should have its requested-vs-actual result checked
// and logged here — never silently assumed to have succeeded. No silent
// fallback anywhere. Later phases call these; this file just provides the
// helpers.

/// Logs a native call whose result is a status/error code (0 == success by
/// convention unless `expected` says otherwise). Returns whether `status`
/// matched `expected`.
@discardableResult
func nativeCheck(_ component: String, _ call: String, status: Int32, expected: Int32 = 0,
                  extra: [String: Any] = [:]) -> Bool {
    let ok = status == expected
    var fields: [String: Any] = ["call": call, "status": status, "expected": expected, "ok": ok]
    for (key, value) in extra { fields[key] = value }
    RescLog.shared.event("native_call", component: component, fields: fields)
    return ok
}

/// Logs a native call whose result is a plain success/failure boolean (no
/// numeric status code available, e.g. an Objective-C API returning nil on
/// failure). `detail` should say what was checked.
@discardableResult
func nativeCheck(_ component: String, _ call: String, ok: Bool, detail: String,
                  extra: [String: Any] = [:]) -> Bool {
    var fields: [String: Any] = ["call": call, "ok": ok, "detail": detail]
    for (key, value) in extra { fields[key] = value }
    RescLog.shared.event("native_call", component: component, fields: fields)
    return ok
}
