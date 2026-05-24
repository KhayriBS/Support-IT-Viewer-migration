//! H.264 / Annex-B parsing and NAL handling helpers.
//!
//! Pure functions used by the stream sender loops (OpenH264, MediaFoundation,
//! FFmpeg). Kept in their own module so they can be unit-tested in isolation
//! without dragging the whole WebRTC pipeline.

use std::sync::Arc;

use webrtc::peer_connection::RTCPeerConnection;

/// Parse the negotiated H264 RTP payload type from an SDP. Returns `None` if no
/// H264/90000 codec line is present.
pub(super) fn parse_h264_payload_type_from_sdp(sdp: &str) -> Option<u8> {
    for raw in sdp.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("a=rtpmap:") else {
            continue;
        };

        // Examples:
        // a=rtpmap:102 H264/90000
        // a=rtpmap:96 H264/90000
        let mut parts = rest.split_whitespace();
        let pt_str = parts.next()?;
        let codec_str = parts.next()?;

        let Some((codec, clock)) = codec_str.split_once('/') else {
            continue;
        };

        if codec.eq_ignore_ascii_case("H264") && clock == "90000" {
            if let Ok(pt) = pt_str.parse::<u8>() {
                return Some(pt);
            }
        }
    }

    None
}

/// Extract the first SSRC value found in an SDP (`a=ssrc:...`).
pub(super) fn parse_first_ssrc_from_sdp(sdp: &str) -> Option<u32> {
    for raw in sdp.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("a=ssrc:") else {
            continue;
        };
        // Example: a=ssrc:123456789 cname:...
        let chars = rest.chars();
        let mut num = String::new();
        for ch in chars {
            if ch.is_ascii_digit() {
                num.push(ch);
            } else {
                break;
            }
        }
        if num.is_empty() {
            continue;
        }
        if let Ok(value) = num.parse::<u32>() {
            if value != 0 {
                return Some(value);
            }
        }
    }
    None
}

pub(super) async fn resolve_h264_payload_type(peer: &Arc<RTCPeerConnection>) -> Option<u8> {
    let local = peer.local_description().await?;
    parse_h264_payload_type_from_sdp(&local.sdp)
}

pub(super) async fn resolve_video_ssrc(peer: &Arc<RTCPeerConnection>) -> Option<u32> {
    let local = peer.local_description().await?;
    parse_first_ssrc_from_sdp(&local.sdp)
}

/// Reorder a list of NAL units so SPS (type 7) and PPS (type 8) come first
/// (required by some decoders before an IDR), and cache the most recent SPS/PPS
/// for later re-injection if the encoder is reset mid-stream.
///
/// Returns the reordered list and a flag indicating whether an IDR (type 5) was
/// present.
pub(super) fn reorder_and_cache_sps_pps<'a>(
    nalus: Vec<&'a [u8]>,
    cached_sps: &mut Option<Vec<u8>>,
    cached_pps: &mut Option<Vec<u8>>,
) -> (Vec<&'a [u8]>, bool) {
    // Une frame H264 contient typiquement 1 SPS + 1 PPS + 1-4 NAL autres.
    // Pre-allouer evite des reallocs a 60 FPS sur le hot path d'encoding.
    let mut sps: Vec<&'a [u8]> = Vec::with_capacity(1);
    let mut pps: Vec<&'a [u8]> = Vec::with_capacity(1);
    let mut others: Vec<&'a [u8]> = Vec::with_capacity(4);
    let mut has_idr = false;

    for nal in nalus {
        let Some(&first) = nal.first() else {
            continue;
        };
        let nal_type = first & 0x1f;
        match nal_type {
            5 => {
                has_idr = true;
                others.push(nal);
            }
            7 => {
                *cached_sps = Some(nal.to_vec());
                sps.push(nal);
            }
            8 => {
                *cached_pps = Some(nal.to_vec());
                pps.push(nal);
            }
            _ => others.push(nal),
        }
    }

    let mut ordered = Vec::with_capacity(sps.len() + pps.len() + others.len() + 2);

    ordered.extend_from_slice(&sps);
    ordered.extend_from_slice(&pps);
    ordered.extend_from_slice(&others);
    (ordered, has_idr)
}

#[derive(Default, Clone, Copy)]
pub(super) struct NalSummary {
    pub nalus: usize,
    pub has_sps: bool,
    pub has_pps: bool,
    pub has_idr: bool,
}

/// Split an Annex-B (start-code 0x000001 or 0x00000001) or AVCC (4-byte
/// length-prefixed) encoded byte stream into raw NAL unit slices. Returned
/// slices exclude the start code / length prefix.
pub(super) fn split_annexb_nalus(data: &[u8]) -> Vec<&[u8]> {
    // Frames typiques : 1-2 NAL pour un P-frame, 4-5 pour une IDR.
    // 8 couvre 99% des cas sans gaspiller.
    let mut nalus = Vec::with_capacity(8);
    let mut i = 0usize;
    let len = data.len();

    let find_start_code = |from: usize| -> Option<(usize, usize)> {
        let mut j = from;
        while j + 3 < len {
            if data[j] == 0 && data[j + 1] == 0 {
                if data[j + 2] == 1 {
                    return Some((j, 3));
                }
                if j + 3 < len && data[j + 2] == 0 && data[j + 3] == 1 {
                    return Some((j, 4));
                }
            }
            j += 1;
        }
        None
    };

    while let Some((sc_pos, sc_len)) = find_start_code(i) {
        let nal_start = sc_pos + sc_len;
        if let Some((next_sc_pos, _)) = find_start_code(nal_start) {
            if next_sc_pos > nal_start {
                nalus.push(&data[nal_start..next_sc_pos]);
            }
            i = next_sc_pos;
        } else {
            if nal_start < len {
                nalus.push(&data[nal_start..len]);
            }
            break;
        }
    }

    if nalus.is_empty() && !data.is_empty() {
        // Fallback: try AVCC / length-prefixed NAL units (4-byte big-endian lengths).
        let mut offset = 0usize;
        while offset + 4 <= len {
            let size = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;

            if size == 0 || offset + size > len {
                nalus.clear();
                break;
            }

            nalus.push(&data[offset..offset + size]);
            offset += size;
        }

        // If parsing failed or produced nothing, assume the input is a single NAL unit.
        if nalus.is_empty() {
            nalus.push(data);
        }
    }

    nalus
}

pub(super) fn summarize_nalus(nalus: &[&[u8]]) -> NalSummary {
    let mut summary = NalSummary {
        nalus: nalus.len(),
        ..NalSummary::default()
    };
    for nal in nalus {
        let Some(&first) = nal.first() else {
            continue;
        };
        let nal_type = first & 0x1f;
        match nal_type {
            5 => summary.has_idr = true,
            7 => summary.has_sps = true,
            8 => summary.has_pps = true,
            _ => {}
        }
    }
    summary
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SDP parsing ──────────────────────────────────────────────────────────

    #[test]
    fn parse_payload_type_finds_h264_line() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\n";
        assert_eq!(parse_h264_payload_type_from_sdp(sdp), Some(102));
    }

    #[test]
    fn parse_payload_type_case_insensitive_codec() {
        let sdp = "a=rtpmap:96 h264/90000\r\n";
        assert_eq!(parse_h264_payload_type_from_sdp(sdp), Some(96));
    }

    #[test]
    fn parse_payload_type_ignores_other_codecs() {
        let sdp = "a=rtpmap:111 opus/48000/2\r\na=rtpmap:120 VP8/90000\r\n";
        assert_eq!(parse_h264_payload_type_from_sdp(sdp), None);
    }

    #[test]
    fn parse_payload_type_rejects_wrong_clock_rate() {
        // Some weird config that's not 90 kHz — should be ignored.
        let sdp = "a=rtpmap:99 H264/48000\r\n";
        assert_eq!(parse_h264_payload_type_from_sdp(sdp), None);
    }

    #[test]
    fn parse_first_ssrc_extracts_number() {
        let sdp = "a=ssrc:123456789 cname:abc\r\na=ssrc:987 cname:def\r\n";
        assert_eq!(parse_first_ssrc_from_sdp(sdp), Some(123456789));
    }

    #[test]
    fn parse_first_ssrc_skips_zero() {
        // SSRC=0 is invalid per RFC 3550; we must keep searching.
        let sdp = "a=ssrc:0 cname:abc\r\na=ssrc:42 cname:def\r\n";
        assert_eq!(parse_first_ssrc_from_sdp(sdp), Some(42));
    }

    #[test]
    fn parse_first_ssrc_returns_none_when_absent() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\n";
        assert_eq!(parse_first_ssrc_from_sdp(sdp), None);
    }

    // ── Annex-B splitting ────────────────────────────────────────────────────

    #[test]
    fn split_annexb_3byte_start_codes() {
        // Two NALs separated by 3-byte start codes (0x00 0x00 0x01).
        let data = [
            0, 0, 1, 0x67, 0x42, // NAL #1: SPS-like (type=7)
            0, 0, 1, 0x68, 0xce, // NAL #2: PPS-like (type=8)
        ];
        let nalus = split_annexb_nalus(&data);
        assert_eq!(nalus.len(), 2);
        assert_eq!(nalus[0], &[0x67, 0x42]);
        assert_eq!(nalus[1], &[0x68, 0xce]);
    }

    #[test]
    fn split_annexb_4byte_start_codes() {
        // Two NALs separated by 4-byte start codes (0x00 0x00 0x00 0x01).
        let data = [
            0, 0, 0, 1, 0x67, 0x42,
            0, 0, 0, 1, 0x65, 0x88, // IDR (type=5)
        ];
        let nalus = split_annexb_nalus(&data);
        assert_eq!(nalus.len(), 2);
        assert_eq!(nalus[0], &[0x67, 0x42]);
        assert_eq!(nalus[1], &[0x65, 0x88]);
    }

    #[test]
    fn split_annexb_avcc_fallback() {
        // No start code — should fall back to 4-byte length-prefixed format.
        let data = [
            0, 0, 0, 2, 0x67, 0x42, // length=2, NAL: 0x67 0x42
            0, 0, 0, 2, 0x68, 0xce, // length=2, NAL: 0x68 0xce
        ];
        let nalus = split_annexb_nalus(&data);
        assert_eq!(nalus.len(), 2);
        assert_eq!(nalus[0], &[0x67, 0x42]);
        assert_eq!(nalus[1], &[0x68, 0xce]);
    }

    #[test]
    fn split_annexb_empty_input() {
        let nalus = split_annexb_nalus(&[]);
        assert!(nalus.is_empty());
    }

    #[test]
    fn split_annexb_single_nal_treated_as_one_unit_when_no_marker() {
        // Single NAL without start code, AVCC length parsing fails → treated as one unit.
        let data = [0x65, 0x88, 0x80];
        let nalus = split_annexb_nalus(&data);
        assert_eq!(nalus.len(), 1);
        assert_eq!(nalus[0], &[0x65, 0x88, 0x80]);
    }

    // ── Reorder + cache ──────────────────────────────────────────────────────

    #[test]
    fn reorder_puts_sps_pps_before_idr() {
        // Order in: IDR, SPS, PPS — must come out: SPS, PPS, IDR.
        let idr: &[u8] = &[0x65, 0x88]; // type=5
        let sps: &[u8] = &[0x67, 0x42]; // type=7
        let pps: &[u8] = &[0x68, 0xce]; // type=8
        let mut cached_sps = None;
        let mut cached_pps = None;
        let (ordered, has_idr) =
            reorder_and_cache_sps_pps(vec![idr, sps, pps], &mut cached_sps, &mut cached_pps);
        assert!(has_idr);
        assert_eq!(ordered.len(), 3);
        // SPS first, then PPS, then IDR
        assert_eq!(ordered[0], sps);
        assert_eq!(ordered[1], pps);
        assert_eq!(ordered[2], idr);
    }

    #[test]
    fn reorder_caches_sps_and_pps_for_next_call() {
        let sps: &[u8] = &[0x67, 0x42, 0x00];
        let pps: &[u8] = &[0x68, 0xce, 0x01];
        let mut cached_sps = None;
        let mut cached_pps = None;
        let _ = reorder_and_cache_sps_pps(vec![sps, pps], &mut cached_sps, &mut cached_pps);
        assert_eq!(cached_sps.as_deref(), Some(sps));
        assert_eq!(cached_pps.as_deref(), Some(pps));
    }

    #[test]
    fn reorder_without_idr_returns_false() {
        let p_frame: &[u8] = &[0x61, 0x9a]; // non-IDR slice (type=1)
        let mut cached_sps = None;
        let mut cached_pps = None;
        let (_, has_idr) =
            reorder_and_cache_sps_pps(vec![p_frame], &mut cached_sps, &mut cached_pps);
        assert!(!has_idr);
    }

    // ── NAL summary ──────────────────────────────────────────────────────────

    #[test]
    fn summarize_nalus_flags_idr_sps_pps() {
        let sps: &[u8] = &[0x67];
        let pps: &[u8] = &[0x68];
        let idr: &[u8] = &[0x65];
        let summary = summarize_nalus(&[sps, pps, idr]);
        assert_eq!(summary.nalus, 3);
        assert!(summary.has_sps);
        assert!(summary.has_pps);
        assert!(summary.has_idr);
    }

    #[test]
    fn summarize_nalus_p_frame_only() {
        let p: &[u8] = &[0x61];
        let summary = summarize_nalus(&[p]);
        assert_eq!(summary.nalus, 1);
        assert!(!summary.has_sps);
        assert!(!summary.has_pps);
        assert!(!summary.has_idr);
    }

    #[test]
    fn summarize_nalus_empty_input() {
        let summary = summarize_nalus(&[]);
        assert_eq!(summary.nalus, 0);
        assert!(!summary.has_sps);
        assert!(!summary.has_pps);
        assert!(!summary.has_idr);
    }
}
