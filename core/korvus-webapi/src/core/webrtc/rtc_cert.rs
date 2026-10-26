//! `core/webrtc/rtc_cert.rs` -- the self-signed ECDSA P-256 certificate the WebRTC SEND path presents in
//! its DTLS handshake, and the SHA-256 of it that becomes the offer's `a=fingerprint`. Minted the first
//! time a connection offers a fed sending track; a connection that never sends keeps the fake fingerprint
//! `peer_connection::ice_identity` draws, and this is never called (invariant I9). The DER and the key
//! are handed to `korvus-runtime`'s `op_rtc_dtls_connect`, which runs the handshake from them.
//!
//! The same BoringSSL the rest of the engine uses (`btls`), the same EC/X509 surface `asym.rs` already
//! builds keys with -- so this adds no new crate.

use btls::asn1::{Asn1Integer, Asn1Time};
use btls::bn::BigNum;
use btls::ec::{EcGroup, EcKey};
use btls::hash::MessageDigest;
use btls::nid::Nid;
use btls::pkey::PKey;
use btls::x509::{X509Builder, X509NameBuilder};

/// A fresh self-signed P-256 certificate: `(certificate DER, private key PKCS#8 DER, SHA-256 fingerprint
/// as upper-hex colon-separated -- the `a=fingerprint` form)`. `None` if BoringSSL refuses any step
/// (never seen; the caller then keeps the fake fingerprint so the connection still works).
pub(crate) fn mint() -> Option<(Vec<u8>, Vec<u8>, String)> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).ok()?;
    let ec = EcKey::generate(&group).ok()?;
    let key = PKey::from_ec_key(ec).ok()?;
    let mut name = X509NameBuilder::new().ok()?;
    name.append_entry_by_text("CN", "WebRTC").ok()?;
    let name = name.build();
    let one = BigNum::from_u32(1).ok()?;
    let serial = Asn1Integer::from_bn(&one).ok()?;
    let not_before = Asn1Time::days_from_now(0).ok()?;
    let not_after = Asn1Time::days_from_now(30).ok()?;
    let mut b = X509Builder::new().ok()?;
    b.set_version(2).ok()?;
    b.set_serial_number(&serial).ok()?;
    b.set_subject_name(&name).ok()?;
    b.set_issuer_name(&name).ok()?;
    b.set_pubkey(&key).ok()?;
    b.set_not_before(&not_before).ok()?;
    b.set_not_after(&not_after).ok()?;
    b.sign(&key, MessageDigest::sha256()).ok()?;
    let cert = b.build();
    let der = cert.to_der().ok()?;
    let fp = cert.digest(MessageDigest::sha256()).ok()?.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":");
    let key_der = key.private_key_to_der_pkcs8().ok()?;
    Some((der, key_der, fp))
}

/// Lower-hex, for carrying DER through a v8 string slot to the handshake op.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_yields_a_p256_cert_with_a_sha256_fingerprint() {
        let (der, key, fp) = mint().expect("mint");
        assert!(der.len() > 200 && key.len() > 60, "plausible DER sizes");
        // 32 bytes -> 32 upper-hex pairs joined by 31 colons
        assert_eq!(fp.len(), 32 * 2 + 31);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase() || c == ':'));
        // the cert round-trips through the hex the op will decode
        assert_eq!(hex(&[0x00, 0xAB, 0xff]), "00abff");
        assert!(btls::x509::X509::from_der(&der).is_ok());
    }
}
