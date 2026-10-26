//! KORVUS PATCH (H3): the client's QUIC transport parameters as QUICHE serializes them
//! (`quic/core/crypto/transport_parameters.cc`, `SerializeTransportParameters`).
//!
//! quinn-proto writes its own set in its own (shuffled) order; Chrome's differs in what it sends and in what it
//! adds. The values that govern flow control are the ones quinn enforces (the driver configures quinn from the
//! same [`super::Http3Options`]), so this only changes the spelling, never a limit:
//!
//!  * sent: `max_idle_timeout`, `max_udp_payload_size`, `initial_max_data`, the three stream windows, both stream
//!    limits, an empty `initial_source_connection_id`, `max_datagram_frame_size`, `google_connection_options`,
//!    `version_information` (0x11) with one GREASE version at a random index, one GREASE parameter (id `31N+27`,
//!    0..=15 random bytes, `:865-882`), and `initial_round_trip_time` (0x3127) only when an srtt is cached for the
//!    server (`quic_session_pool.cc:2229-2262`);
//!  * omitted: an integer equal to its RFC default (`:292-300`), `active_connection_id_limit`, `min_ack_delay`,
//!    `disable_active_migration`, `grease_quic_bit` (QUICHE implements none of them for a client);
//!  * order: a Fisher-Yates shuffle of every id, every handshake (`:890-895`).

use super::{Http3Options, crypto::random_u64};

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

pub(crate) fn put_varint(out: &mut Vec<u8>, v: u64) {
    match varint_len(v) {
        1 => out.push(v as u8),
        2 => out.extend_from_slice(&((v as u16) | 0x4000).to_be_bytes()),
        4 => out.extend_from_slice(&((v as u32) | 0x8000_0000).to_be_bytes()),
        _ => out.extend_from_slice(&(v | 0xc000_0000_0000_0000).to_be_bytes()),
    }
}

pub(crate) fn get_varint(b: &[u8], at: &mut usize) -> Option<u64> {
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

/// The id of the parameter a quinn-written set calls `initial_source_connection_id`.
const INITIAL_SOURCE_CONNECTION_ID: u64 = 0x0f;

/// The client's `initial_source_connection_id` out of quinn's own serialization (its value is the connection's
/// source CID, which only quinn knows).
pub(crate) fn initial_source_cid(quinn_params: &[u8]) -> Vec<u8> {
    let mut at = 0;
    while at < quinn_params.len() {
        let (Some(id), Some(len)) = (get_varint(quinn_params, &mut at), get_varint(quinn_params, &mut at)) else {
            break;
        };
        let end = at + len as usize;
        if id == INITIAL_SOURCE_CONNECTION_ID {
            return quinn_params.get(at..end).map(<[u8]>::to_vec).unwrap_or_default();
        }
        at = end;
    }
    Vec::new()
}

/// A GREASE version label, `0x?a?a?a?a` (`CreateRandomVersionLabelForNegotiation`, `quic_versions.cc:29-39`).
fn grease_version() -> u32 {
    (super::crypto::random_u32() & 0xf0f0_f0f0) | 0x0a0a_0a0a
}

/// Chrome's transport parameters for a connection whose source CID is `scid`; `initial_rtt_us` is the server's
/// cached smoothed RTT, if any.
pub(crate) fn encode(opts: &Http3Options, scid: &[u8], initial_rtt_us: Option<u64>) -> Vec<u8> {
    // An integer parameter is left out when it equals its RFC default (`:292-300`).
    fn int(id: u64, v: u64, default: u64, params: &mut Vec<(u64, Vec<u8>)>) {
        if v != default {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            params.push((id, b));
        }
    }
    let mut params: Vec<(u64, Vec<u8>)> = Vec::new();
    int(0x01, opts.max_idle_timeout_ms, 0, &mut params);
    int(0x03, opts.max_udp_payload_size, 65527, &mut params);
    int(0x04, opts.initial_max_data, 0, &mut params);
    int(0x05, opts.initial_max_stream_data, 0, &mut params);
    int(0x06, opts.initial_max_stream_data, 0, &mut params);
    int(0x07, opts.initial_max_stream_data, 0, &mut params);
    int(0x08, opts.initial_max_streams_bidi, 0, &mut params);
    int(0x09, opts.initial_max_streams_uni, 0, &mut params);
    params.push((INITIAL_SOURCE_CONNECTION_ID, scid.to_vec()));
    if let Some(v) = opts.max_datagram_frame_size {
        int(0x20, v, 0, &mut params);
    }
    if let Some(rtt) = initial_rtt_us.filter(|_| opts.send_initial_rtt) {
        int(0x3127, rtt, 0, &mut params);
    }
    if !opts.connection_options.is_empty() {
        params.push((0x3128, opts.connection_options.iter().flat_map(|o| o.iter().copied()).collect()));
    }
    if opts.version_information {
        // Chosen version, then the other versions with one GREASE label at a random index (`:1240-1270`).
        let mut others = vec![super::crypto::QUIC_V1];
        let at = (random_u64() % (others.len() as u64 + 1)) as usize;
        others.insert(at, grease_version());
        let mut b = super::crypto::QUIC_V1.to_be_bytes().to_vec();
        for v in others {
            b.extend_from_slice(&v.to_be_bytes());
        }
        params.push((0x11, b));
    }
    if opts.grease_transport_parameter {
        let id = (random_u64() % ((1u64 << 62) - 31)) / 31 * 31 + 27;
        let len = (random_u64() % 16) as usize;
        let mut b = vec![0u8; len];
        super::crypto::random_bytes(&mut b);
        params.push((id, b));
    }
    if opts.shuffle_transport_parameters {
        for i in (1..params.len()).rev() {
            let j = (random_u64() % (i as u64 + 1)) as usize;
            params.swap(i, j);
        }
    }
    let mut out = Vec::new();
    for (id, v) in params {
        put_varint(&mut out, id);
        put_varint(&mut out, v.len() as u64);
        out.extend_from_slice(&v);
    }
    out
}
