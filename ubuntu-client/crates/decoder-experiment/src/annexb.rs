//! Annex-B NAL-unit splitting for the `--force-zero-output` mode's crafted
//! parameter-set-only packet (`A00_COMPLETION_REPORT_AMENDED_review.md`
//! finding 5 / `..._response_review.md` amendment 5; ERR-03 /
//! `A00_REMEDIATION_PLAN.md` §3 D2 escape hatch).
//!
//! `decoder-experiment` already demuxes the sample via ffmpeg's own `hevc`
//! parser (`ictx.packets()` in `main.rs`), which hands back one
//! already-assembled access unit (AU) per packet, in Annex-B byte-stream
//! form (start codes intact — this crate feeds `packet.data()` straight
//! into `avcodec_send_packet`, no bitstream filter). This module goes one
//! level deeper, splitting a *single* AU's raw bytes into its constituent
//! NAL units so the caller can pick out only the parameter-set NALs
//! (VPS/SPS/PPS) and re-encode them as a standalone, independently
//! submittable Annex-B buffer carrying no VCL/slice data at all.

/// One HEVC NAL unit found inside an Annex-B byte buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnnexBNal {
    /// Byte range of this NAL's payload (header + RBSP), *excluding* its
    /// start-code prefix — `data[range]` is exactly what a fresh start code
    /// should be prepended to for a valid standalone re-encoding.
    range: std::ops::Range<usize>,
    /// HEVC `nal_unit_type` — `(first_payload_byte >> 1) & 0x3F` (ITU-T
    /// H.265 §7.3.1.2). 32 = VPS, 33 = SPS, 34 = PPS.
    nal_type: u8,
}

const NAL_TYPE_VPS: u8 = 32;
const NAL_TYPE_SPS: u8 = 33;
const NAL_TYPE_PPS: u8 = 34;

/// Scans `data` for `00 00 01` start-code occurrences (matching both the
/// 3-byte `00 00 01` and 4-byte `00 00 00 01` prefixes — the latter
/// contains the former as its last three bytes, so a left-to-right scan
/// naturally finds the correct boundary either way) and returns the offset
/// of each match's leading `0x00`.
fn find_start_codes(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
}

/// Splits one Annex-B buffer (a run of NAL units, each preceded by a 3- or
/// 4-byte start code) into its constituent [`AnnexBNal`]s, in stream order.
/// A start code with no following byte (malformed tail) is silently
/// dropped rather than treated as fatal — this tool only ever reads real
/// sample bytes pulled straight from ffmpeg's own demuxer, never untrusted
/// input.
fn split_annexb_nals(data: &[u8]) -> Vec<AnnexBNal> {
    let starts = find_start_codes(data);
    let mut nals = Vec::with_capacity(starts.len());
    for (idx, &start) in starts.iter().enumerate() {
        let nal_data_start = start + 3;
        if nal_data_start >= data.len() {
            continue;
        }
        let nal_data_end = starts.get(idx + 1).copied().unwrap_or(data.len());
        if nal_data_end <= nal_data_start {
            continue;
        }
        let nal_type = (data[nal_data_start] >> 1) & 0x3F;
        nals.push(AnnexBNal { range: nal_data_start..nal_data_end, nal_type });
    }
    nals
}

/// Extracts only the parameter-set NALs (VPS=32/SPS=33/PPS=34, per
/// `A00_COMPLETION_REPORT_AMENDED_review.md` finding 5) from one AU's raw
/// Annex-B `au_data`, re-encoding them — each with a fresh canonical 4-byte
/// `00 00 00 01` start code, in their original relative order — into a
/// standalone Annex-B buffer that carries no VCL/slice data whatsoever.
///
/// Returns `(crafted_bytes, nal_types_included)`. Both are empty if
/// `au_data` carries no parameter sets at all (an unexpected sample
/// structure) — callers must treat that as "strategy A unavailable" rather
/// than submitting an empty packet.
pub fn extract_param_sets(au_data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    const START_CODE: [u8; 4] = [0, 0, 0, 1];
    let mut out = Vec::new();
    let mut types_included = Vec::new();
    for nal in split_annexb_nals(au_data) {
        if matches!(nal.nal_type, NAL_TYPE_VPS | NAL_TYPE_SPS | NAL_TYPE_PPS) {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(&au_data[nal.range.clone()]);
            types_included.push(nal.nal_type);
        }
    }
    (out, types_included)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one Annex-B NAL: `start_code` (3 or 4 bytes) + a 2-byte HEVC
    /// NAL header encoding `nal_type` + `payload_tail`.
    fn nal(start_code: &[u8], nal_type: u8, payload_tail: &[u8]) -> Vec<u8> {
        let mut v = start_code.to_vec();
        // byte0: forbidden_zero_bit(0) | nal_unit_type(6) | layer_id_high(1,0)
        v.push((nal_type << 1) & 0xFE);
        // byte1: layer_id_low(5) | temporal_id_plus1(3) — arbitrary nonzero.
        v.push(0x01);
        v.extend_from_slice(payload_tail);
        v
    }

    #[test]
    fn splits_mixed_3_and_4_byte_start_codes_and_recovers_types() {
        let mut buf = Vec::new();
        buf.extend(nal(&[0, 0, 0, 1], 32, &[0xAA, 0xBB])); // VPS, 4-byte start code
        buf.extend(nal(&[0, 0, 1], 33, &[0xCC])); // SPS, 3-byte start code
        buf.extend(nal(&[0, 0, 1], 34, &[0xDD, 0xEE, 0xFF])); // PPS
        buf.extend(nal(&[0, 0, 0, 1], 19, &[0x11, 0x22])); // IDR_W_RADL slice (VCL)

        let nals = split_annexb_nals(&buf);
        assert_eq!(
            nals.iter().map(|n| n.nal_type).collect::<Vec<_>>(),
            vec![32, 33, 34, 19]
        );
    }

    #[test]
    fn extract_param_sets_keeps_only_vps_sps_pps_in_order_and_drops_vcl() {
        let mut buf = Vec::new();
        buf.extend(nal(&[0, 0, 0, 1], 32, &[0xAA]));
        buf.extend(nal(&[0, 0, 1], 33, &[0xBB]));
        buf.extend(nal(&[0, 0, 1], 34, &[0xCC]));
        buf.extend(nal(&[0, 0, 0, 1], 19, &[0xDD, 0xDD])); // must not survive extraction

        let (crafted, types) = extract_param_sets(&buf);
        assert_eq!(types, vec![32, 33, 34]);

        let crafted_types: Vec<u8> = split_annexb_nals(&crafted).into_iter().map(|n| n.nal_type).collect();
        assert_eq!(crafted_types, vec![32, 33, 34], "crafted buffer must carry no VCL NAL");
    }

    #[test]
    fn no_param_sets_yields_empty_crafted_buffer() {
        let mut buf = Vec::new();
        buf.extend(nal(&[0, 0, 0, 1], 19, &[0xDD]));
        let (crafted, types) = extract_param_sets(&buf);
        assert!(crafted.is_empty());
        assert!(types.is_empty());
    }

    #[test]
    fn empty_input_yields_no_nals() {
        assert!(split_annexb_nals(&[]).is_empty());
        let (crafted, types) = extract_param_sets(&[]);
        assert!(crafted.is_empty());
        assert!(types.is_empty());
    }
}
