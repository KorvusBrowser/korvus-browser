//! KORVUS PATCH (H3): HPACK's Huffman code (RFC 7541 Appendix B) for QPACK strings. The encoder and decoder follow
//! the `http2` crate's (MIT), whose generated tables `huffman_table.rs` carries.

use super::huffman_table::{DECODE_TABLE, ENCODE_TABLE};

const MAYBE_EOS: u8 = 1;
const DECODED: u8 = 2;
const ERROR: u8 = 4;

/// The Huffman-coded length of `src` in bytes (`http2::HuffmanSize`).
pub(crate) fn encoded_len(src: &[u8]) -> usize {
    let bits: usize = src.iter().map(|b| ENCODE_TABLE[*b as usize].0).sum();
    bits.div_ceil(8)
}

pub(crate) fn encode(src: &[u8], dst: &mut Vec<u8>) {
    let mut bits: u64 = 0;
    let mut bits_left = 40;
    for &b in src {
        let (nbits, code) = ENCODE_TABLE[b as usize];
        bits |= code << (bits_left - nbits);
        bits_left -= nbits;
        while bits_left <= 32 {
            dst.push((bits >> 32) as u8);
            bits <<= 8;
            bits_left += 8;
        }
    }
    if bits_left != 40 {
        // The rest of the last byte is the EOS code's prefix: all ones.
        bits |= (1 << bits_left) - 1;
        dst.push((bits >> 32) as u8);
    }
}

pub(crate) fn decode(src: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(src.len() * 2);
    let mut state = 0u8;
    let mut maybe_eos = false;
    for b in src {
        for nibble in [b >> 4, b & 0x0f] {
            let (next, byte, flags) = DECODE_TABLE[state as usize][nibble as usize];
            if flags & ERROR == ERROR {
                return None;
            }
            if flags & DECODED == DECODED {
                out.push(byte);
            }
            state = next;
            maybe_eos = flags & MAYBE_EOS == MAYBE_EOS;
        }
    }
    (state == 0 || maybe_eos).then_some(out)
}
