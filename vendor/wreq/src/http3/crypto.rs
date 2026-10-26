//! KORVUS PATCH (H3): QUIC's TLS on the SAME BoringSSL the TCP path speaks through, behind quinn-proto's crypto
//! traits.
//!
//! Chrome's QUIC handshake is BoringSSL's QUIC API driven by QUICHE (`TlsClientHandshaker`): BoringSSL writes the
//! ClientHello, QUICHE only carries its bytes. Driving the very library the TCP connector configures (the
//! vendored `btls`, same rev) through `SSL_set_quic_method` is what keeps the ClientHello the persona's: the
//! extension permutation, ALPS, the ML-KEM key share, ECH GREASE and certificate compression are BoringSSL's own
//! code paths, configured through the same `TlsConnector` (`crate::tls::conn`). Nothing here composes a
//! ClientHello.
//!
//! Packet protection (RFC 9001 5) is BoringSSL's too: `EVP_AEAD` for the payload, AES-ECB / ChaCha20 for the header
//! mask, `HKDF` for the labels. The server half exists for the loopback fixture that measures the client.

use std::{
    any::Any,
    os::raw::{c_int, c_uint},
    sync::{Arc, OnceLock},
};

use btls::ssl::Ssl;
use btls_sys as ffi;
use bytes::BytesMut;
use foreign_types::ForeignType;
use quinn_proto::{
    ConnectError, ConnectionId, Side, TransportError, TransportErrorCode,
    crypto::{
        self, CryptoError, ExportKeyingMaterialError, HeaderKey, KeyPair, Keys, PacketKey, UnsupportedVersion,
    },
    transport_parameters::TransportParameters,
};

/// QUIC version 1 (RFC 9000), the only version Chrome 150 offers (`net/quic/quic_context.h:22-29`).
pub(crate) const QUIC_V1: u32 = 0x0000_0001;

const INITIAL_SALT_V1: [u8; 20] = [
    0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8, 0x0c, 0xad, 0xcc, 0xbb, 0x7f,
    0x0a,
];
const RETRY_KEY_V1: [u8; 16] = [
    0xbe, 0x0c, 0x69, 0x0b, 0x9f, 0x66, 0x57, 0x5a, 0x1d, 0x76, 0x6b, 0x54, 0xe3, 0x68, 0xc8, 0x4e,
];
const RETRY_NONCE_V1: [u8; 12] = [0x46, 0x15, 0x99, 0xd3, 0x5d, 0x63, 0x2b, 0xf2, 0x23, 0x98, 0x25, 0xbb];

const LEVEL_INITIAL: usize = 0;
const LEVEL_EARLY: usize = 1;
const LEVEL_HANDSHAKE: usize = 2;
const LEVEL_APPLICATION: usize = 3;

/// Random bytes from BoringSSL's generator.
pub(crate) fn random_bytes(out: &mut [u8]) {
    // SAFETY: `out` is a valid writable buffer of the length passed.
    unsafe {
        ffi::RAND_bytes(out.as_mut_ptr(), out.len());
    }
}

pub(crate) fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    random_bytes(&mut b);
    u64::from_le_bytes(b)
}

pub(crate) fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    random_bytes(&mut b);
    u32::from_le_bytes(b)
}

// ===== cipher suites =====

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Suite {
    Aes128Gcm,
    Aes256Gcm,
    Chacha20Poly1305,
}

impl Suite {
    fn from_protocol_id(id: u16) -> Option<Suite> {
        match id {
            0x1301 => Some(Suite::Aes128Gcm),
            0x1302 => Some(Suite::Aes256Gcm),
            0x1303 => Some(Suite::Chacha20Poly1305),
            _ => None,
        }
    }

    fn md(self) -> *const ffi::EVP_MD {
        // SAFETY: both return static descriptors.
        unsafe {
            match self {
                Suite::Aes256Gcm => ffi::EVP_sha384(),
                _ => ffi::EVP_sha256(),
            }
        }
    }

    fn aead(self) -> *const ffi::EVP_AEAD {
        // SAFETY: all three return static descriptors.
        unsafe {
            match self {
                Suite::Aes128Gcm => ffi::EVP_aead_aes_128_gcm(),
                Suite::Aes256Gcm => ffi::EVP_aead_aes_256_gcm(),
                Suite::Chacha20Poly1305 => ffi::EVP_aead_chacha20_poly1305(),
            }
        }
    }

    fn key_len(self) -> usize {
        match self {
            Suite::Aes128Gcm => 16,
            _ => 32,
        }
    }

    /// RFC 9001 6.6.
    fn confidentiality_limit(self) -> u64 {
        match self {
            Suite::Chacha20Poly1305 => u64::MAX,
            _ => 1 << 23,
        }
    }

    fn integrity_limit(self) -> u64 {
        match self {
            Suite::Chacha20Poly1305 => 1 << 36,
            _ => 1 << 52,
        }
    }
}

/// HKDF-Expand-Label (RFC 8446 7.1) with an empty context.
fn expand_label(md: *const ffi::EVP_MD, secret: &[u8], label: &[u8], len: usize) -> Vec<u8> {
    let mut info = Vec::with_capacity(4 + 6 + label.len());
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label);
    info.push(0);
    let mut out = vec![0u8; len];
    // SAFETY: every pointer is valid for the length given with it.
    unsafe {
        ffi::HKDF_expand(out.as_mut_ptr(), len, md, secret.as_ptr(), secret.len(), info.as_ptr(), info.len());
    }
    out
}

// ===== packet protection =====

struct Aead {
    ctx: *mut ffi::EVP_AEAD_CTX,
    iv: [u8; 12],
    suite: Suite,
}

// SAFETY: an EVP_AEAD_CTX is only read by seal/open, which BoringSSL documents as safe to call concurrently on
// one context.
unsafe impl Send for Aead {}
unsafe impl Sync for Aead {}

impl Aead {
    fn new(suite: Suite, key: &[u8], iv: &[u8]) -> Aead {
        // SAFETY: `key` is `suite.key_len()` bytes; the tag length 0 asks for the AEAD's default (16).
        let ctx = unsafe { ffi::EVP_AEAD_CTX_new(suite.aead(), key.as_ptr(), key.len(), 0) };
        assert!(!ctx.is_null(), "EVP_AEAD_CTX_new");
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&iv[..12]);
        Aead { ctx, iv: nonce, suite }
    }

    fn from_secret(suite: Suite, secret: &[u8]) -> Aead {
        let key = expand_label(suite.md(), secret, b"quic key", suite.key_len());
        let iv = expand_label(suite.md(), secret, b"quic iv", 12);
        Aead::new(suite, &key, &iv)
    }

    fn nonce(&self, packet: u64) -> [u8; 12] {
        let mut n = self.iv;
        for (i, b) in packet.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        n
    }
}

impl Drop for Aead {
    fn drop(&mut self) {
        // SAFETY: allocated by EVP_AEAD_CTX_new and freed once.
        unsafe { ffi::EVP_AEAD_CTX_free(self.ctx) }
    }
}

impl PacketKey for Aead {
    fn encrypt(&self, packet: u64, buf: &mut [u8], header_len: usize) {
        let (header, rest) = buf.split_at_mut(header_len);
        let plain_len = rest.len() - 16;
        let nonce = self.nonce(packet);
        let mut out_len = 0usize;
        // SAFETY: BoringSSL allows `out` and `in` to alias exactly; `rest` has room for the tag.
        let ok = unsafe {
            ffi::EVP_AEAD_CTX_seal(
                self.ctx,
                rest.as_mut_ptr(),
                &mut out_len,
                rest.len(),
                nonce.as_ptr(),
                nonce.len(),
                rest.as_ptr(),
                plain_len,
                header.as_ptr(),
                header.len(),
            )
        };
        assert!(ok == 1 && out_len == rest.len(), "AEAD seal");
    }

    fn decrypt(&self, packet: u64, header: &[u8], payload: &mut BytesMut) -> Result<(), CryptoError> {
        if payload.len() < 16 {
            return Err(CryptoError);
        }
        let nonce = self.nonce(packet);
        let mut out_len = 0usize;
        let len = payload.len();
        // SAFETY: in-place open; `out` aliases `in` exactly.
        let ok = unsafe {
            ffi::EVP_AEAD_CTX_open(
                self.ctx,
                payload.as_mut_ptr(),
                &mut out_len,
                len,
                nonce.as_ptr(),
                nonce.len(),
                payload.as_ptr(),
                len,
                header.as_ptr(),
                header.len(),
            )
        };
        if ok != 1 {
            return Err(CryptoError);
        }
        payload.truncate(out_len);
        Ok(())
    }

    fn tag_len(&self) -> usize {
        16
    }

    fn confidentiality_limit(&self) -> u64 {
        self.suite.confidentiality_limit()
    }

    fn integrity_limit(&self) -> u64 {
        self.suite.integrity_limit()
    }
}

enum Mask {
    Aes(Box<ffi::AES_KEY>),
    Chacha([u8; 32]),
}

// SAFETY: the AES key schedule is read-only after setup.
unsafe impl Send for Mask {}
unsafe impl Sync for Mask {}

impl Mask {
    fn new(suite: Suite, secret: &[u8]) -> Mask {
        let hp = expand_label(suite.md(), secret, b"quic hp", suite.key_len());
        Mask::from_key(suite, &hp)
    }

    fn from_key(suite: Suite, hp: &[u8]) -> Mask {
        match suite {
            Suite::Chacha20Poly1305 => {
                let mut k = [0u8; 32];
                k.copy_from_slice(hp);
                Mask::Chacha(k)
            }
            _ => {
                let mut key: Box<ffi::AES_KEY> = Box::new(ffi::AES_KEY { rd_key: [0; 60], rounds: 0 });
                // SAFETY: `hp` is 16 or 32 bytes, the bit length passed.
                unsafe { ffi::AES_set_encrypt_key(hp.as_ptr(), (hp.len() * 8) as c_uint, &mut *key) };
                Mask::Aes(key)
            }
        }
    }

    fn mask(&self, sample: &[u8]) -> [u8; 5] {
        let mut out = [0u8; 16];
        match self {
            // SAFETY: one 16-byte block in, one out.
            Mask::Aes(key) => unsafe { ffi::AES_ecb_encrypt(sample.as_ptr(), out.as_mut_ptr(), &**key, 1) },
            Mask::Chacha(key) => {
                let counter = u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                let zeros = [0u8; 5];
                // SAFETY: 5 bytes of keystream into `out`; the nonce is the sample's last 12 bytes.
                unsafe {
                    ffi::CRYPTO_chacha_20(out.as_mut_ptr(), zeros.as_ptr(), 5, key.as_ptr(), sample[4..16].as_ptr(), counter)
                };
            }
        }
        [out[0], out[1], out[2], out[3], out[4]]
    }
}

impl HeaderKey for Mask {
    fn decrypt(&self, pn_offset: usize, packet: &mut [u8]) {
        let mask = self.mask(&packet[pn_offset + 4..pn_offset + 20]);
        let bits = if packet[0] & 0x80 != 0 { 0x0f } else { 0x1f };
        packet[0] ^= mask[0] & bits;
        let pn_len = usize::from(packet[0] & 0x03) + 1;
        for i in 0..pn_len {
            packet[pn_offset + i] ^= mask[1 + i];
        }
    }

    fn encrypt(&self, pn_offset: usize, packet: &mut [u8]) {
        let mask = self.mask(&packet[pn_offset + 4..pn_offset + 20]);
        let bits = if packet[0] & 0x80 != 0 { 0x0f } else { 0x1f };
        let pn_len = usize::from(packet[0] & 0x03) + 1;
        for i in 0..pn_len {
            packet[pn_offset + i] ^= mask[1 + i];
        }
        packet[0] ^= mask[0] & bits;
    }

    fn sample_size(&self) -> usize {
        16
    }
}

fn keys_from(suite: Suite, local: &[u8], remote: &[u8]) -> Keys {
    Keys {
        header: KeyPair {
            local: Box::new(Mask::new(suite, local)),
            remote: Box::new(Mask::new(suite, remote)),
        },
        packet: KeyPair {
            local: Box::new(Aead::from_secret(suite, local)),
            remote: Box::new(Aead::from_secret(suite, remote)),
        },
    }
}

/// The Initial keys of a connection whose client chose `dst_cid` (RFC 9001 5.2), as `side` holds them.
pub(crate) fn initial_keys(dst_cid: &[u8], side: Side) -> Keys {
    let suite = Suite::Aes128Gcm;
    // SAFETY: static descriptor.
    let md = unsafe { ffi::EVP_sha256() };
    let mut prk = [0u8; 32];
    let mut prk_len = 0usize;
    // SAFETY: every pointer is valid for the length given with it.
    unsafe {
        ffi::HKDF_extract(prk.as_mut_ptr(), &mut prk_len, md, dst_cid.as_ptr(), dst_cid.len(), INITIAL_SALT_V1.as_ptr(), INITIAL_SALT_V1.len());
    }
    let client = expand_label(md, &prk[..prk_len], b"client in", 32);
    let server = expand_label(md, &prk[..prk_len], b"server in", 32);
    match side {
        Side::Client => keys_from(suite, &client, &server),
        Side::Server => keys_from(suite, &server, &client),
    }
}

/// The client's Initial packet protection, for a fixture that reads the first flight off the wire.
#[doc(hidden)]
pub fn client_initial_open(dst_cid: &[u8]) -> (Box<dyn HeaderKey>, Box<dyn PacketKey>) {
    let Keys { header, packet } = initial_keys(dst_cid, Side::Server);
    (header.remote, packet.remote)
}

fn retry_tag(orig_dst_cid: &[u8], packet: &[u8]) -> [u8; 16] {
    let mut pseudo = Vec::with_capacity(1 + orig_dst_cid.len() + packet.len());
    pseudo.push(orig_dst_cid.len() as u8);
    pseudo.extend_from_slice(orig_dst_cid);
    pseudo.extend_from_slice(packet);
    let aead = Aead::new(Suite::Aes128Gcm, &RETRY_KEY_V1, &RETRY_NONCE_V1);
    let mut tag = [0u8; 16];
    let mut out_len = 0usize;
    // SAFETY: an empty plaintext sealed into a 16-byte tag buffer, the pseudo-packet as additional data.
    unsafe {
        ffi::EVP_AEAD_CTX_seal(
            aead.ctx,
            tag.as_mut_ptr(),
            &mut out_len,
            tag.len(),
            RETRY_NONCE_V1.as_ptr(),
            RETRY_NONCE_V1.len(),
            std::ptr::null(),
            0,
            pseudo.as_ptr(),
            pseudo.len(),
        );
    }
    tag
}

// ===== the BoringSSL QUIC method =====

#[derive(Default)]
struct QuicState {
    /// Handshake bytes BoringSSL wrote, in order, each at its encryption level.
    out: Vec<(usize, Vec<u8>)>,
    write: [Option<(Suite, Vec<u8>)>; 4],
    read: [Option<(Suite, Vec<u8>)>; 4],
    alert: Option<u8>,
}

fn ex_index() -> c_int {
    static INDEX: OnceLock<c_int> = OnceLock::new();
    // SAFETY: registers a plain pointer slot with no callbacks.
    *INDEX.get_or_init(|| unsafe { ffi::SSL_get_ex_new_index(0, std::ptr::null_mut(), std::ptr::null_mut(), None, None) })
}

/// SAFETY: `ssl` carries a `QuicState` pointer at `ex_index()` for as long as BoringSSL may call back (set before
/// the first `SSL_do_handshake`, the state outlives the `SSL`).
unsafe fn state<'a>(ssl: *mut ffi::SSL) -> Option<&'a mut QuicState> {
    unsafe { (ffi::SSL_get_ex_data(ssl, ex_index()) as *mut QuicState).as_mut() }
}

unsafe extern "C" fn cb_set_read_secret(
    ssl: *mut ffi::SSL,
    level: ffi::ssl_encryption_level_t,
    cipher: *const ffi::SSL_CIPHER,
    secret: *const u8,
    len: usize,
) -> c_int {
    unsafe {
        let Some(st) = state(ssl) else { return 0 };
        let Some(suite) = Suite::from_protocol_id(ffi::SSL_CIPHER_get_protocol_id(cipher)) else { return 0 };
        st.read[(level.0 as usize).min(3)] = Some((suite, std::slice::from_raw_parts(secret, len).to_vec()));
        1
    }
}

unsafe extern "C" fn cb_set_write_secret(
    ssl: *mut ffi::SSL,
    level: ffi::ssl_encryption_level_t,
    cipher: *const ffi::SSL_CIPHER,
    secret: *const u8,
    len: usize,
) -> c_int {
    unsafe {
        let Some(st) = state(ssl) else { return 0 };
        let Some(suite) = Suite::from_protocol_id(ffi::SSL_CIPHER_get_protocol_id(cipher)) else { return 0 };
        st.write[(level.0 as usize).min(3)] = Some((suite, std::slice::from_raw_parts(secret, len).to_vec()));
        1
    }
}

unsafe extern "C" fn cb_add_handshake_data(
    ssl: *mut ffi::SSL,
    level: ffi::ssl_encryption_level_t,
    data: *const u8,
    len: usize,
) -> c_int {
    unsafe {
        let Some(st) = state(ssl) else { return 0 };
        let level = level.0 as usize;
        let bytes = std::slice::from_raw_parts(data, len);
        match st.out.last_mut() {
            Some((l, buf)) if *l == level => buf.extend_from_slice(bytes),
            _ => st.out.push((level, bytes.to_vec())),
        }
        1
    }
}

unsafe extern "C" fn cb_flush_flight(_ssl: *mut ffi::SSL) -> c_int {
    1
}

unsafe extern "C" fn cb_send_alert(ssl: *mut ffi::SSL, _level: ffi::ssl_encryption_level_t, alert: u8) -> c_int {
    unsafe {
        if let Some(st) = state(ssl) {
            st.alert = Some(alert);
        }
    }
    1
}

static QUIC_METHOD: ffi::SSL_QUIC_METHOD = ffi::ssl_quic_method_st {
    set_read_secret: Some(cb_set_read_secret),
    set_write_secret: Some(cb_set_write_secret),
    add_handshake_data: Some(cb_add_handshake_data),
    flush_flight: Some(cb_flush_flight),
    send_alert: Some(cb_send_alert),
};

/// What the handshake settled, for whoever asks the session (`Connection::crypto_session().handshake_data()`).
#[derive(Debug, Clone)]
pub struct HandshakeData {
    /// The ALPN protocol the server selected.
    pub protocol: Option<Vec<u8>>,
}

/// One QUIC connection's TLS: an `SSL` in QUIC mode and what its callbacks collected.
pub(crate) struct Session {
    // Field order is drop order: the `SSL` goes before the state its callbacks point into.
    ssl: Ssl,
    state: Box<QuicState>,
    side: Side,
    /// The level whose handshake bytes `write_handshake` hands out (quinn's `highest_space`).
    write_level: usize,
    handshaking: bool,
    got_handshake_data: bool,
    /// The 1-RTT secrets, kept for key updates (RFC 9001 6: "quic ku").
    app_secrets: Option<(Suite, Vec<u8>, Vec<u8>)>,
}

// SAFETY: the `SSL` and the state are only touched through `&mut self` (quinn-proto drives one connection from one
// task at a time).
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    /// An `SSL` the connector configured (SNI, ALPN `h3`, ALPS, ECH, verification, the session cache) turned into a
    /// QUIC endpoint sending `transport_params`.
    pub(crate) fn new(ssl: Ssl, side: Side, transport_params: &[u8]) -> Result<Session, ConnectError> {
        let mut state = Box::<QuicState>::default();
        let raw = ssl.as_ptr();
        // SAFETY: `raw` is the live `SSL` this session owns; the state pointer outlives it (see field order).
        unsafe {
            ffi::SSL_set_ex_data(raw, ex_index(), &mut *state as *mut QuicState as *mut _);
            if ffi::SSL_set_quic_method(raw, &QUIC_METHOD) != 1 {
                return Err(ConnectError::EndpointStopping);
            }
            // RFC 9001's codepoint 0x39, not the draft's 0xffa5 (`tls_client_handshaker.cc:107-112`).
            ffi::SSL_set_quic_use_legacy_codepoint(raw, 0);
            if ffi::SSL_set_quic_transport_params(raw, transport_params.as_ptr(), transport_params.len()) != 1 {
                return Err(ConnectError::EndpointStopping);
            }
            match side {
                Side::Client => ffi::SSL_set_connect_state(raw),
                Side::Server => ffi::SSL_set_accept_state(raw),
            }
        }
        let mut s = Session {
            ssl,
            state,
            side,
            write_level: LEVEL_INITIAL,
            handshaking: true,
            got_handshake_data: false,
            app_secrets: None,
        };
        if side == Side::Client {
            // The ClientHello is written now, as QUICHE's `CryptoConnect` writes it.
            s.advance().map_err(|_| ConnectError::EndpointStopping)?;
        }
        Ok(s)
    }

    fn raw(&self) -> *mut ffi::SSL {
        self.ssl.as_ptr()
    }

    fn error(&self, what: &str) -> TransportError {
        match self.state.alert {
            Some(alert) => TransportError {
                code: TransportErrorCode::crypto(alert),
                frame: None,
                reason: format!("TLS alert {alert} in {what}"),
            },
            None => TransportError::PROTOCOL_VIOLATION(format!("TLS failure in {what}")),
        }
    }

    /// Run the handshake as far as the data provided allows.
    fn advance(&mut self) -> Result<(), TransportError> {
        if !self.handshaking {
            // SAFETY: the live `SSL`.
            if unsafe { ffi::SSL_process_quic_post_handshake(self.raw()) } != 1 {
                return Err(self.error("post-handshake"));
            }
            return Ok(());
        }
        // SAFETY: the live `SSL`.
        let rc = unsafe { ffi::SSL_do_handshake(self.raw()) };
        if rc == 1 {
            self.handshaking = false;
            return Ok(());
        }
        // SAFETY: the live `SSL` and the return code of the call above.
        match unsafe { ffi::SSL_get_error(self.raw(), rc) } {
            ffi::SSL_ERROR_WANT_READ | ffi::SSL_ERROR_WANT_WRITE => Ok(()),
            _ => Err(self.error("handshake")),
        }
    }

    fn alpn(&self) -> Option<Vec<u8>> {
        let mut p: *const u8 = std::ptr::null();
        let mut len: c_uint = 0;
        // SAFETY: BoringSSL hands back a pointer into the `SSL` valid while it lives.
        unsafe {
            ffi::SSL_get0_alpn_selected(self.raw(), &mut p, &mut len);
            (!p.is_null() && len > 0).then(|| std::slice::from_raw_parts(p, len as usize).to_vec())
        }
    }

    fn keys_for(&self, level: usize) -> Option<(Suite, Vec<u8>, Vec<u8>)> {
        let (suite, local) = self.state.write[level].clone()?;
        let (_, remote) = self.state.read[level].clone()?;
        Some((suite, local, remote))
    }
}

impl crypto::Session for Session {
    fn initial_keys(&self, dst_cid: &ConnectionId, side: Side) -> Keys {
        initial_keys(dst_cid, side)
    }

    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.got_handshake_data.then(|| Box::new(HandshakeData { protocol: self.alpn() }) as Box<dyn Any>)
    }

    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        None
    }

    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)> {
        // 0-RTT is not offered (see the module note in `mod.rs`).
        let _ = LEVEL_EARLY;
        None
    }

    fn early_data_accepted(&self) -> Option<bool> {
        match self.side {
            Side::Client => Some(false),
            Side::Server => None,
        }
    }

    fn is_handshaking(&self) -> bool {
        self.handshaking
    }

    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
        // SAFETY: the live `SSL`; `buf` is valid for its length.
        unsafe {
            let level = ffi::SSL_quic_read_level(self.raw());
            if ffi::SSL_provide_quic_data(self.raw(), level, buf.as_ptr(), buf.len()) != 1 {
                return Err(self.error("provide_quic_data"));
            }
        }
        self.advance()?;
        if !self.got_handshake_data && (self.alpn().is_some() || !self.handshaking) {
            self.got_handshake_data = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
        let mut p: *const u8 = std::ptr::null();
        let mut len = 0usize;
        // SAFETY: BoringSSL hands back a pointer into the `SSL` valid while it lives.
        let bytes = unsafe {
            ffi::SSL_get_peer_quic_transport_params(self.raw(), &mut p, &mut len);
            if p.is_null() || len == 0 {
                return Ok(None);
            }
            std::slice::from_raw_parts(p, len)
        };
        TransportParameters::read(self.side, &mut std::io::Cursor::new(bytes))
            .map(Some)
            .map_err(Into::into)
    }

    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
        // This level's bytes first: quinn sends them in the space it is in before the keys below move it on.
        let level = self.write_level;
        let mut rest = Vec::new();
        for (l, bytes) in std::mem::take(&mut self.state.out) {
            if l == level || (l == LEVEL_EARLY && level == LEVEL_INITIAL) {
                buf.extend_from_slice(&bytes);
            } else {
                rest.push((l, bytes));
            }
        }
        self.state.out = rest;
        let next = match level {
            LEVEL_INITIAL => LEVEL_HANDSHAKE,
            LEVEL_HANDSHAKE => LEVEL_APPLICATION,
            _ => return None,
        };
        let (suite, local, remote) = self.keys_for(next)?;
        // Hand the next level's keys over only once nothing of this level is left to send.
        if self.state.out.iter().any(|(l, _)| *l == level) {
            return None;
        }
        self.write_level = next;
        if next == LEVEL_APPLICATION {
            self.app_secrets = Some((suite, local.clone(), remote.clone()));
        }
        Some(keys_from(suite, &local, &remote))
    }

    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
        let (suite, local, remote) = self.app_secrets.as_mut()?;
        let md = suite.md();
        let len = local.len();
        *local = expand_label(md, local, b"quic ku", len);
        *remote = expand_label(md, remote, b"quic ku", len);
        Some(KeyPair {
            local: Box::new(Aead::from_secret(*suite, local)),
            remote: Box::new(Aead::from_secret(*suite, remote)),
        })
    }

    fn is_valid_retry(&self, orig_dst_cid: &ConnectionId, header: &[u8], payload: &[u8]) -> bool {
        let Some(tag_start) = payload.len().checked_sub(16) else {
            return false;
        };
        let mut packet = Vec::with_capacity(header.len() + tag_start);
        packet.extend_from_slice(header);
        packet.extend_from_slice(&payload[..tag_start]);
        let tag = retry_tag(orig_dst_cid, &packet);
        // SAFETY: two 16-byte buffers.
        unsafe { ffi::CRYPTO_memcmp(tag.as_ptr() as *const _, payload[tag_start..].as_ptr() as *const _, 16) == 0 }
    }

    fn export_keying_material(&self, output: &mut [u8], label: &[u8], context: &[u8]) -> Result<(), ExportKeyingMaterialError> {
        // SAFETY: every pointer is valid for the length given with it.
        let ok = unsafe {
            ffi::SSL_export_keying_material(
                self.raw(),
                output.as_mut_ptr(),
                output.len(),
                label.as_ptr() as *const _,
                label.len(),
                context.as_ptr(),
                context.len(),
                1,
            )
        };
        if ok == 1 { Ok(()) } else { Err(ExportKeyingMaterialError) }
    }
}

/// Makes the client's `Session`s: the connection's `SSL` comes from the HTTP/3 connector, the transport parameters
/// from `tp::encode`.
pub(crate) struct ClientConfig {
    pub(crate) make: Box<dyn Fn(&TransportParameters) -> Result<(Ssl, Vec<u8>), ConnectError> + Send + Sync>,
}

impl crypto::ClientConfig for ClientConfig {
    fn start_session(self: Arc<Self>, version: u32, _server_name: &str, params: &TransportParameters) -> Result<Box<dyn crypto::Session>, ConnectError> {
        if version != QUIC_V1 {
            return Err(ConnectError::UnsupportedVersion);
        }
        let (ssl, tp) = (self.make)(params)?;
        Ok(Box::new(Session::new(ssl, Side::Client, &tp)?))
    }
}

/// HMAC-SHA256 for the endpoint's stateless-reset tokens.
pub(crate) struct HmacKey([u8; 32]);

impl HmacKey {
    pub(crate) fn random() -> HmacKey {
        let mut k = [0u8; 32];
        random_bytes(&mut k);
        HmacKey(k)
    }
}

impl crypto::HmacKey for HmacKey {
    fn sign(&self, data: &[u8], signature_out: &mut [u8]) {
        let mut out = [0u8; 32];
        let mut len: c_uint = 0;
        // SAFETY: every pointer is valid for the length given with it; SHA-256 writes 32 bytes.
        unsafe {
            ffi::HMAC(ffi::EVP_sha256(), self.0.as_ptr() as *const _, 32, data.as_ptr(), data.len(), out.as_mut_ptr(), &mut len);
        }
        let n = signature_out.len().min(32);
        signature_out[..n].copy_from_slice(&out[..n]);
    }

    fn signature_len(&self) -> usize {
        32
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        let mut expect = [0u8; 32];
        self.sign(data, &mut expect);
        // SAFETY: both buffers are 32 bytes when the lengths agree.
        if signature.len() == 32 && unsafe { ffi::CRYPTO_memcmp(expect.as_ptr() as *const _, signature.as_ptr() as *const _, 32) } == 0 {
            Ok(())
        } else {
            Err(CryptoError)
        }
    }
}

/// The server's address-validation token key (fixture only): HKDF-SHA256 per token, AES-256-GCM with a zero nonce,
/// as quinn's own `ring` implementation does.
#[doc(hidden)]
pub struct TokenKey([u8; 32]);

impl TokenKey {
    pub fn random() -> TokenKey {
        let mut k = [0u8; 32];
        random_bytes(&mut k);
        TokenKey(k)
    }
}

struct TokenAead(Aead);

impl crypto::HandshakeTokenKey for TokenKey {
    fn aead_from_hkdf(&self, random_bytes: &[u8]) -> Box<dyn crypto::AeadKey> {
        let mut key = [0u8; 32];
        // SAFETY: every pointer is valid for the length given with it.
        unsafe {
            ffi::HKDF_expand(key.as_mut_ptr(), 32, ffi::EVP_sha256(), self.0.as_ptr(), 32, random_bytes.as_ptr(), random_bytes.len());
        }
        Box::new(TokenAead(Aead::new(Suite::Aes256Gcm, &key, &[0u8; 12])))
    }
}

impl crypto::AeadKey for TokenAead {
    fn seal(&self, data: &mut Vec<u8>, additional_data: &[u8]) -> Result<(), CryptoError> {
        let len = data.len();
        data.resize(len + 16, 0);
        let mut out_len = 0usize;
        // SAFETY: in-place seal of `len` bytes into a buffer with room for the tag.
        let ok = unsafe {
            ffi::EVP_AEAD_CTX_seal(self.0.ctx, data.as_mut_ptr(), &mut out_len, data.len(), self.0.iv.as_ptr(), 12, data.as_ptr(), len, additional_data.as_ptr(), additional_data.len())
        };
        if ok == 1 { Ok(()) } else { Err(CryptoError) }
    }

    fn open<'a>(&self, data: &'a mut [u8], additional_data: &[u8]) -> Result<&'a mut [u8], CryptoError> {
        let mut out_len = 0usize;
        let len = data.len();
        // SAFETY: in-place open.
        let ok = unsafe {
            ffi::EVP_AEAD_CTX_open(self.0.ctx, data.as_mut_ptr(), &mut out_len, len, self.0.iv.as_ptr(), 12, data.as_ptr(), len, additional_data.as_ptr(), additional_data.len())
        };
        if ok == 1 { Ok(&mut data[..out_len]) } else { Err(CryptoError) }
    }
}

/// The server half, for the loopback fixture only: a BoringSSL server context that answers `h3`.
#[doc(hidden)]
pub struct ServerConfig {
    ctx: btls::ssl::SslContext,
    params: Box<dyn Fn(&TransportParameters) -> Vec<u8> + Send + Sync>,
}

impl ServerConfig {
    /// A server presenting `cert_chain_pem` / `key_pem`.
    pub fn new(cert_chain_pem: &[u8], key_pem: &[u8]) -> Result<ServerConfig, btls::error::ErrorStack> {
        use btls::{pkey::PKey, ssl::{AlpnError, SslContextBuilder, SslMethod, SslVersion}, x509::X509};
        let mut b = SslContextBuilder::new(SslMethod::tls())?;
        b.set_min_proto_version(Some(SslVersion::TLS1_3))?;
        b.set_max_proto_version(Some(SslVersion::TLS1_3))?;
        let mut certs = X509::stack_from_pem(cert_chain_pem)?.into_iter();
        if let Some(leaf) = certs.next() {
            b.set_certificate(&leaf)?;
        }
        for c in certs {
            b.add_extra_chain_cert(c)?;
        }
        let key = PKey::private_key_from_pem(key_pem)?;
        b.set_private_key(&key)?;
        b.set_alpn_select_callback(|_, client| btls::ssl::select_next_proto(b"\x02h3", client).ok_or(AlpnError::NOACK));
        Ok(ServerConfig {
            ctx: b.build(),
            params: Box::new(|p: &TransportParameters| {
                let mut v = Vec::new();
                p.write(&mut v);
                v
            }),
        })
    }
}

impl crypto::ServerConfig for ServerConfig {
    fn initial_keys(&self, version: u32, dst_cid: &ConnectionId) -> Result<Keys, UnsupportedVersion> {
        if version != QUIC_V1 {
            return Err(UnsupportedVersion);
        }
        Ok(initial_keys(dst_cid, Side::Server))
    }

    fn retry_tag(&self, _version: u32, orig_dst_cid: &ConnectionId, packet: &[u8]) -> [u8; 16] {
        retry_tag(orig_dst_cid, packet)
    }

    fn start_session(self: Arc<Self>, _version: u32, params: &TransportParameters) -> Box<dyn crypto::Session> {
        let ssl = Ssl::new(&self.ctx).expect("SSL_new");
        let tp = (self.params)(params);
        Box::new(Session::new(ssl, Side::Server, &tp).expect("a QUIC server session"))
    }
}
