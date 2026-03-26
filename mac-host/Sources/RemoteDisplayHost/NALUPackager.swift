import Foundation
import CoreMedia
import VideoToolbox

/// Extracts NAL units from VideoToolbox output and converts AVCC/HVCC → Annex B.
enum NALUPackager {

    static let startCode: [UInt8] = [0x00, 0x00, 0x00, 0x01]

    // MARK: - H.264

    /// Extract H.264 SPS/PPS parameter sets from format description.
    static func extractH264ParameterSets(from fmt: CMFormatDescription) -> Data? {
        var data = Data()
        var count: Int = 0
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            fmt, parameterSetIndex: 0,
            parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil
        )
        for i in 0..<count {
            var ptr: UnsafePointer<UInt8>?
            var size: Int = 0
            let s = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt, parameterSetIndex: i,
                parameterSetPointerOut: &ptr, parameterSetSizeOut: &size,
                parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil
            )
            guard s == noErr, let p = ptr else { continue }
            data.append(contentsOf: startCode)
            data.append(p, count: size)
        }
        return data.isEmpty ? nil : data
    }

    /// Convert H.264 AVCC sample buffer → Annex B.
    static func convertH264ToAnnexB(sampleBuffer: CMSampleBuffer) -> (Data, Bool)? {
        return convertToAnnexB(sampleBuffer: sampleBuffer, extractParams: extractH264ParameterSets)
    }

    // MARK: - HEVC

    /// Extract HEVC VPS/SPS/PPS parameter sets from format description.
    static func extractHEVCParameterSets(from fmt: CMFormatDescription) -> Data? {
        var data = Data()
        var count: Int = 0
        let s = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            fmt, parameterSetIndex: 0,
            parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil
        )
        // s may be -12712 when probing count
        for i in 0..<count {
            var ptr: UnsafePointer<UInt8>?
            var size: Int = 0
            let status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                fmt, parameterSetIndex: i,
                parameterSetPointerOut: &ptr, parameterSetSizeOut: &size,
                parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil
            )
            guard status == noErr, let p = ptr else { continue }
            data.append(contentsOf: startCode)
            data.append(p, count: size)
        }
        return data.isEmpty ? nil : data
    }

    /// Convert HEVC HVCC sample buffer → Annex B.
    static func convertHEVCToAnnexB(sampleBuffer: CMSampleBuffer) -> (Data, Bool)? {
        return convertToAnnexB(sampleBuffer: sampleBuffer, extractParams: extractHEVCParameterSets)
    }

    // MARK: - Shared

    /// Generic AVCC/HVCC → Annex B conversion.
    private static func convertToAnnexB(
        sampleBuffer: CMSampleBuffer,
        extractParams: (CMFormatDescription) -> Data?
    ) -> (Data, Bool)? {
        guard let dataBuffer = sampleBuffer.dataBuffer else { return nil }
        guard let formatDesc = sampleBuffer.formatDescription else { return nil }

        // Determine keyframe
        let isKeyframe: Bool
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[CFString: Any]],
           let first = attachments.first {
            isKeyframe = !(first[kCMSampleAttachmentKey_NotSync] as? Bool ?? false)
        } else {
            isKeyframe = true
        }

        // Get NALU length size (typically 4)
        var naluLengthSize: Int32 = 4
        // Try H.264 first, then HEVC
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            formatDesc, parameterSetIndex: 0,
            parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: nil, nalUnitHeaderLengthOut: &naluLengthSize
        )
        if naluLengthSize == 0 {
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                formatDesc, parameterSetIndex: 0,
                parameterSetPointerOut: nil, parameterSetSizeOut: nil,
                parameterSetCountOut: nil, nalUnitHeaderLengthOut: &naluLengthSize
            )
        }
        if naluLengthSize == 0 { naluLengthSize = 4 }

        // Read raw data
        var totalLength: Int = 0
        var rawBufferPtr: UnsafeMutablePointer<Int8>?
        let status = CMBlockBufferGetDataPointer(
            dataBuffer, atOffset: 0, lengthAtOffsetOut: nil,
            totalLengthOut: &totalLength, dataPointerOut: &rawBufferPtr
        )
        guard status == kCMBlockBufferNoErr, let bufferPtr = rawBufferPtr else { return nil }

        var result = Data()

        // Prepend parameter sets on keyframes
        if isKeyframe {
            if let params = extractParams(formatDesc) {
                result.append(params)
            }
        }

        // Walk NALUs
        var offset = 0
        while offset < totalLength - Int(naluLengthSize) {
            var naluLength: UInt32 = 0
            memcpy(&naluLength, bufferPtr + offset, Int(naluLengthSize))
            naluLength = CFSwapInt32BigToHost(naluLength)
            offset += Int(naluLengthSize)

            guard naluLength > 0, offset + Int(naluLength) <= totalLength else { break }

            result.append(contentsOf: startCode)
            result.append(Data(bytes: bufferPtr + offset, count: Int(naluLength)))
            offset += Int(naluLength)
        }

        return (result, isKeyframe)
    }
}
