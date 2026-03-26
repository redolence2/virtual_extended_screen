import Foundation

/// Runtime configuration, loadable from JSON file or CLI args.
struct AppConfig: Codable {
    var controlPort: UInt16 = 9870
    var displayWidth: Int = 1920
    var displayHeight: Int = 1080
    var refreshRate: Int = 60
    var bitrateBps: UInt32 = 20_000_000
    var keyframeIntervalSec: Double = 1.0
    var gracePeriodSec: Double = 30.0
    var virtualDisplayEnabled: Bool = true
    var clientHost: String? = nil
    var dumpH264Path: String? = nil

    /// Load from JSON file, falling back to defaults.
    static func load(from path: String?) -> AppConfig {
        guard let path = path,
              let data = FileManager.default.contents(atPath: path) else {
            return AppConfig()
        }
        do {
            var config = try JSONDecoder().decode(AppConfig.self, from: data)
            return config
        } catch {
            print("[RESC] Config load error: \(error). Using defaults.")
            return AppConfig()
        }
    }

    /// Apply CLI argument overrides.
    mutating func applyArgs(_ args: [String]) {
        let a = args
        if a.count > 1, let w = Int(a[1]) { displayWidth = w }
        if a.count > 2, let h = Int(a[2]) { displayHeight = h }
        if a.count > 3, let r = Int(a[3]) { refreshRate = r }

        if let idx = a.firstIndex(of: "--port"), idx + 1 < a.count {
            controlPort = UInt16(a[idx + 1]) ?? controlPort
        }
        if let idx = a.firstIndex(of: "--client"), idx + 1 < a.count {
            clientHost = a[idx + 1]
        }
        if let idx = a.firstIndex(of: "--dump-h264"), idx + 1 < a.count {
            dumpH264Path = a[idx + 1]
        }
        if let idx = a.firstIndex(of: "--bitrate"), idx + 1 < a.count {
            bitrateBps = UInt32(a[idx + 1]) ?? bitrateBps
        }
        if let idx = a.firstIndex(of: "--config"), idx + 1 < a.count {
            let loaded = AppConfig.load(from: a[idx + 1])
            // Only override non-CLI fields from config file
            if clientHost == nil { clientHost = loaded.clientHost }
        }
        if a.contains("--no-virtual-display") {
            virtualDisplayEnabled = false
        }

        // Auto-set bitrate based on resolution
        if displayWidth >= 3840 || displayHeight >= 2160 {
            if bitrateBps == 20_000_000 { bitrateBps = 50_000_000 }
        }
    }
}
