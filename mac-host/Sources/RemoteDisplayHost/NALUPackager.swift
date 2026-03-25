import Foundation
import CoreMedia
import VideoToolbox

/// Extracts NAL units from VideoToolbox output and converts AVCC → Annex B format.
/// Annex B is required for streaming (each NALU prefixed with 0x00000001 start code).
enum NALUPackager {

    /// Annex B start code (4 bytes).
    static let startCode: [UInt8] = [0x00, 0x00, 0x00, 0x01]

    /// Extracts SPS and PPS from a CMFormatDescription (H.264 parameter sets).
    /// Returns Annex B encoded parameter sets (start code + SPS + start code + PPS).
    static func extractParameterSets(from formatDescription: CMFormatDescription) -> Data? {
        var data = Data()

        // Get number of parameter sets
        var paramSetCount: Int = 0
        var status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            formatDescription, parameterSetIndex: 0,
            parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: &paramSetCount, nalUnitHeaderLengthOut: nil
        )

        // status -12712 means "index out of range" which is expected when probing count
        guard status == noErr || status == -12712 else {
            return nil
        }

        for i in 0..<paramSetCount {
            var paramSetPtr: UnsafePointer<UInt8>?
            var paramSetSize: Int = 0
            status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                formatDescription, parameterSetIndex: i,
                parameterSetPointerOut: &paramSetPtr, parameterSetSizeOut: &paramSetSize,
                parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil
            )
            guard status == noErr, let ptr = paramSetPtr else { continue }

            data.append(contentsOf: startCode)
            data.append(ptr, count: paramSetSize)
        }

        return data.isEmpty ? nil : data
    }

    /// Converts a CMSampleBuffer (AVCC format from VideoToolbox) to Annex B format.
    /// AVCC: each NALU is prefixed with its length (typically 4 bytes big-endian).
    /// Annex B: each NALU is prefixed with 0x00000001 start code.
    ///
    /// Returns (annexBData, isKeyframe).
    static func convertToAnnexB(sampleBuffer: CMSampleBuffer) -> (Data, Bool)? {
        guard let dataBuffer = sampleBuffer.dataBuffer else { return nil }

        // Check if keyframe
        let isKeyframe: Bool
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[CFString: Any]],
           let first = attachments.first {
            let notSync = first[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
            isKeyframe = !notSync
        } else {
            isKeyframe = true // assume keyframe if no attachments
        }

        // Get NALU length prefix size (typically 4 bytes for H.264)
        guard let formatDesc = sampleBuffer.formatDescription else { return nil }
        var naluLengthSize: Int32 = 0
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            formatDesc, parameterSetIndex: 0,
            parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: nil, nalUnitHeaderLengthOut: &naluLengthSize
        )
        if naluLengthSize == 0 { naluLengthSize = 4 } // default

        // Read raw data
        var totalLength: Int = 0
        var rawBufferPtr: UnsafeMutablePointer<Int8>?
        let status = CMBlockBufferGetDataPointer(
            dataBuffer, atOffset: 0, lengthAtOffsetOut: nil,
            totalLengthOut: &totalLength, dataPointerOut: &rawBufferPtr
        )
        guard status == kCMBlockBufferNoErr, let bufferPtr = rawBufferPtr else { return nil }

        var result = Data()

        // If keyframe, prepend SPS/PPS
        if isKeyframe {
            if let paramSets = extractParameterSets(from: formatDesc) {
                result.append(paramSets)
            }
        }

        // Walk AVCC NALUs and convert to Annex B
        var offset = 0
        while offset < totalLength - Int(naluLengthSize) {
            // Read NALU length (big-endian)
            var naluLength: UInt32 = 0
            memcpy(&naluLength, bufferPtr + offset, Int(naluLengthSize))
            naluLength = CFSwapInt32BigToHost(naluLength)
            offset += Int(naluLengthSize)

            guard naluLength > 0, offset + Int(naluLength) <= totalLength else { break }

            // Write start code + NALU data
            result.append(contentsOf: startCode)
            result.append(Data(bytes: bufferPtr + offset, count: Int(naluLength)))
            offset += Int(naluLength)
        }

        return (result, isKeyframe)
    }
}
