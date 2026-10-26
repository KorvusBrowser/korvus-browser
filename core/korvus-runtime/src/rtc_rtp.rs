//! `rtc_rtp.rs` -- RTP packetization for the WebRTC send path (goal A, Slice 1c): an encoded VP8 frame to
//! one or more RTP packets, the RFC 3550 fixed header plus the RFC 7741 VP8 payload descriptor, fragmented
//! to fit the path MTU. Pure byte-building, no I/O and no crypto (the SRTP protect in `rtc_srtp.rs` wraps
//! each packet after this); unit-tested in isolation.
//!
//! Compiled under `webrtc-srtp` (the no-libvpx send-path core), so its tests run without the libvpx build.
//!
//! DEFERRED to the final 1c commit (the sender): the RFC 8285 one-byte header-extension block
//! (`mid` / abs-send-time / transport-wide-cc) that the persona offer advertises. abs-send-time is a
//! send-instant value and transport-wide-cc is a connection-wide counter, so both are assembled in the
//! sender where that state lives; until then packets carry X=0 (a fidelity gap, not a correctness one --
//! the receiver reassembles a frame from the marker bit and the shared timestamp, which are correct here).
#![allow(dead_code)] // the sender that calls packetize lands in the final 1c commit; the unit tests exercise it now.

/// The path-MTU budget a protected packet must fit within (header + descriptor + payload + SRTP tag).
pub(crate) const MAX_RTP_PACKET: usize = 1200;
const RTP_HEADER: usize = 12;
/// The VP8 descriptor this profile emits: X=1, I=1, M=1 (a 15-bit PictureID), as libwebrtc sends.
const VP8_DESC: usize = 4;

/// The RFC 8285 one-byte header-extension block korvus sends (0xBEDE profile): abs-send-time (id 2, 3
/// bytes) + transport-wide-cc (id 3, 2 bytes), the two per-packet extensions the persona video m-section
/// advertises that libwebrtc sets on every packet. 7 bytes of elements padded to 8, under the 4-byte
/// `0xBEDE`+length word = 12 bytes. (The `mid` extension, id 4, is deferred: it needs the sending
/// section's mid threaded through the op.)
const EXT_BLOCK: usize = 12;
/// The RTP header length when the extension block is present (what the sender hands SRTP as `header_len`).
pub(crate) const HEADER_WITH_EXT: usize = RTP_HEADER + EXT_BLOCK;

/// The RTP sending state for one stream: the SSRC and payload type are fixed for the connection; the
/// sequence number increments per packet, the PictureID once per frame, and the transport-wide-cc
/// sequence (a connection-wide counter in the extension block) per packet. The RTP seq and PictureID
/// start random (RFC 3550 5.1), seeded by the sender.
pub(crate) struct RtpSendState {
    ssrc: u32,
    pt: u8,
    seq: u16,
    picture_id: u16,
    tcc_seq: u16,
}

impl RtpSendState {
    pub(crate) fn new(ssrc: u32, pt: u8, seq: u16, picture_id: u16) -> Self {
        Self { ssrc, pt, seq, picture_id: picture_id & 0x7fff, tcc_seq: 0 }
    }

    /// The sequence number of the NEXT packet (for the sender to seed the SRTP context / for tests).
    pub(crate) fn next_seq(&self) -> u16 {
        self.seq
    }

    /// Packetize one encoded VP8 `frame` at 90 kHz `ts`, fragmenting so each packet plus its `srtp_tag_len`
    /// stays within the MTU. `abs_send_time` present => each packet carries the RFC 8285 extension block
    /// (X=1) with that 24-bit abs-send-time and a per-packet transport-wide-cc sequence; absent => X=0.
    /// Every packet of the frame carries the SAME `ts`; the marker bit is set on the LAST packet only;
    /// `seq` increments per packet; the PictureID bumps once for the whole frame. An empty frame yields
    /// no packets. When the block is present each packet's VP8 descriptor (and SRTP-encrypted payload)
    /// starts at [`HEADER_WITH_EXT`], else at 12.
    pub(crate) fn packetize(&mut self, frame: &[u8], ts: u32, srtp_tag_len: usize, abs_send_time: Option<u32>) -> Vec<Vec<u8>> {
        if frame.is_empty() {
            return Vec::new();
        }
        let header = if abs_send_time.is_some() { HEADER_WITH_EXT } else { RTP_HEADER };
        let budget = MAX_RTP_PACKET.saturating_sub(header + VP8_DESC + srtp_tag_len).max(1);
        let pid = self.picture_id & 0x7fff;
        let chunks: Vec<&[u8]> = frame.chunks(budget).collect();
        let n = chunks.len();
        let mut out = Vec::with_capacity(n);
        for (i, chunk) in chunks.iter().enumerate() {
            let last = i == n - 1;
            let mut pkt = Vec::with_capacity(header + VP8_DESC + chunk.len());
            // RFC 3550 fixed header: V=2, P=0, CC=0; X=1 when the extension block is present; M on the
            // last packet; PT in bits 6..0.
            pkt.push(if abs_send_time.is_some() { 0x90 } else { 0x80 });
            pkt.push((self.pt & 0x7f) | if last { 0x80 } else { 0 });
            pkt.extend_from_slice(&self.seq.to_be_bytes());
            pkt.extend_from_slice(&ts.to_be_bytes());
            pkt.extend_from_slice(&self.ssrc.to_be_bytes());
            if let Some(ast) = abs_send_time {
                // RFC 8285 one-byte header extension, profile 0xBEDE, length = 2 words (8 bytes).
                pkt.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x02]);
                // abs-send-time: id 2, len 3 (24-bit 6.18 NTP seconds).
                pkt.push((2 << 4) | (3 - 1));
                pkt.extend_from_slice(&[(ast >> 16) as u8, (ast >> 8) as u8, ast as u8]);
                // transport-wide-cc: id 3, len 2 (a connection-wide 16-bit sequence).
                pkt.push((3 << 4) | (2 - 1));
                pkt.extend_from_slice(&self.tcc_seq.to_be_bytes());
                self.tcc_seq = self.tcc_seq.wrapping_add(1);
                pkt.push(0x00); // pad 7 bytes of elements to the 8-byte (2-word) boundary
            }
            // RFC 7741 VP8 descriptor: X=1 (0x80), S=1 (0x10) only on the first packet of the frame, PID=0
            // (one token partition); then I=1 (0x80) with the 15-bit PictureID (M=1).
            pkt.push(0x80 | if i == 0 { 0x10 } else { 0 });
            pkt.push(0x80);
            pkt.push(0x80 | ((pid >> 8) & 0x7f) as u8);
            pkt.push((pid & 0xff) as u8);
            pkt.extend_from_slice(chunk);
            out.push(pkt);
            self.seq = self.seq.wrapping_add(1);
        }
        self.picture_id = (pid + 1) & 0x7fff;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq_of(pkt: &[u8]) -> u16 {
        u16::from_be_bytes([pkt[2], pkt[3]])
    }
    fn ts_of(pkt: &[u8]) -> u32 {
        u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]])
    }

    #[test]
    fn one_packet_header_and_descriptor() {
        let mut st = RtpSendState::new(0xCAFE_BABE, 96, 1000, 0x1234);
        let pkts = st.packetize(&[0xAA; 20], 90_000, 10, None);
        assert_eq!(pkts.len(), 1);
        let p = &pkts[0];
        assert_eq!(p[0], 0x80, "V=2, no X/P/CC");
        assert_eq!(p[1], 0x80 | 96, "marker set (last) + PT 96");
        assert_eq!(seq_of(p), 1000);
        assert_eq!(ts_of(p), 90_000);
        assert_eq!(&p[8..12], &0xCAFE_BABEu32.to_be_bytes(), "SSRC");
        // VP8 descriptor: X=1,S=1 ; I=1 ; M + PictureID 0x1234
        assert_eq!(p[12], 0x80 | 0x10);
        assert_eq!(p[13], 0x80);
        assert_eq!(p[14], 0x80 | 0x12);
        assert_eq!(p[15], 0x34);
        assert_eq!(&p[16..], &[0xAA; 20], "payload after the 4-byte descriptor");
        // the PictureID advanced once for the frame
        assert_eq!(st.packetize(&[0], 90_000, 10, None)[0][14] & 0x7f, 0x12);
        assert_eq!(st.packetize(&[0], 90_000, 10, None)[0][15], 0x36, "PictureID +1 per frame");
    }

    #[test]
    fn fragments_a_large_frame_with_one_timestamp_and_marker_on_the_last() {
        let mut st = RtpSendState::new(1, 96, 0, 0);
        let frame = vec![0x55u8; 4000]; // > budget -> several packets
        let pkts = st.packetize(&frame, 7777, 10, None);
        assert!(pkts.len() >= 4, "4000 bytes fragments across packets");
        // all but the last have no marker; the last does
        for (i, p) in pkts.iter().enumerate() {
            let last = i == pkts.len() - 1;
            assert_eq!(p[1] & 0x80 != 0, last, "marker only on the last packet");
            assert_eq!(ts_of(p), 7777, "every packet of the frame shares the timestamp");
            assert_eq!(seq_of(p), i as u16, "sequence numbers are consecutive");
            // S (start of partition) only on the first packet
            assert_eq!(p[12] & 0x10 != 0, i == 0, "S bit only on the first packet");
            assert!(p.len() <= MAX_RTP_PACKET - 10, "a packet + its SRTP tag fits the MTU");
        }
        // reassembled payload is the original frame
        let body: Vec<u8> = pkts.iter().flat_map(|p| p[16..].iter().copied()).collect();
        assert_eq!(body, frame);
    }

    #[test]
    fn empty_frame_yields_no_packets() {
        let mut st = RtpSendState::new(1, 96, 5, 5);
        assert!(st.packetize(&[], 1, 10, None).is_empty());
        assert_eq!(st.next_seq(), 5, "a dropped frame does not burn a sequence number");
    }

    #[test]
    fn sequence_wraps() {
        let mut st = RtpSendState::new(1, 96, 0xFFFF, 0);
        let a = st.packetize(&[1], 1, 10, None);
        let b = st.packetize(&[2], 2, 10, None);
        assert_eq!(seq_of(&a[0]), 0xFFFF);
        assert_eq!(seq_of(&b[0]), 0x0000, "sequence number wraps");
    }

    #[test]
    fn extension_block_carries_abs_send_time_and_transport_cc() {
        let mut st = RtpSendState::new(0xAABB_CCDD, 96, 0, 0);
        let ast = 0x12_3456u32; // a 24-bit abs-send-time
        let pkts = st.packetize(&[0x11; 10], 90_000, 10, Some(ast));
        assert_eq!(pkts.len(), 1);
        let p = &pkts[0];
        assert_eq!(p[0], 0x90, "V=2, X=1 (extension present)");
        assert_eq!(&p[12..16], &[0xBE, 0xDE, 0x00, 0x02], "0xBEDE one-byte header extension, 2 words");
        assert_eq!(p[16], (2 << 4) | 2, "abs-send-time id 2, len 3");
        assert_eq!(&p[17..20], &[0x12, 0x34, 0x56], "the 24-bit abs-send-time");
        assert_eq!(p[20], (3 << 4) | 1, "transport-wide-cc id 3, len 2");
        assert_eq!(&p[21..23], &[0x00, 0x00], "transport-cc seq 0 on the first packet");
        assert_eq!(p[23], 0x00, "pad to the 8-byte element boundary");
        assert_eq!(p[HEADER_WITH_EXT] & 0x90, 0x90, "the VP8 descriptor begins after header+ext (X|S)");
        assert_eq!(&p[HEADER_WITH_EXT + 4..], &[0x11; 10], "payload after header + ext + VP8 descriptor");
        // the transport-cc sequence advances per packet
        let p2 = st.packetize(&[0x22; 10], 90_000, 10, Some(ast));
        assert_eq!(&p2[0][21..23], &[0x00, 0x01], "transport-cc seq advanced to 1");
    }
}
