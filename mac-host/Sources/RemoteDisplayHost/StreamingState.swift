import Foundation

/// Thread-safe streaming state. All access is serialized on a dedicated queue.
/// Eliminates data races on activeVideoSender, hasSentKeyframe, etc.
final class StreamingState: @unchecked Sendable {

    private let queue = DispatchQueue(label: "com.resc.streaming-state")

    private var _sender: VideoSender?
    private var _hasSentKeyframe = false
    private var _streamID: UInt32 = 0
    private var _configID: UInt32 = 0
    private var _isStreaming = false

    // MARK: - Atomic reads (safe from any thread)

    var isStreaming: Bool {
        queue.sync { _isStreaming }
    }

    // MARK: - State transitions (always on serial queue)

    func startStreaming(sender: VideoSender, streamID: UInt32, configID: UInt32) {
        queue.sync {
            _sender = sender
            _hasSentKeyframe = false
            _streamID = streamID
            _configID = configID
            _isStreaming = true
        }
    }

    func stopStreaming() {
        queue.sync {
            _sender?.disconnect()
            _sender = nil
            _hasSentKeyframe = false
            _isStreaming = false
        }
    }

    /// Called from encoder output callback. Thread-safe.
    /// Returns true if the frame was sent, false if skipped.
    @discardableResult
    func sendFrame(data: Data, isKeyframe: Bool, timestampUs: UInt64) -> Bool {
        queue.sync {
            guard let sender = _sender, _isStreaming else { return false }

            // Gate: skip non-keyframes until first keyframe sent (SPS/PPS)
            if !_hasSentKeyframe {
                if isKeyframe {
                    _hasSentKeyframe = true
                    print("[RESC] First keyframe sent to client (\(data.count / 1024)KB)")
                } else {
                    return false
                }
            }

            sender.sendFrame(data: data, isKeyframe: isKeyframe, timestampUs: timestampUs)
            return true
        }
    }

    var stats: (packets: UInt64, bytes: UInt64) {
        queue.sync { _sender?.stats ?? (0, 0) }
    }
}
