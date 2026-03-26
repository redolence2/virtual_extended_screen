import Foundation

/// Adaptive bitrate controller based on receiver Stats.
/// Reduces bitrate on loss/drops, probes up when stable.
final class BitrateAdapter {

    private let encoder: VideoEncoder
    private var currentBitrate: UInt32
    private let minBitrate: UInt32 = 2_000_000   // 2 Mbps floor
    private let maxBitrate: UInt32                 // ceiling (initial bitrate)
    private var stableSeconds: Int = 0
    private let stableThreshold = 5 // 5 consecutive stable seconds to probe up

    init(encoder: VideoEncoder, initialBitrate: UInt32) {
        self.encoder = encoder
        self.currentBitrate = initialBitrate
        self.maxBitrate = initialBitrate
    }

    /// Called every ~1 second with receiver stats.
    func onStats(packetLossRate: Float, frameDropRate: Float, decodeP95Ms: UInt32) {
        let shouldReduce = frameDropRate > 0.05 || packetLossRate > 0.02

        if shouldReduce {
            stableSeconds = 0
            let newBitrate = max(minBitrate, UInt32(Double(currentBitrate) * 0.8))
            if newBitrate != currentBitrate {
                currentBitrate = newBitrate
                encoder.updateBitrate(currentBitrate)
                print("[RESC] Bitrate ↓ \(currentBitrate / 1_000_000)Mbps (loss=\(String(format: "%.1f", packetLossRate * 100))%, drops=\(String(format: "%.1f", frameDropRate * 100))%)")
            }
        } else {
            stableSeconds += 1
            if stableSeconds >= stableThreshold {
                stableSeconds = 0
                let newBitrate = min(maxBitrate, UInt32(Double(currentBitrate) * 1.05))
                if newBitrate != currentBitrate {
                    currentBitrate = newBitrate
                    encoder.updateBitrate(currentBitrate)
                    print("[RESC] Bitrate ↑ \(currentBitrate / 1_000_000)Mbps (stable)")
                }
            }
        }
    }

    /// Called on IDR request — reduce by 10%.
    func onIDRRequest() {
        stableSeconds = 0
        currentBitrate = max(minBitrate, UInt32(Double(currentBitrate) * 0.9))
        encoder.updateBitrate(currentBitrate)
    }

    var bitrate: UInt32 { currentBitrate }
}
