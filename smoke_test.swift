#!/usr/bin/env swift
// smoke_test.swift — Post-OS-update validation for Remote Extended Screen
// Run: swift smoke_test.swift
// Verifies: CGVirtualDisplay API, ScreenCaptureKit, VideoToolbox

import Foundation
import CoreGraphics

var passed = 0
var failed = 0

func check(_ name: String, _ test: () -> Bool) {
    let ok = test()
    if ok {
        print("  ✓ \(name)")
        passed += 1
    } else {
        print("  ✗ \(name)")
        failed += 1
    }
}

print("RESC Smoke Test")
print("===============")
print("macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)")

// 1. CGVirtualDisplay API available
check("CGVirtualDisplay class exists") {
    NSClassFromString("CGVirtualDisplay") != nil
}

check("CGVirtualDisplayDescriptor class exists") {
    NSClassFromString("CGVirtualDisplayDescriptor") != nil
}

check("CGVirtualDisplayMode class exists") {
    NSClassFromString("CGVirtualDisplayMode") != nil
}

check("CGVirtualDisplaySettings class exists") {
    NSClassFromString("CGVirtualDisplaySettings") != nil
}

// 2. ScreenCaptureKit available
check("ScreenCaptureKit framework loadable") {
    let handle = dlopen("/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit", RTLD_LAZY)
    if handle != nil { dlclose(handle); return true }
    return false
}

// 3. VideoToolbox available
check("VideoToolbox framework loadable") {
    let handle = dlopen("/System/Library/Frameworks/VideoToolbox.framework/VideoToolbox", RTLD_LAZY)
    if handle != nil { dlclose(handle); return true }
    return false
}

// 4. Display enumeration
check("Can enumerate displays") {
    var count: UInt32 = 0
    CGGetOnlineDisplayList(0, nil, &count)
    return count > 0
}

// 5. Accessibility
check("Accessibility permission") {
    CGPreflightPostEventAccess()
}

print("")
print("Results: \(passed) passed, \(failed) failed")
if failed > 0 {
    print("⚠ Some checks failed. Virtual display may not work on this OS version.")
    exit(1)
} else {
    print("All checks passed.")
}
