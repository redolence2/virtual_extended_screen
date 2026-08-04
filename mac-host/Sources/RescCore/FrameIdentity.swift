import Foundation

/// Exact per-frame capture identity, minus the pixels — what travels from
/// the capture callback through the encoder submit-context to the wire
/// frameID mapping (A00_REMEDIATION_PLAN.md §4 items 4–5). Carried as the
/// encoder's per-submit context (`VideoEncoder.encode(identity:)`) and
/// recovered unchanged in the asynchronous output callback, so the host
/// trace's frameID→capture-identity join is exact rather than latest-wins.
public struct FrameIdentity: Equatable {
    /// The capture run this frame belongs to — see `CapturedFrame.generation`.
    public let generation: UInt64
    /// Per-run capture sequence — see `CapturedFrame.captureSeq`.
    public let captureSeq: UInt64
    /// Host continuous-monotonic microseconds — see `CapturedFrame.captureTsUs`.
    public let captureTsUs: UInt64
    /// Where `captureTsUs` came from — see `CapturedFrame.tsSource`.
    public let tsSource: CaptureTsSource
    /// Half-width clock uncertainty in microseconds — see `CapturedFrame.uncertaintyUs`.
    public let uncertaintyUs: UInt64

    public init(generation: UInt64, captureSeq: UInt64, captureTsUs: UInt64,
                tsSource: CaptureTsSource, uncertaintyUs: UInt64) {
        self.generation = generation
        self.captureSeq = captureSeq
        self.captureTsUs = captureTsUs
        self.tsSource = tsSource
        self.uncertaintyUs = uncertaintyUs
    }
}

public extension CapturedFrame {
    /// The frame's identity without its pixel payload — what gets threaded
    /// through the encoder submit-context instead of the whole `CapturedFrame`
    /// (A00_REMEDIATION_PLAN.md §4 item 4).
    var identity: FrameIdentity {
        FrameIdentity(generation: generation, captureSeq: captureSeq, captureTsUs: captureTsUs,
                      tsSource: tsSource, uncertaintyUs: uncertaintyUs)
    }
}
