//! KORVUS PATCH (H3): QUICHE's chaos protection of a client's Initial packets, as Chrome 150 runs it
//! (`quic_enable_chaos_protection = true`, `quiche_protocol_flags_list.h:247-249`).
//!
//! Two parts, both ported from `net/third_party/quiche/src/quiche/quic/core/`:
//!  * [`split`] -- `QuicPacketCreator::MultiPacketChaosProtect` (`quic_packet_creator.cc:1609-1720`): a ClientHello
//!    too long for one packet goes out as its head (55 + rand % 32 bytes) and its tail in the first packet, its
//!    middle in the packets after.
//!  * [`protect`] -- `QuicChaosProtector::BuildDataPacket` (`quic_chaos_protector.cc:90-211`): within one Initial
//!    packet that carries CRYPTO data and padding, the CRYPTO frames are split (2..=10 more), PING frames added
//!    (2..=10), the padding spread between the frames, and the frames shuffled -- an ACK frame never moves.

use rand::{Rng, RngExt};

/// The size of the smallest CRYPTO frame carrying `len` bytes at `offset` (`QuicFramer::GetMinCryptoFrameSize`).
fn min_crypto_frame_size(offset: u64, len: u64) -> usize {
    1 + varint_len(offset) + varint_len(len)
}

fn varint_len(v: u64) -> usize {
    if v < 1 << 6 {
        1
    } else if v < 1 << 14 {
        2
    } else if v < 1 << 30 {
        4
    } else {
        8
    }
}

/// Where a ClientHello of `write_length` bytes is cut for the multi-packet chaos protection: `Some((head, tail))`
/// -- the first packet carries `[0, head)` and `[write_length - tail, write_length)`, the rest goes after -- or
/// `None` when it fits one packet (or the arithmetic refuses, as QUICHE's does).
///
/// `bytes_free` is what the first packet has for frames, `max_plaintext` a packet's plaintext capacity and
/// `header_size` its header (QUICHE's `BytesFree()`, `max_plaintext_size_`, `PacketHeaderSize()`).
pub(super) fn split(write_length: usize, bytes_free: usize, max_plaintext: usize, header_size: usize, rng: &mut impl Rng) -> Option<(usize, usize)> {
    let offset = 0u64;
    let min_crypto = min_crypto_frame_size(offset + write_length as u64 - 1, write_length as u64);
    if bytes_free < 2 * min_crypto {
        return None;
    }
    let max_first = bytes_free - 2 * min_crypto;
    if max_plaintext < header_size + min_crypto {
        return None;
    }
    let max_other = max_plaintext - header_size - min_crypto;
    if write_length <= max_first || max_other < max_first {
        return None;
    }
    let first_occupied = max_other - max_first;
    let num_packets = (write_length + first_occupied + max_other - 1) / max_other;
    let data_in_other = (write_length + first_occupied + num_packets - 1) / num_packets;
    if data_in_other < first_occupied {
        return None;
    }
    let data_in_first = data_in_other - first_occupied;
    const MIN_FIRST_FRAME_LENGTH: usize = 55;
    const FIRST_FRAME_LENGTH_RANDOM: u64 = 32;
    let first_frame_length = MIN_FIRST_FRAME_LENGTH + (rng.random::<u64>() % FIRST_FRAME_LENGTH_RANDOM) as usize;
    if write_length <= first_frame_length || data_in_first <= first_frame_length {
        return None;
    }
    Some((first_frame_length, data_in_first - first_frame_length))
}

enum Frame<'a> {
    Crypto { offset: u64, data: &'a [u8] },
    Ping,
    Padding(usize),
    /// An ACK (or any other frame this module leaves alone), copied as it was written.
    Raw { bytes: &'a [u8], ack: bool },
}

fn read_varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let first = *b.get(*at)?;
    let len = 1usize << (first >> 6);
    let bytes = b.get(*at..*at + len)?;
    let mut v = u64::from(first & 0x3f);
    for x in &bytes[1..] {
        v = (v << 8) | u64::from(*x);
    }
    *at += len;
    Some(v)
}

fn write_varint(out: &mut Vec<u8>, v: u64) {
    match varint_len(v) {
        1 => out.push(v as u8),
        2 => out.extend_from_slice(&((v as u16) | 0x4000).to_be_bytes()),
        4 => out.extend_from_slice(&((v as u32) | 0x8000_0000).to_be_bytes()),
        _ => out.extend_from_slice(&(v | 0xc000_0000_0000_0000).to_be_bytes()),
    }
}

/// Read a packet payload as frames; `None` for anything this module does not understand (the packet then goes
/// out as quinn wrote it).
fn parse(payload: &[u8]) -> Option<Vec<Frame<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < payload.len() {
        let start = at;
        let ty = read_varint(payload, &mut at)?;
        match ty {
            0x00 => {
                let mut end = at;
                while end < payload.len() && payload[end] == 0 {
                    end += 1;
                }
                let n = end - start;
                at = end;
                match out.last_mut() {
                    Some(Frame::Padding(m)) => *m += n,
                    _ => out.push(Frame::Padding(n)),
                }
            }
            0x01 => out.push(Frame::Ping),
            0x02 | 0x03 => {
                let _largest = read_varint(payload, &mut at)?;
                let _delay = read_varint(payload, &mut at)?;
                let count = read_varint(payload, &mut at)?;
                let _first = read_varint(payload, &mut at)?;
                for _ in 0..count {
                    read_varint(payload, &mut at)?;
                    read_varint(payload, &mut at)?;
                }
                if ty == 0x03 {
                    for _ in 0..3 {
                        read_varint(payload, &mut at)?;
                    }
                }
                out.push(Frame::Raw { bytes: &payload[start..at], ack: true });
            }
            0x06 => {
                let offset = read_varint(payload, &mut at)?;
                let len = read_varint(payload, &mut at)? as usize;
                let data = payload.get(at..at + len)?;
                at += len;
                out.push(Frame::Crypto { offset, data });
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Rewrite one client Initial packet's plaintext `payload` (frames, then quinn's trailing padding) in place, the
/// same length, as `QuicChaosProtector` lays it out. A packet without CRYPTO data or without padding is left alone,
/// as QUICHE leaves it (`IngestFrames` refuses it).
pub(super) fn protect(payload: &mut [u8], rng: &mut impl Rng) {
    if let Some(out) = rewrite(payload, rng) {
        // The arithmetic keeps the length; should it ever not, the packet goes out as quinn wrote it.
        if out.len() == payload.len() {
            payload.copy_from_slice(&out);
        }
    }
}

fn rewrite(payload: &[u8], rng: &mut impl Rng) -> Option<Vec<u8>> {
    let frames = parse(payload)?;
    // IngestFrames: one padding run (quinn writes it last), at least one CRYPTO frame.
    let mut remaining_padding: isize = 0;
    let mut kept: Vec<Frame<'_>> = Vec::new();
    let mut has_crypto = false;
    let mut paddings = 0;
    for f in frames {
        match f {
            Frame::Padding(n) => {
                paddings += 1;
                remaining_padding += n as isize;
            }
            Frame::Crypto { .. } => {
                has_crypto = true;
                kept.push(f);
            }
            other => kept.push(other),
        }
    }
    if !has_crypto || paddings != 1 || remaining_padding <= 0 {
        return None;
    }
    let mut frames = kept;
    let (lo, hi) = frames.iter().fold((u64::MAX, 0u64), |(lo, hi), f| match f {
        Frame::Crypto { offset, data } => (lo.min(*offset), hi.max(offset + data.len() as u64)),
        _ => (lo, hi),
    });
    let crypto_data_length = hi - lo;

    // SplitCryptoFrame.
    let max_overhead = min_crypto_frame_size(lo + crypto_data_length, crypto_data_length) as isize;
    let added = 2 + rng.random::<u64>() % (10 + 1 - 2);
    for _ in 0..added {
        if remaining_padding < max_overhead {
            break;
        }
        let index = (rng.random::<u64>() % frames.len() as u64) as usize;
        let (offset, data) = match frames[index] {
            Frame::Crypto { offset, data } => (offset, data),
            _ => continue,
        };
        if data.len() <= 1 {
            continue;
        }
        let old_overhead = min_crypto_frame_size(offset, data.len() as u64) as isize;
        let keep = 1 + (rng.random::<u64>() % (data.len() as u64 - 1)) as usize;
        let (head, tail) = data.split_at(keep);
        let new_offset = offset + keep as u64;
        frames[index] = Frame::Crypto { offset, data: head };
        frames.push(Frame::Crypto { offset: new_offset, data: tail });
        remaining_padding -= min_crypto_frame_size(new_offset, tail.len() as u64) as isize;
        remaining_padding -= min_crypto_frame_size(offset, head.len() as u64) as isize;
        remaining_padding += old_overhead;
    }

    // AddPingFrames.
    if remaining_padding > 0 {
        let pings = (2 + rng.random::<u64>() % (10 + 1 - 2)).min(remaining_padding as u64);
        for _ in 0..pings {
            frames.push(Frame::Ping);
        }
        remaining_padding -= pings as isize;
    }

    // SpreadPadding.
    let mut spread: Vec<Frame<'_>> = Vec::with_capacity(frames.len() * 2 + 1);
    for f in frames {
        let n = (rng.random::<u64>() % (remaining_padding.max(0) as u64 + 1)) as isize;
        if n > 0 && !matches!(f, Frame::Raw { ack: true, .. }) {
            spread.push(Frame::Padding(n as usize));
            remaining_padding -= n;
        }
        spread.push(f);
    }
    if remaining_padding > 0 {
        spread.push(Frame::Padding(remaining_padding as usize));
    }

    // ReorderFrames: a Fisher-Yates walk that never moves an ACK.
    for i in (1..spread.len()).rev() {
        let j = (rng.random::<u64>() % (i as u64 + 1)) as usize;
        let ack = |f: &Frame<'_>| matches!(f, Frame::Raw { ack: true, .. });
        if !ack(&spread[i]) && !ack(&spread[j]) {
            spread.swap(i, j);
        }
    }

    let mut out = Vec::with_capacity(payload.len());
    for f in &spread {
        match f {
            Frame::Crypto { offset, data } => {
                out.push(0x06);
                write_varint(&mut out, *offset);
                write_varint(&mut out, data.len() as u64);
                out.extend_from_slice(data);
            }
            Frame::Ping => out.push(0x01),
            Frame::Padding(n) => out.extend(std::iter::repeat_n(0u8, *n)),
            Frame::Raw { bytes, .. } => out.extend_from_slice(bytes),
        }
    }
    Some(out)
}
