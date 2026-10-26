//! `rtc_srtp.rs` -- SRTP protection for the WebRTC send path (goal A, Slice 1c), the profile
//! `SRTP_AES128_CM_SHA1_80` (RFC 3711) that DTLS-SRTP negotiates and Chrome uses by default. Built on the
//! `aes`, `hmac` and `sha1` crates korvus already links -- AES-128 Counter Mode for confidentiality and
//! HMAC-SHA1 truncated to 80 bits for authentication -- so it adds NO dependency. The master key and salt
//! come from the DTLS handshake's exported keying material (RFC 5764).
//!
//! Correctness is the whole point here: a single wrong byte and the peer drops every packet. The tests
//! pin the RFC 3711 Appendix B.2 (AES-CM keystream) and B.3 (key derivation) vectors verbatim, and a
//! protect -> unprotect round-trip proves recovery and that a one-bit tamper is caught.
//!
//! Compiled only under the `webrtc-srtp` feature (which `webrtc-media` turns on); the default build has
//! none of it. The sender that calls `protect` lands in the final Slice-1c commit.
#![allow(dead_code)] // protect/new have their live caller (the sender) in the final 1c commit; the KAT exercises them now.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use hmac::{Hmac, Mac};
use sha1::Sha1;

const MASTER_KEY: usize = 16;
const MASTER_SALT: usize = 14;
const SESSION_KEY: usize = 16;
const SESSION_SALT: usize = 14;
const AUTH_KEY: usize = 20;
/// SRTP_AES128_CM_SHA1_80: the auth tag is the first 80 bits of HMAC-SHA1.
const AUTH_TAG: usize = 10;

/// AES-128 Counter Mode keystream XORed into `buf`, from the 128-bit initial `counter`. The block counter
/// is the low 16 bits (RFC 3711 4.1.1): an SRTP packet is far under 2^16 blocks and the IV's low two
/// bytes start at zero, so this never carries past 16 bits. Symmetric: the same call decrypts.
fn aes_cm_xor(key: &[u8; MASTER_KEY], mut counter: [u8; 16], buf: &mut [u8]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for chunk in buf.chunks_mut(16) {
        let mut ks = counter;
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut ks));
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= *k;
        }
        let n = u16::from_be_bytes([counter[14], counter[15]]).wrapping_add(1);
        counter[14..16].copy_from_slice(&n.to_be_bytes());
    }
}

/// RFC 3711 4.3 key derivation (kdr = 0): the session key for `label` (0x00 encryption, 0x01 auth, 0x02
/// salt) is the AES-CM keystream of the master key over the master salt with `label` XORed into octet 7.
fn kdf(master_key: &[u8; MASTER_KEY], master_salt: &[u8; MASTER_SALT], label: u8, out: &mut [u8]) {
    let mut iv = [0u8; 16];
    iv[..MASTER_SALT].copy_from_slice(master_salt);
    iv[7] ^= label;
    out.iter_mut().for_each(|b| *b = 0);
    aes_cm_xor(master_key, iv, out);
}

/// The SRTP sending context for one SSRC: the derived session keys, and the rollover counter that extends
/// the 16-bit RTP sequence number to the 48-bit SRTP index.
pub(crate) struct SrtpSendCtx {
    session_key: [u8; SESSION_KEY],
    session_salt: [u8; SESSION_SALT],
    auth_key: [u8; AUTH_KEY],
    ssrc: u32,
    roc: u32,
    highest_seq: u16,
    seen_first: bool,
}

impl SrtpSendCtx {
    /// Derive the sending context from the 60 bytes DTLS-SRTP exports (RFC 5764 4.2:
    /// client_key(16) | server_key(16) | client_salt(14) | server_salt(14)). korvus uses the CLIENT write
    /// keys when it is the DTLS client (the answer's `a=setup:passive`), the SERVER write keys otherwise.
    pub(crate) fn new(keying: &[u8; 60], is_server: bool, ssrc: u32) -> Self {
        let (mk, ms): (&[u8], &[u8]) = if is_server { (&keying[16..32], &keying[46..60]) } else { (&keying[0..16], &keying[32..46]) };
        let mk: [u8; MASTER_KEY] = mk.try_into().expect("16");
        let ms: [u8; MASTER_SALT] = ms.try_into().expect("14");
        let mut session_key = [0u8; SESSION_KEY];
        let mut auth_key = [0u8; AUTH_KEY];
        let mut session_salt = [0u8; SESSION_SALT];
        kdf(&mk, &ms, 0x00, &mut session_key);
        kdf(&mk, &ms, 0x01, &mut auth_key);
        kdf(&mk, &ms, 0x02, &mut session_salt);
        Self { session_key, session_salt, auth_key, ssrc, roc: 0, highest_seq: 0, seen_first: false }
    }

    /// The SRTP IV for the current index (RFC 3711 4.1.1): `(salt << 16) XOR (SSRC << 64) XOR (index << 16)`.
    fn iv(&self, index: u64) -> [u8; 16] {
        let mut iv = [0u8; 16];
        iv[..SESSION_SALT].copy_from_slice(&self.session_salt);
        let ssrc = self.ssrc.to_be_bytes();
        for i in 0..4 {
            iv[4 + i] ^= ssrc[i];
        }
        let idx = index.to_be_bytes(); // the low 48 bits are idx[2..8]
        for i in 0..6 {
            iv[8 + i] ^= idx[2 + i];
        }
        iv
    }

    /// Advance the rollover counter for a sender emitting monotonically increasing sequence numbers, and
    /// return the 48-bit SRTP index for `seq`.
    fn index_for(&mut self, seq: u16) -> u64 {
        if self.seen_first && seq < self.highest_seq {
            self.roc = self.roc.wrapping_add(1);
        }
        self.highest_seq = seq;
        self.seen_first = true;
        ((self.roc as u64) << 16) | seq as u64
    }

    /// Protect an RTP packet in place: encrypt the payload after `header_len` with AES-CM and append the
    /// 10-byte HMAC-SHA1-80 tag over the packet and the rollover counter.
    pub(crate) fn protect(&mut self, pkt: &mut Vec<u8>, header_len: usize) {
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let index = self.index_for(seq);
        let iv = self.iv(index);
        aes_cm_xor(&self.session_key, iv, &mut pkt[header_len..]);
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&self.auth_key).expect("any key length");
        mac.update(pkt);
        mac.update(&self.roc.to_be_bytes());
        let tag = mac.finalize().into_bytes();
        pkt.extend_from_slice(&tag[..AUTH_TAG]);
    }

    /// Verify the tag and decrypt in place (test-only; the receive path is a later goal). `false` on a tag
    /// mismatch, with the packet left unchanged.
    #[cfg(test)]
    pub(crate) fn unprotect(&mut self, pkt: &mut Vec<u8>, header_len: usize) -> bool {
        if pkt.len() < header_len + AUTH_TAG {
            return false;
        }
        let body = pkt.len() - AUTH_TAG;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let index = self.index_for(seq);
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&self.auth_key).expect("any key length");
        mac.update(&pkt[..body]);
        mac.update(&self.roc.to_be_bytes());
        let tag = mac.finalize().into_bytes();
        if tag[..AUTH_TAG] != pkt[body..] {
            return false;
        }
        let iv = self.iv(index);
        pkt.truncate(body);
        aes_cm_xor(&self.session_key, iv, &mut pkt[header_len..]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexn(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    fn hex16(s: &str) -> [u8; 16] {
        hexn(s).try_into().unwrap()
    }
    fn hex14(s: &str) -> [u8; 14] {
        hexn(s).try_into().unwrap()
    }

    /// RFC 3711 Appendix B.2: AES-CM keystream for a zero plaintext is the three blocks shown.
    #[test]
    fn aes_cm_matches_rfc3711_b2() {
        let key = hex16("2B7E151628AED2A6ABF7158809CF4F3C");
        let iv = hex16("F0F1F2F3F4F5F6F7F8F9FAFBFCFD0000");
        let mut buf = [0u8; 48];
        aes_cm_xor(&key, iv, &mut buf);
        assert_eq!(buf[0..16], hex16("E03EAD0935C95E80E166B16DD92B4EB4"));
        assert_eq!(buf[16..32], hex16("D23513162B02D0F72A43A2FE4A5F97AB"));
        assert_eq!(buf[32..48], hex16("41E95B3BB0A2E8DD477901E4FCA894C0"));
    }

    /// RFC 3711 Appendix B.3: the three session keys derived from the master key/salt (kdr = 0). The auth
    /// key is the first 20 bytes of the shown value (SRTP_AES128_CM_SHA1_80's auth key length).
    #[test]
    fn kdf_matches_rfc3711_b3() {
        let mk = hex16("E1F97A0D3E018BE0D64FA32C06DE4139");
        let ms = hex14("0EC675AD498AFEEBB6960B3AABE6");
        let mut ke = [0u8; 16];
        kdf(&mk, &ms, 0x00, &mut ke);
        assert_eq!(ke, hex16("C61E7A93744F39EE10734AFE3FF7A087"), "session encryption key");
        let mut ks = [0u8; 14];
        kdf(&mk, &ms, 0x02, &mut ks);
        assert_eq!(ks, hex14("30CBBC08863D8C85D49DB34A9AE1"), "session salt");
        let mut ka = [0u8; 20];
        kdf(&mk, &ms, 0x01, &mut ka);
        assert_eq!(ka[..], hexn("CEBE321F6FF7716B6FD4AB49AF256A156D38BAA4")[..], "session auth key");
    }

    /// protect -> unprotect recovers the payload and verifies the tag, and a one-bit flip is caught.
    #[test]
    fn protect_round_trips_and_detects_tamper() {
        let keying = [0x11u8; 60];
        let ssrc = 0xCAFE_BABE;
        let mut tx = SrtpSendCtx::new(&keying, false, ssrc);
        let mut pkt = vec![0x80, 96, 0, 1, 0, 0, 0, 0, 0xCA, 0xFE, 0xBA, 0xBE, 1, 2, 3, 4, 5, 6, 7, 8];
        let plain = pkt[12..].to_vec();
        tx.protect(&mut pkt, 12);
        assert_eq!(pkt.len(), 12 + 8 + AUTH_TAG, "header + encrypted payload + tag");
        assert_ne!(pkt[12..20], plain[..], "payload is encrypted");

        let mut rx = SrtpSendCtx::new(&keying, false, ssrc);
        let mut recv = pkt.clone();
        assert!(rx.unprotect(&mut recv, 12), "tag verifies, decrypts");
        assert_eq!(recv[12..], plain[..], "payload recovered");

        let mut bad = pkt.clone();
        bad[15] ^= 1;
        let mut rx2 = SrtpSendCtx::new(&keying, false, ssrc);
        assert!(!rx2.unprotect(&mut bad, 12), "a one-bit tamper fails the tag");
    }

    /// The client and server write keys are the two halves of the exported block.
    #[test]
    fn role_picks_its_write_keys() {
        let mut keying = [0u8; 60];
        for (i, b) in keying.iter_mut().enumerate() {
            *b = i as u8;
        }
        let client = SrtpSendCtx::new(&keying, false, 1);
        let server = SrtpSendCtx::new(&keying, true, 1);
        assert_ne!(client.session_key, server.session_key, "distinct derived keys per role");
    }
}
