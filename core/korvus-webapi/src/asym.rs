//! `asym.rs` — RSA and elliptic-curve keys for WebCrypto, on the BoringSSL this binary already
//! links. No v8 here, like its sibling `crypto.rs`: bytes in, bytes out.
//!
//! WHY BORINGSSL AND NOT A PURE-RUST STACK. This is a parity goal, and **BoringSSL is the library
//! Chrome's WebCrypto is implemented on**. Every reading in
//! `tests/fixtures/webcrypto-probe.chrome-150.txt` — which moduli import, which curves are named,
//! what a truncated DER answers, how a 512-bit key behaves under SHA-512 — is that library's
//! behaviour. Reproducing it from a different implementation means re-deriving each of those by
//! probing and getting every one right; reproducing it from the SAME implementation means not
//! having to. WC1 lost 14 WPT subtests learning that on ONE constant (192-bit AES); this lane
//! spans RSA and three curves.
//!
//! WHAT IT COST: nothing. `btls` 0.5.6 (Apache-2.0, BoringSSL) and `btls-sys` (MIT) arrive through
//! `wreq` and are pinned by `[patch.crates-io]`; `core/korvus-webapi` already depended on
//! `transport`, which already linked them. Naming them here added **0 crates and 0 entries** to
//! `Cargo.lock` (measured: 365 before, 365 after) and no C to the build. The alternative priced
//! against it — `rsa 0.9.10` + `p256/p384/p521` — was 26 new crates, 25 CPU-seconds, and
//! `RUSTSEC-2023-0071` (Marvin Attack) with `patched = []`.
//!
//! EVERYTHING IS DER-SHAPED. A `CryptoKey`'s `secret` slot holds the key as BoringSSL's OWN
//! canonical encoding — SPKI for a public key, PKCS#8 for a private one — re-encoded on import
//! rather than echoed back. That is what makes `exportKey` a measurement instead of a memory: the
//! probe's `exportKey spki round trip` compares against the bytes OpenSSL wrote, so a canonical
//! form that disagreed with Chrome's would fail there rather than pass by tautology.

use btls::bn::{BigNum, BigNumContext};
use btls::ec::{EcGroup, EcKey, EcPoint, PointConversionForm};
use btls::ecdsa::EcdsaSig;
use btls::hash::MessageDigest;
use btls::nid::Nid;
use btls::pkey::{Id, PKey, Private, Public};
use btls::rsa::{Padding, Rsa, RsaPrivateKeyBuilder};
use btls::sign::{RsaPssSaltlen, Signer, Verifier};

/// What kind of key a DER blob turned out to hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Rsa,
    Ec,
}

/// The page-visible facts about a parsed key: everything `key.algorithm` needs.
#[derive(Clone, Debug)]
pub struct Info {
    pub kind: Kind,
    /// RSA only: the modulus length in bits, which is `algorithm.modulusLength`.
    pub bits: u32,
    /// RSA only: the public exponent, big-endian and minimally encoded (`algorithm.publicExponent`).
    pub e: Vec<u8>,
    /// EC only: the WebCrypto spelling of the curve (`algorithm.namedCurve`).
    pub curve: Option<&'static str>,
}

/// The three curves Chrome 150 names. `secp256k1` is NOT among them — Chrome answers
/// `NotSupportedError: EcKeyImportParams: Unrecognized namedCurve` for it, measured, so it is
/// absent here rather than mapped.
pub const CURVES: [&str; 3] = ["P-256", "P-384", "P-521"];

fn nid_of(curve: &str) -> Option<Nid> {
    Some(match curve {
        "P-256" => Nid::X9_62_PRIME256V1,
        "P-384" => Nid::SECP384R1,
        "P-521" => Nid::SECP521R1,
        _ => return None,
    })
}

fn curve_of(nid: Nid) -> Option<&'static str> {
    if nid == Nid::X9_62_PRIME256V1 {
        Some("P-256")
    } else if nid == Nid::SECP384R1 {
        Some("P-384")
    } else if nid == Nid::SECP521R1 {
        Some("P-521")
    } else {
        None
    }
}

fn group_of(curve: &str) -> Option<EcGroup> {
    EcGroup::from_curve_name(nid_of(curve)?).ok()
}

/// A curve's coordinate width in bytes — 32, 48, 66. `x`, `y`, `d` and each half of an ECDSA
/// signature are all zero-padded to exactly this, which is what makes P-521 signatures 132 bytes
/// and not "whatever the two integers happened to need".
pub fn field_bytes(curve: &str) -> usize {
    match curve {
        "P-256" => 32,
        "P-384" => 48,
        "P-521" => 66,
        _ => 0,
    }
}

/// A curve's order in bits, which is the ceiling `deriveBits` enforces for ECDH.
pub fn field_bits(curve: &str) -> usize {
    match curve {
        "P-256" => 256,
        "P-384" => 384,
        "P-521" => 521,
        _ => 0,
    }
}

fn md_of(hash: &str) -> Option<MessageDigest> {
    Some(match hash {
        "SHA-1" => MessageDigest::sha1(),
        "SHA-256" => MessageDigest::sha256(),
        "SHA-384" => MessageDigest::sha384(),
        "SHA-512" => MessageDigest::sha512(),
        _ => return None,
    })
}

fn pad_left(v: &[u8], n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    if v.len() <= n {
        out[n - v.len()..].copy_from_slice(v);
    } else {
        // A BigNum never exceeds the field width for a valid key; if it did, keep the low bytes
        // rather than panic, and let the signature/derivation fail its own check.
        out.copy_from_slice(&v[v.len() - n..]);
    }
    out
}

// ── parsing ─────────────────────────────────────────────────────────────────────────────────────

/// Parse an `spki` blob. `None` for anything BoringSSL will not decode, which is the whole of
/// Chrome's `DataError` with an EMPTY message: a truncated key, a trailing byte, a PKCS#8 handed in
/// as an SPKI and a five-byte `SEQUENCE` all land here and are indistinguishable to a page.
pub fn public_from_spki(der: &[u8]) -> Option<PKey<Public>> {
    // NOT `PKey::public_key_from_der`. That is `d2i_PUBKEY`, which parses a PREFIX and hands back a
    // key while leaving trailing bytes unread; Chrome refuses those (`importKey spki trailing byte
    // = THREW DataError:`, measured). `binding::btls_ffi::parse_spki` is BoringSSL's own
    // `EVP_parse_public_key` over a `CBS` with the exhaustion check Chrome makes.
    crate::binding::btls_ffi::parse_spki(der)
}

/// Parse a `pkcs8` blob. Same rule.
pub fn private_from_pkcs8(der: &[u8]) -> Option<PKey<Private>> {
    crate::binding::btls_ffi::parse_pkcs8(der)
}

fn info_of<T: btls::pkey::HasPublic>(k: &btls::pkey::PKeyRef<T>) -> Option<Info> {
    match k.id() {
        Id::RSA => {
            let r = k.rsa().ok()?;
            Some(Info { kind: Kind::Rsa, bits: k.bits(), e: r.e().to_vec(), curve: None })
        }
        Id::EC => {
            let ec = k.ec_key().ok()?;
            let curve = curve_of(ec.group().curve_name()?)?;
            Some(Info { kind: Kind::Ec, bits: k.bits(), e: Vec::new(), curve: Some(curve) })
        }
        _ => None,
    }
}

pub fn info_public(k: &PKey<Public>) -> Option<Info> {
    info_of(k)
}
pub fn info_private(k: &PKey<Private>) -> Option<Info> {
    info_of(k)
}

/// BoringSSL's canonical `spki` for a public key.
pub fn spki_of(k: &PKey<Public>) -> Option<Vec<u8>> {
    k.public_key_to_der().ok()
}

/// BoringSSL's canonical `pkcs8` for a private key.
pub fn pkcs8_of(k: &PKey<Private>) -> Option<Vec<u8>> {
    k.private_key_to_der_pkcs8().ok()
}

// ── RSA, component form (what `jwk` is) ─────────────────────────────────────────────────────────

/// `{kty:"RSA", n, e}` → a public key.
pub fn rsa_public_from_components(n: &[u8], e: &[u8]) -> Option<PKey<Public>> {
    let rsa = Rsa::from_public_components(BigNum::from_slice(n).ok()?, BigNum::from_slice(e).ok()?).ok()?;
    PKey::from_rsa(rsa).ok()
}

/// `{kty:"RSA", n, e, d, p, q, dp, dq, qi}` → a private key. Chrome requires the CRT members: a
/// JWK carrying only `d` answers `DataError: The required JWK member "p" was missing`, measured,
/// so the caller checks for them and this takes all eight.
#[allow(clippy::too_many_arguments)]
pub fn rsa_private_from_components(
    n: &[u8],
    e: &[u8],
    d: &[u8],
    p: &[u8],
    q: &[u8],
    dp: &[u8],
    dq: &[u8],
    qi: &[u8],
) -> Option<PKey<Private>> {
    let b = RsaPrivateKeyBuilder::new(
        BigNum::from_slice(n).ok()?,
        BigNum::from_slice(e).ok()?,
        BigNum::from_slice(d).ok()?,
    )
    .ok()?
    .set_factors(BigNum::from_slice(p).ok()?, BigNum::from_slice(q).ok()?)
    .ok()?
    .set_crt_params(
        BigNum::from_slice(dp).ok()?,
        BigNum::from_slice(dq).ok()?,
        BigNum::from_slice(qi).ok()?,
    )
    .ok()?;
    let rsa = b.build();
    // A JWK can carry components that are individually well-formed and jointly not a key.
    if !rsa.check_key().unwrap_or(false) {
        return None;
    }
    PKey::from_rsa(rsa).ok()
}

/// `(n, e)` of a public RSA key, big-endian and minimally encoded — the two `jwk` members.
pub fn rsa_public_components(k: &PKey<Public>) -> Option<(Vec<u8>, Vec<u8>)> {
    let r = k.rsa().ok()?;
    Some((r.n().to_vec(), r.e().to_vec()))
}

/// The eight `jwk` members of a private RSA key, in JWK order.
pub fn rsa_private_components(k: &PKey<Private>) -> Option<[Vec<u8>; 8]> {
    let r = k.rsa().ok()?;
    let width = ((r.n().num_bits() as usize) + 7) / 8;
    let half = width.div_ceil(2);
    Some([
        pad_left(&r.n().to_vec(), width),
        r.e().to_vec(),
        pad_left(&r.d().to_vec(), width),
        pad_left(&r.p()?.to_vec(), half),
        pad_left(&r.q()?.to_vec(), half),
        pad_left(&r.dmp1()?.to_vec(), half),
        pad_left(&r.dmq1()?.to_vec(), half),
        pad_left(&r.iqmp()?.to_vec(), half),
    ])
}

// ── EC, component form ──────────────────────────────────────────────────────────────────────────

/// An uncompressed point (`raw`, and the `x`/`y` of a `jwk`) → a public key.
pub fn ec_public_from_point(curve: &str, point: &[u8]) -> Option<PKey<Public>> {
    let group = group_of(curve)?;
    let mut ctx = BigNumContext::new().ok()?;
    let p = EcPoint::from_bytes(&group, point, &mut ctx).ok()?;
    let ec = EcKey::from_public_key(&group, &p).ok()?;
    ec.check_key().ok()?;
    PKey::from_ec_key(ec).ok()
}

pub fn ec_public_from_xy(curve: &str, x: &[u8], y: &[u8]) -> Option<PKey<Public>> {
    let mut point = Vec::with_capacity(1 + x.len() + y.len());
    point.push(0x04);
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    ec_public_from_point(curve, &point)
}

pub fn ec_private_from_components(curve: &str, x: &[u8], y: &[u8], d: &[u8]) -> Option<PKey<Private>> {
    let group = group_of(curve)?;
    let mut ctx = BigNumContext::new().ok()?;
    let mut point = Vec::with_capacity(1 + x.len() + y.len());
    point.push(0x04);
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    let p = EcPoint::from_bytes(&group, &point, &mut ctx).ok()?;
    let d = BigNum::from_slice(d).ok()?;
    let ec = EcKey::from_private_components(&group, &d, &p).ok()?;
    ec.check_key().ok()?;
    PKey::from_ec_key(ec).ok()
}

/// The uncompressed point of a public EC key — `exportKey("raw")`, and `x`/`y` split out of it.
pub fn ec_point_of(k: &PKey<Public>) -> Option<Vec<u8>> {
    let ec = k.ec_key().ok()?;
    let group = ec.group();
    let mut ctx = BigNumContext::new().ok()?;
    ec.public_key().to_bytes(group, PointConversionForm::UNCOMPRESSED, &mut ctx).ok()
}

/// `(x, y)` of a public EC key, each zero-padded to the field width.
pub fn ec_xy_of(k: &PKey<Public>) -> Option<(Vec<u8>, Vec<u8>)> {
    let ec = k.ec_key().ok()?;
    let group = ec.group();
    let curve = curve_of(group.curve_name()?)?;
    let n = field_bytes(curve);
    let mut ctx = BigNumContext::new().ok()?;
    let mut x = BigNum::new().ok()?;
    let mut y = BigNum::new().ok()?;
    ec.public_key().affine_coordinates_gfp(group, &mut x, &mut y, &mut ctx).ok()?;
    Some((pad_left(&x.to_vec(), n), pad_left(&y.to_vec(), n)))
}

/// `(x, y, d)` of a private EC key.
pub fn ec_xyd_of(k: &PKey<Private>) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let ec = k.ec_key().ok()?;
    let group = ec.group();
    let curve = curve_of(group.curve_name()?)?;
    let n = field_bytes(curve);
    let mut ctx = BigNumContext::new().ok()?;
    let mut x = BigNum::new().ok()?;
    let mut y = BigNum::new().ok()?;
    ec.public_key().affine_coordinates_gfp(group, &mut x, &mut y, &mut ctx).ok()?;
    Some((
        pad_left(&x.to_vec(), n),
        pad_left(&y.to_vec(), n),
        pad_left(&ec.private_key().to_vec(), n),
    ))
}

/// The public half of a private EC key, as an SPKI — `exportKey("raw")` and the ECDH peer check
/// both need a `PKey<Public>` and a private key is not one.
pub fn ec_public_of_private(k: &PKey<Private>) -> Option<PKey<Public>> {
    let ec = k.ec_key().ok()?;
    let group = ec.group();
    let curve = curve_of(group.curve_name()?)?;
    let mut ctx = BigNumContext::new().ok()?;
    let point = ec.public_key().to_bytes(group, PointConversionForm::UNCOMPRESSED, &mut ctx).ok()?;
    ec_public_from_point(curve, &point)
}

// ── signatures ──────────────────────────────────────────────────────────────────────────────────

/// `RSASSA-PKCS1-v1_5` sign. Deterministic, which is why the probe states it as bytes.
pub fn rsa_pkcs1_sign(key: &PKey<Private>, hash: &str, data: &[u8]) -> Option<Vec<u8>> {
    let mut s = Signer::new(md_of(hash)?, key).ok()?;
    s.update(data).ok()?;
    s.sign_to_vec().ok()
}

pub fn rsa_pkcs1_verify(key: &PKey<Public>, hash: &str, sig: &[u8], data: &[u8]) -> bool {
    let Ok(mut v) = Verifier::new(match md_of(hash) { Some(m) => m, None => return false }, key) else {
        return false;
    };
    if v.update(data).is_err() {
        return false;
    }
    v.verify(sig).unwrap_or(false)
}

/// `RSA-PSS` sign with an explicit salt length in BYTES, which is what `RsaPssParams.saltLength`
/// is. A salt too long for the modulus is BoringSSL's refusal, and Chrome reports it as a bare
/// `OperationError`.
pub fn rsa_pss_sign(key: &PKey<Private>, hash: &str, salt_len: i32, data: &[u8]) -> Option<Vec<u8>> {
    let md = md_of(hash)?;
    let mut s = Signer::new(md, key).ok()?;
    s.set_rsa_padding(Padding::PKCS1_PSS).ok()?;
    s.set_rsa_mgf1_md(md).ok()?;
    s.set_rsa_pss_saltlen(RsaPssSaltlen::custom(salt_len)).ok()?;
    s.update(data).ok()?;
    s.sign_to_vec().ok()
}

pub fn rsa_pss_verify(key: &PKey<Public>, hash: &str, salt_len: i32, sig: &[u8], data: &[u8]) -> bool {
    let Some(md) = md_of(hash) else { return false };
    let Ok(mut v) = Verifier::new(md, key) else { return false };
    if v.set_rsa_padding(Padding::PKCS1_PSS).is_err()
        || v.set_rsa_mgf1_md(md).is_err()
        || v.set_rsa_pss_saltlen(RsaPssSaltlen::custom(salt_len)).is_err()
        || v.update(data).is_err()
    {
        return false;
    }
    v.verify(sig).unwrap_or(false)
}

/// `ECDSA` sign. WebCrypto's signature is the FIXED-WIDTH `r || s`, not the DER `SEQUENCE`
/// OpenSSL-family APIs hand back — 64 bytes on P-256 and 132 on P-521, measured on Chrome 150.
pub fn ecdsa_sign(key: &PKey<Private>, hash: &str, data: &[u8]) -> Option<Vec<u8>> {
    let digest = crate::crypto::digest(hash, data)?;
    let ec = key.ec_key().ok()?;
    let curve = curve_of(ec.group().curve_name()?)?;
    let n = field_bytes(curve);
    let sig = EcdsaSig::sign(&digest, &ec).ok()?;
    let mut out = pad_left(&sig.r().to_vec(), n);
    out.extend_from_slice(&pad_left(&sig.s().to_vec(), n));
    Some(out)
}

/// `ECDSA` verify. A signature of the wrong LENGTH is `false`, not a throw: Chrome answers
/// `ECDSA verify a DER signature = false` for a 70-byte input, measured.
pub fn ecdsa_verify(key: &PKey<Public>, hash: &str, sig: &[u8], data: &[u8]) -> bool {
    let Some(digest) = crate::crypto::digest(hash, data) else { return false };
    let Ok(ec) = key.ec_key() else { return false };
    let Some(nid) = ec.group().curve_name() else { return false };
    let Some(curve) = curve_of(nid) else { return false };
    let n = field_bytes(curve);
    if sig.len() != 2 * n {
        return false;
    }
    let (Ok(r), Ok(s)) = (BigNum::from_slice(&sig[..n]), BigNum::from_slice(&sig[n..])) else {
        return false;
    };
    let Ok(sig) = EcdsaSig::from_private_components(r, s) else { return false };
    sig.verify(&digest, &ec).unwrap_or(false)
}

// ── ECDH ────────────────────────────────────────────────────────────────────────────────────────

/// The raw ECDH shared secret, field-width bytes. The caller truncates to the requested bit count.
pub fn ecdh_derive(private: &PKey<Private>, peer: &PKey<Public>) -> Option<Vec<u8>> {
    let mut d = btls::derive::Deriver::new(private).ok()?;
    d.set_peer(peer).ok()?;
    d.derive_to_vec().ok()
}

/// Chrome's truncation rule for `deriveBits`, measured: `deriveBits(.., 4)` on a secret starting
/// `4b` answers `40` — the first 4 BITS, with the rest of the byte zeroed, not the first byte and
/// not a rounded-up count.
pub fn truncate_bits(secret: &[u8], bits: usize) -> Vec<u8> {
    let whole = bits / 8;
    let rem = bits % 8;
    let mut out = secret[..whole.min(secret.len())].to_vec();
    if rem != 0 && whole < secret.len() {
        out.push(secret[whole] & (0xffu8 << (8 - rem)));
    }
    out
}

// ── OAEP ────────────────────────────────────────────────────────────────────────────────────────

pub fn rsa_oaep_encrypt(key: &PKey<Public>, hash: &str, label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    crate::binding::btls_ffi::oaep_encrypt(key, hash, label, data)
}

pub fn rsa_oaep_decrypt(key: &PKey<Private>, hash: &str, label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    crate::binding::btls_ffi::oaep_decrypt(key, hash, label, data)
}

// ── generation ──────────────────────────────────────────────────────────────────────────────────

/// `(spki, pkcs8)` of a fresh RSA key. Chrome accepts only 3 and 65537 as the exponent on
/// `generateKey` (`The "publicExponent" must be either 3 or 65537`) while accepting any exponent on
/// `importKey`, measured; that asymmetry is the caller's to enforce, not this function's.
pub fn rsa_generate(bits: u32, e: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let e = BigNum::from_slice(e).ok()?;
    let rsa = Rsa::generate_with_e(bits, &e).ok()?;
    let private = PKey::from_rsa(rsa).ok()?;
    let spki = private.public_key_to_der().ok()?;
    let pkcs8 = private.private_key_to_der_pkcs8().ok()?;
    Some((spki, pkcs8))
}

/// `(spki, pkcs8)` of a fresh EC key on `curve`.
pub fn ec_generate(curve: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let group = group_of(curve)?;
    let ec = EcKey::generate(&group).ok()?;
    let private = PKey::from_ec_key(ec).ok()?;
    let spki = private.public_key_to_der().ok()?;
    let pkcs8 = private.private_key_to_der_pkcs8().ok()?;
    Some((spki, pkcs8))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7515 §A.2.1's RSA key, as the JWK members WebCrypto would carry. A KNOWN-ANSWER test for
    /// the component path: if `n`/`e` were assembled wrong, the SPKI below would not be the one
    /// BoringSSL derives from the same numbers by the other route.
    #[test]
    fn rsa_components_round_trip_through_der() {
        let (spki, pkcs8) = rsa_generate(2048, &[1, 0, 1]).expect("generate");
        let pubk = public_from_spki(&spki).expect("parse spki");
        let privk = private_from_pkcs8(&pkcs8).expect("parse pkcs8");
        let (n, e) = rsa_public_components(&pubk).expect("components");
        assert_eq!(e, vec![1, 0, 1], "65537 is minimally encoded");
        let rebuilt = rsa_public_from_components(&n, &e).expect("rebuild");
        assert_eq!(spki_of(&rebuilt).unwrap(), spki, "n/e rebuild is the same key");
        let c = rsa_private_components(&privk).expect("private components");
        let reb = rsa_private_from_components(&c[0], &c[1], &c[2], &c[3], &c[4], &c[5], &c[6], &c[7])
            .expect("private rebuild");
        assert_eq!(pkcs8_of(&reb).unwrap(), pkcs8, "CRT rebuild is the same key");
        let info = info_public(&pubk).unwrap();
        assert_eq!((info.kind, info.bits), (Kind::Rsa, 2048));
    }

    #[test]
    fn rsa_pkcs1_signature_verifies_and_a_tampered_one_does_not() {
        let (spki, pkcs8) = rsa_generate(2048, &[1, 0, 1]).unwrap();
        let pubk = public_from_spki(&spki).unwrap();
        let privk = private_from_pkcs8(&pkcs8).unwrap();
        for hash in ["SHA-1", "SHA-256", "SHA-384", "SHA-512"] {
            let sig = rsa_pkcs1_sign(&privk, hash, b"korvus").expect(hash);
            assert_eq!(sig.len(), 256);
            assert!(rsa_pkcs1_verify(&pubk, hash, &sig, b"korvus"), "{hash} verify");
            assert!(!rsa_pkcs1_verify(&pubk, hash, &sig, b"korvuz"), "{hash} tampered data");
            let mut bad = sig.clone();
            bad[0] ^= 1;
            assert!(!rsa_pkcs1_verify(&pubk, hash, &bad, b"korvus"), "{hash} tampered sig");
        }
        // Deterministic: PKCS#1 v1.5 has no salt, so two signings agree byte for byte.
        assert_eq!(
            rsa_pkcs1_sign(&privk, "SHA-256", b"korvus").unwrap(),
            rsa_pkcs1_sign(&privk, "SHA-256", b"korvus").unwrap()
        );
    }

    #[test]
    fn rsa_oaep_round_trips_and_the_label_is_authenticated() {
        let (spki, pkcs8) = rsa_generate(2048, &[1, 0, 1]).unwrap();
        let pubk = public_from_spki(&spki).unwrap();
        let privk = private_from_pkcs8(&pkcs8).unwrap();
        let ct = rsa_oaep_encrypt(&pubk, "SHA-256", b"", b"hello").expect("encrypt");
        assert_eq!(ct.len(), 256);
        assert_eq!(rsa_oaep_decrypt(&privk, "SHA-256", b"", &ct).as_deref(), Some(&b"hello"[..]));
        // The digest is a real parameter and not decoration: SHA-1 cannot open a SHA-256 message.
        assert_eq!(rsa_oaep_decrypt(&privk, "SHA-1", b"", &ct), None);
        let ct = rsa_oaep_encrypt(&pubk, "SHA-256", b"label", b"hello").unwrap();
        assert_eq!(rsa_oaep_decrypt(&privk, "SHA-256", b"label", &ct).as_deref(), Some(&b"hello"[..]));
        assert_eq!(rsa_oaep_decrypt(&privk, "SHA-256", b"", &ct), None, "a wrong label must fail");
        // 2048-bit modulus, SHA-256: k - 2*hLen - 2 = 256 - 64 - 2 = 190.
        assert!(rsa_oaep_encrypt(&pubk, "SHA-256", b"", &vec![0u8; 190]).is_some());
        assert!(rsa_oaep_encrypt(&pubk, "SHA-256", b"", &vec![0u8; 191]).is_none());
    }

    #[test]
    fn rsa_pss_round_trips_and_an_oversized_salt_is_refused() {
        let (spki, pkcs8) = rsa_generate(2048, &[1, 0, 1]).unwrap();
        let pubk = public_from_spki(&spki).unwrap();
        let privk = private_from_pkcs8(&pkcs8).unwrap();
        let sig = rsa_pss_sign(&privk, "SHA-256", 32, b"korvus").expect("pss");
        assert!(rsa_pss_verify(&pubk, "SHA-256", 32, &sig, b"korvus"));
        assert!(!rsa_pss_verify(&pubk, "SHA-256", 32, &sig, b"other"));
        assert!(rsa_pss_sign(&privk, "SHA-256", 0, b"korvus").is_some());
        assert!(rsa_pss_sign(&privk, "SHA-256", 256, b"korvus").is_none());
    }

    #[test]
    fn ec_components_round_trip_and_signatures_are_fixed_width() {
        for curve in CURVES {
            let (spki, pkcs8) = ec_generate(curve).unwrap();
            let pubk = public_from_spki(&spki).unwrap();
            let privk = private_from_pkcs8(&pkcs8).unwrap();
            let n = field_bytes(curve);
            let (x, y) = ec_xy_of(&pubk).unwrap();
            assert_eq!((x.len(), y.len()), (n, n), "{curve} coordinates are padded");
            assert_eq!(spki_of(&ec_public_from_xy(curve, &x, &y).unwrap()).unwrap(), spki);
            let point = ec_point_of(&pubk).unwrap();
            assert_eq!(point.len(), 1 + 2 * n);
            assert_eq!(point[0], 0x04, "{curve} raw is the UNCOMPRESSED form");
            let (x2, y2, d) = ec_xyd_of(&privk).unwrap();
            assert_eq!(d.len(), n);
            assert_eq!(
                pkcs8_of(&ec_private_from_components(curve, &x2, &y2, &d).unwrap()).unwrap(),
                pkcs8
            );
            let hash = if curve == "P-521" { "SHA-512" } else { "SHA-256" };
            let sig = ecdsa_sign(&privk, hash, b"korvus").unwrap();
            assert_eq!(sig.len(), 2 * n, "{curve} signature width");
            assert!(ecdsa_verify(&pubk, hash, &sig, b"korvus"));
            assert!(!ecdsa_verify(&pubk, hash, &sig, b"other"));
            // Chrome answers `false`, not a throw, for a signature of the wrong length.
            assert!(!ecdsa_verify(&pubk, hash, &vec![0u8; 70], b"korvus"));
        }
    }

    #[test]
    fn ecdh_agrees_in_both_directions() {
        let (spki_a, p8_a) = ec_generate("P-256").unwrap();
        let (spki_b, p8_b) = ec_generate("P-256").unwrap();
        let a = ecdh_derive(&private_from_pkcs8(&p8_a).unwrap(), &public_from_spki(&spki_b).unwrap()).unwrap();
        let b = ecdh_derive(&private_from_pkcs8(&p8_b).unwrap(), &public_from_spki(&spki_a).unwrap()).unwrap();
        assert_eq!(a, b, "both sides derive the same secret");
        assert_eq!(a.len(), 32);
    }

    /// Chrome 150's own numbers: a secret beginning `0x4b` truncated to 4 bits is `0x40`.
    #[test]
    fn truncate_bits_matches_chromes_rule() {
        let s = [0x4bu8, 0xb9, 0x14, 0x67];
        assert_eq!(truncate_bits(&s, 32), s.to_vec());
        assert_eq!(truncate_bits(&s, 16), vec![0x4b, 0xb9]);
        assert_eq!(truncate_bits(&s, 4), vec![0x40]);
        assert_eq!(truncate_bits(&s, 1), vec![0x00]);
        assert_eq!(truncate_bits(&s, 0), Vec::<u8>::new());
        assert_eq!(truncate_bits(&s, 12), vec![0x4b, 0xb0]);
    }

    /// The whole of Chrome's empty-message `DataError`, in one place.
    #[test]
    fn malformed_der_is_refused_every_way_it_can_be_malformed() {
        let (spki, pkcs8) = rsa_generate(2048, &[1, 0, 1]).unwrap();
        assert!(public_from_spki(&spki).is_some());
        assert!(public_from_spki(&spki[..spki.len() - 3]).is_none(), "truncated");
        assert!(public_from_spki(&[]).is_none(), "empty");
        assert!(public_from_spki(&[0x30, 0x03, 0x02, 0x01, 0x00]).is_none(), "garbage");
        let mut trailing = spki.clone();
        trailing.push(0);
        assert!(public_from_spki(&trailing).is_none(), "a trailing byte");
        assert!(public_from_spki(&pkcs8).is_none(), "a pkcs8 handed in as an spki");
        assert!(private_from_pkcs8(&spki).is_none(), "an spki handed in as a pkcs8");
        assert!(private_from_pkcs8(&pkcs8[..400]).is_none(), "truncated pkcs8");
    }

    /// Chrome imposes NO modulus floor on `importKey`: 512 bits parses and reports 512. This is a
    /// place the browser and the spec's usual assumptions part company, and the browser wins (§5).
    #[test]
    fn small_moduli_import_because_chrome_imports_them() {
        for bits in [512u32, 1024, 2048] {
            let (spki, _) = rsa_generate(bits, &[1, 0, 1]).unwrap();
            let k = public_from_spki(&spki).expect("parse");
            assert_eq!(info_public(&k).unwrap().bits, bits);
        }
        let (spki, _) = rsa_generate(2048, &[3]).unwrap();
        assert_eq!(info_public(&public_from_spki(&spki).unwrap()).unwrap().e, vec![3]);
    }

    #[test]
    fn a_curve_chrome_does_not_name_has_no_mapping() {
        assert!(nid_of("secp256k1").is_none());
        assert!(nid_of("P-257").is_none());
        assert!(group_of("P-256").is_some());
        assert_eq!(field_bytes("P-521"), 66);
        assert_eq!(field_bits("P-521"), 521);
    }
}
