//! Native WebCrypto (SubtleCrypto) core (surface-fidelity work, LIES 47->38; history in git).
//! Pure-Rust primitives + validation helpers with NO v8 dependency, unit-tested in isolation before the
//! cppgc `CryptoKey`/`SubtleCrypto` classes (bindings.rs) are built on top. These mirror the primitives the
//! engine crypto ops used (same RustCrypto stack); the v8-touching algorithm normalization lives in bindings.rs.

// Foundational core: consumed by the SubtleCrypto cppgc classes (bindings.rs) in later steps of the port.
#![allow(dead_code)]

/// The WebCrypto hashes Korvus supports (canonical spec spelling).
pub const HASHES: [&str; 4] = ["SHA-1", "SHA-256", "SHA-384", "SHA-512"];
pub fn is_supported_hash(h: &str) -> bool { HASHES.contains(&h) }
/// AES key sizes (bits) this family of browsers accepts.
///
/// NOT the spec's set. WebCrypto names 128, 192 and 256; Chrome 150 refuses 192 everywhere it can
/// appear -- `generateKey`, `importKey("raw")` and `importKey("jwk")` all answer
/// `OperationError: 192-bit AES keys are not supported`, measured (the `192` lines of
/// `webcrypto-probe.chrome-150.txt`). [`AES_192_UNSUPPORTED`] is that refusal's message; it is a
/// DIFFERENT error from an unrecognized length, which is why 192 is not simply absent from here.
pub fn aes_key_len_ok(bits: i64) -> bool { matches!(bits, 128 | 256) }
/// Chrome 150's word for a 192-bit AES key, which it recognizes and declines.
pub const AES_192_UNSUPPORTED: &str = "192-bit AES keys are not supported";
/// The AES-GCM authentication-tag lengths (bits) Chrome 150 accepts.
pub const GCM_TAG_BITS: [i64; 7] = [32, 64, 96, 104, 112, 120, 128];
pub fn gcm_tag_len_ok(bits: i64) -> bool { GCM_TAG_BITS.contains(&bits) }
/// `usages` is a non-empty subset of `allowed` (the spec's key-usage check).
pub fn usages_ok(usages: &[String], allowed: &[&str]) -> bool {
    !usages.is_empty() && usages.iter().all(|u| allowed.contains(&u.as_str()))
}

/// Hash `data` with `algo` (canonical uppercase); `None` for an unsupported algorithm.
pub fn digest(algo: &str, data: &[u8]) -> Option<Vec<u8>> {
    use sha2::{Digest, Sha256, Sha384, Sha512};
    Some(match algo {
        "SHA-1" => sha1::Sha1::digest(data).to_vec(),
        "SHA-256" => Sha256::digest(data).to_vec(),
        "SHA-384" => Sha384::digest(data).to_vec(),
        "SHA-512" => Sha512::digest(data).to_vec(),
        _ => return None,
    })
}

/// HMAC-`hash` over `data` with `key` (any key length); `None` for an unsupported hash.
pub fn hmac_sign(hash: &str, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use hmac::{Hmac, Mac};
    use sha2::{Sha256, Sha384, Sha512};
    macro_rules! mac {
        ($d:ty) => {{
            let mut m = <Hmac<$d> as Mac>::new_from_slice(key).expect("hmac takes any key length");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    Some(match hash {
        "SHA-1" => mac!(sha1::Sha1),
        "SHA-256" => mac!(Sha256),
        "SHA-384" => mac!(Sha384),
        "SHA-512" => mac!(Sha512),
        _ => return None,
    })
}

/// The AES block cipher at the three key sizes, so the five modes below share ONE key-length
/// decision instead of each re-deriving it. `None` for a key length AES does not define.
///
/// 192 is built here although [`aes_key_len_ok`] refuses it: the refusal is a SURFACE decision of
/// this browser family and belongs at the call site that can name it, not in the block cipher.
enum Aes {
    K128(Box<aes::Aes128>),
    K192(Box<aes::Aes192>),
    K256(Box<aes::Aes256>),
}

impl Aes {
    fn new(key: &[u8]) -> Option<Aes> {
        use aes::cipher::{generic_array::GenericArray, KeyInit};
        Some(match key.len() {
            16 => Aes::K128(Box::new(aes::Aes128::new(GenericArray::from_slice(key)))),
            24 => Aes::K192(Box::new(aes::Aes192::new(GenericArray::from_slice(key)))),
            32 => Aes::K256(Box::new(aes::Aes256::new(GenericArray::from_slice(key)))),
            _ => return None,
        })
    }
    fn encrypt_block(&self, b: &mut [u8; 16]) {
        use aes::cipher::{generic_array::GenericArray, BlockEncrypt};
        let g = GenericArray::from_mut_slice(b);
        match self {
            Aes::K128(c) => c.encrypt_block(g),
            Aes::K192(c) => c.encrypt_block(g),
            Aes::K256(c) => c.encrypt_block(g),
        }
    }
    fn decrypt_block(&self, b: &mut [u8; 16]) {
        use aes::cipher::{generic_array::GenericArray, BlockDecrypt};
        let g = GenericArray::from_mut_slice(b);
        match self {
            Aes::K128(c) => c.decrypt_block(g),
            Aes::K192(c) => c.decrypt_block(g),
            Aes::K256(c) => c.decrypt_block(g),
        }
    }
}

/// AES-CTR encrypt/decrypt (symmetric). `length` = counter bit-width (1..=128). `None` on an invalid key
/// size / counter length / exhausted counter block space (the spec's OperationError).
pub fn aes_ctr(key: &[u8], counter: &[u8], length: i32, data: &[u8]) -> Option<Vec<u8>> {
    if counter.len() != 16 || !(1..=128).contains(&length) {
        return None;
    }
    let length = length as u32;
    let num_blocks = data.len().div_ceil(16) as u128;
    if length < 128 && num_blocks > (1u128 << length) {
        return None;
    }
    let aes = Aes::new(key)?;
    let mask: u128 = if length >= 128 { u128::MAX } else { (1u128 << length) - 1 };
    let mut ctr = [0u8; 16];
    ctr.copy_from_slice(counter);
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        let mut ks = ctr;
        aes.encrypt_block(&mut ks);
        for (i, &b) in block.iter().enumerate() {
            out.push(b ^ ks[i]);
        }
        // Increment only the low `length` bits; the high (nonce) bits stay fixed.
        let c = u128::from_be_bytes(ctr);
        let low = (c & mask).wrapping_add(1) & mask;
        ctr = ((c & !mask) | low).to_be_bytes();
    }
    Some(out)
}

/// AES-CBC encrypt with PKCS#7 padding (WebCrypto's CBC is always padded, including a whole extra
/// block for an input that is already a multiple of 16). `None` on a bad key or IV length.
pub fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if iv.len() != 16 {
        return None;
    }
    let aes = Aes::new(key)?;
    let pad = 16 - (data.len() % 16);
    let mut out = Vec::with_capacity(data.len() + pad);
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat(pad as u8).take(pad));
    for block in padded.chunks(16) {
        let mut b = [0u8; 16];
        for i in 0..16 {
            b[i] = block[i] ^ prev[i];
        }
        aes.encrypt_block(&mut b);
        out.extend_from_slice(&b);
        prev = b;
    }
    Some(out)
}

/// AES-CBC decrypt, PKCS#7 padding removed and VALIDATED. `None` on a bad key or IV length, a
/// ciphertext that is not a non-zero multiple of the block size, or padding that does not check out
/// -- all of which Chrome reports as the same empty-message `OperationError`, so one `None` suffices.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if iv.len() != 16 || data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let aes = Aes::new(key)?;
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    for block in data.chunks(16) {
        let mut b = [0u8; 16];
        b.copy_from_slice(block);
        let cipher_block = b;
        aes.decrypt_block(&mut b);
        for i in 0..16 {
            out.push(b[i] ^ prev[i]);
        }
        prev = cipher_block;
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || pad > out.len() {
        return None;
    }
    if out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

// ── AES-GCM (NIST SP 800-38D) ───────────────────────────────────────────────────────────────────
//
// Written here on the `aes` block cipher rather than pulled in as `aes-gcm`, because adding a
// dependency is external traffic and this is ~50 lines. The KNOWN-ANSWER TESTS are Chrome 150's own
// ciphertexts, recorded in `webcrypto-probe.chrome-150.txt` and re-run by `webcrypto_live.rs`: a
// GHASH with a wrong reduction polynomial produces a plausible-looking tag and fails them instantly.

/// Multiply in GF(2^128) with GCM's bit order: the MOST significant bit of `x` is its coefficient 0,
/// and the reduction polynomial is x^128 + x^7 + x^2 + x + 1 (0xe1 in the top byte).
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let mut z: u128 = 0;
    let mut v = y;
    for i in 0..128 {
        if (x >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        let lsb = v & 1;
        v >>= 1;
        if lsb == 1 {
            v ^= R;
        }
    }
    z
}

/// GHASH_H(A || pad || C || pad || [len(A)]64 || [len(C)]64).
fn ghash(h: u128, aad: &[u8], ct: &[u8]) -> u128 {
    let mut y: u128 = 0;
    for part in [aad, ct] {
        for chunk in part.chunks(16) {
            let mut b = [0u8; 16];
            b[..chunk.len()].copy_from_slice(chunk);
            y = gf_mul(y ^ u128::from_be_bytes(b), h);
        }
    }
    let mut last = [0u8; 16];
    last[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
    last[8..].copy_from_slice(&((ct.len() as u64) * 8).to_be_bytes());
    gf_mul(y ^ u128::from_be_bytes(last), h)
}

/// GCM's counter increment: the rightmost 32 bits only, modulo 2^32.
fn inc32(x: u128) -> u128 {
    let low = u128::from((x as u32).wrapping_add(1));
    (x & !0xffff_ffffu128) | low
}

fn gctr(aes: &Aes, icb: u128, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut ctr = icb;
    for block in data.chunks(16) {
        let mut ks = ctr.to_be_bytes();
        aes.encrypt_block(&mut ks);
        for (i, &b) in block.iter().enumerate() {
            out.push(b ^ ks[i]);
        }
        ctr = inc32(ctr);
    }
    out
}

/// GCM's J0: a 96-bit IV is used directly with a counter of 1; any other length is GHASHed.
fn gcm_j0(h: u128, iv: &[u8]) -> u128 {
    if iv.len() == 12 {
        let mut b = [0u8; 16];
        b[..12].copy_from_slice(iv);
        b[15] = 1;
        return u128::from_be_bytes(b);
    }
    // GHASH_H(IV || 0^(s+64) || [len(IV)]_64), which is what `ghash` computes for an empty AAD.
    ghash(h, &[], iv)
}

fn gcm_h(aes: &Aes) -> u128 {
    let mut zero = [0u8; 16];
    aes.encrypt_block(&mut zero);
    u128::from_be_bytes(zero)
}

/// AES-GCM encrypt: ciphertext followed by the leading `tag_bytes` of the tag, which is how
/// WebCrypto returns it. `None` on a bad key length or an empty IV (GCM has no J0 for one).
pub fn aes_gcm_encrypt(key: &[u8], iv: &[u8], aad: &[u8], tag_bytes: usize, data: &[u8]) -> Option<Vec<u8>> {
    if iv.is_empty() || tag_bytes == 0 || tag_bytes > 16 {
        return None;
    }
    let aes = Aes::new(key)?;
    let h = gcm_h(&aes);
    let j0 = gcm_j0(h, iv);
    let mut out = gctr(&aes, inc32(j0), data);
    let s = ghash(h, aad, &out);
    let tag = gctr(&aes, j0, &s.to_be_bytes());
    out.extend_from_slice(&tag[..tag_bytes]);
    Some(out)
}

/// AES-GCM decrypt. `data` is ciphertext || tag. `None` on a bad key length, an empty IV, a `data`
/// shorter than the tag, or a tag that does not authenticate.
pub fn aes_gcm_decrypt(key: &[u8], iv: &[u8], aad: &[u8], tag_bytes: usize, data: &[u8]) -> Option<Vec<u8>> {
    if iv.is_empty() || tag_bytes == 0 || tag_bytes > 16 || data.len() < tag_bytes {
        return None;
    }
    let aes = Aes::new(key)?;
    let (ct, tag) = data.split_at(data.len() - tag_bytes);
    let h = gcm_h(&aes);
    let j0 = gcm_j0(h, iv);
    let s = ghash(h, aad, ct);
    let expected = gctr(&aes, j0, &s.to_be_bytes());
    if !ct_eq(tag, &expected[..tag_bytes]) {
        return None;
    }
    Some(gctr(&aes, inc32(j0), ct))
}

// ── AES-KW (RFC 3394) ───────────────────────────────────────────────────────────────────────────

const KW_IV: [u8; 8] = [0xa6; 8];

/// RFC 3394 key wrap. `data` must be at least two 8-byte semiblocks; `None` otherwise, or on a bad
/// KEK length. The caller distinguishes "not a multiple of 8" from "too small" -- Chrome answers
/// those with a DataError and an OperationError respectively.
pub fn aes_kw_wrap(kek: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 16 || data.len() % 8 != 0 {
        return None;
    }
    let aes = Aes::new(kek)?;
    let n = data.len() / 8;
    let mut a = KW_IV;
    let mut r: Vec<[u8; 8]> = data.chunks(8).map(|c| { let mut b = [0u8; 8]; b.copy_from_slice(c); b }).collect();
    for j in 0..6u64 {
        for i in 1..=n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i - 1]);
            aes.encrypt_block(&mut b);
            let t = ((n as u64) * j + i as u64).to_be_bytes();
            for k in 0..8 {
                a[k] = b[k] ^ t[k];
            }
            r[i - 1].copy_from_slice(&b[8..]);
        }
    }
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&a);
    for x in &r {
        out.extend_from_slice(x);
    }
    Some(out)
}

/// RFC 3394 key unwrap. `None` on a bad KEK length, a `data` that is not at least three semiblocks,
/// or an integrity check that fails.
pub fn aes_kw_unwrap(kek: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 24 || data.len() % 8 != 0 {
        return None;
    }
    let aes = Aes::new(kek)?;
    let n = data.len() / 8 - 1;
    let mut a = [0u8; 8];
    a.copy_from_slice(&data[..8]);
    let mut r: Vec<[u8; 8]> = data[8..].chunks(8).map(|c| { let mut b = [0u8; 8]; b.copy_from_slice(c); b }).collect();
    for j in (0..6u64).rev() {
        for i in (1..=n).rev() {
            let t = ((n as u64) * j + i as u64).to_be_bytes();
            let mut b = [0u8; 16];
            for k in 0..8 {
                b[k] = a[k] ^ t[k];
            }
            b[8..].copy_from_slice(&r[i - 1]);
            aes.decrypt_block(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i - 1].copy_from_slice(&b[8..]);
        }
    }
    if !ct_eq(&a, &KW_IV) {
        return None;
    }
    let mut out = Vec::with_capacity(n * 8);
    for x in &r {
        out.extend_from_slice(x);
    }
    Some(out)
}

// ── base64url, for JWK's `k` member ─────────────────────────────────────────────────────────────

/// JWK's unpadded base64url. Chrome REFUSES padding (`The JWK member "k" could not be base64url
/// decoded or contained padding`, measured), so the engine below rejects `=` rather than tolerating it.
pub fn b64url_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The inverse; `None` for a non-alphabet character, for `+`/`/`, or for any padding.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()
}

/// Fresh X25519 (private, public) key pair, each 32 bytes.
pub fn x25519_keypair() -> ([u8; 32], [u8; 32]) {
    use x25519_dalek::{PublicKey, StaticSecret};
    let mut seed = [0u8; 32];
    let _ = getrandom::getrandom(&mut seed);
    let secret = StaticSecret::from(seed);
    let public = PublicKey::from(&secret);
    (secret.to_bytes(), public.to_bytes())
}

/// X25519 ECDH: the leading `bits/8` bytes of the shared secret; `None` on malformed keys.
pub fn x25519_derive(private: &[u8], public: &[u8], bits: i32) -> Option<Vec<u8>> {
    use x25519_dalek::{PublicKey, StaticSecret};
    if private.len() != 32 || public.len() != 32 {
        return None;
    }
    let mut sk = [0u8; 32];
    let mut pk = [0u8; 32];
    sk.copy_from_slice(private);
    pk.copy_from_slice(public);
    let secret = StaticSecret::from(sk);
    let shared = secret.diffie_hellman(&PublicKey::from(pk));
    let n = ((bits.max(0) as usize) / 8).min(32);
    Some(shared.as_bytes()[..n].to_vec())
}

/// `n` cryptographically-random bytes (for AES/HMAC key generation).
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let _ = getrandom::getrandom(&mut v);
    v
}

/// Constant-time equality (HMAC verify).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
    }

    #[test]
    fn digest_sha256_abc() {
        // FIPS 180-4 known-answer: SHA-256("abc").
        assert_eq!(hex(&digest("SHA-256", b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest("SHA-1", b"abc").unwrap().len(), 20);
        assert_eq!(digest("SHA-512", b"abc").unwrap().len(), 64);
        assert!(digest("MD5", b"abc").is_none());
    }

    #[test]
    fn hmac_sha256_known_vector() {
        // RFC 4231 test case 1: key = 20*0x0b, data = "Hi There".
        let key = [0x0bu8; 20];
        assert_eq!(hex(&hmac_sign("SHA-256", &key, b"Hi There").unwrap()),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assert!(hmac_sign("SHA-999", &key, b"x").is_none());
    }

    #[test]
    fn aes_ctr_roundtrip_and_limits() {
        let key = [0u8; 16];
        let counter = [0u8; 16];
        let data = b"the quick brown fox jumps over!!"; // 32 bytes = 2 blocks
        let ct = aes_ctr(&key, &counter, 64, data).unwrap();
        assert_ne!(&ct[..], &data[..]);
        let pt = aes_ctr(&key, &counter, 64, &ct).unwrap(); // CTR decrypt == encrypt
        assert_eq!(&pt[..], &data[..]);
        assert!(aes_ctr(&key, &[0u8; 8], 64, data).is_none()); // bad counter length
        assert!(aes_ctr(&key, &counter, 0, data).is_none()); // bad length
        // length=1 => only 2 counter blocks; 3 blocks of data exhausts it.
        assert!(aes_ctr(&key, &counter, 1, &[0u8; 48]).is_none());
    }

    #[test]
    fn aes_cbc_matches_nist_and_validates_its_padding() {
        // NIST SP 800-38A F.2.1, CBC-AES128.Encrypt, first block. WebCrypto always pads, so a
        // 16-byte input produces 32 bytes whose FIRST block is the standard's.
        let key = hex_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let pt = hex_bytes("6bc1bee22e409f96e93d7e117393172a");
        let ct = aes_cbc_encrypt(&key, &iv, &pt).unwrap();
        assert_eq!(ct.len(), 32);
        assert_eq!(hex(&ct[..16]), "7649abac8119b246cee98e9b12e9197d");
        assert_eq!(aes_cbc_decrypt(&key, &iv, &ct).unwrap(), pt);
        // An empty message is one whole block of padding, and it round-trips.
        let empty = aes_cbc_encrypt(&key, &iv, b"").unwrap();
        assert_eq!(empty.len(), 16);
        assert!(aes_cbc_decrypt(&key, &iv, &empty).unwrap().is_empty());
        // Padding is VALIDATED, not trusted, and a ciphertext off the block boundary is refused.
        assert!(aes_cbc_decrypt(&key, &iv, &[0u8; 16]).is_none());
        assert!(aes_cbc_decrypt(&key, &iv, &[0u8; 15]).is_none());
        assert!(aes_cbc_decrypt(&key, &iv, &[]).is_none());
        assert!(aes_cbc_encrypt(&key, &[0u8; 15], &pt).is_none());
    }

    #[test]
    fn aes_gcm_matches_the_nist_test_cases() {
        // NIST SP 800-38D, GCM test cases 1-3 (AES-128). Case 1 is the tag over nothing at all,
        // which is the one a wrong GHASH reduction gets wrong first.
        let z16 = [0u8; 16];
        let z12 = [0u8; 12];
        assert_eq!(hex(&aes_gcm_encrypt(&z16, &z12, &[], 16, &[]).unwrap()),
            "58e2fccefa7e3061367f1d57a4e7455a");
        assert_eq!(hex(&aes_gcm_encrypt(&z16, &z12, &[], 16, &z16).unwrap()),
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf");
        // Case 3: a real key, a real IV, four blocks of plaintext, no AAD.
        let key = hex_bytes("feffe9928665731c6d6a8f9467308308");
        let iv = hex_bytes("cafebabefacedbaddecaf888");
        let pt = hex_bytes("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39\
1aafd255");
        let ct = aes_gcm_encrypt(&key, &iv, &[], 16, &pt).unwrap();
        assert_eq!(hex(&ct), "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091\
473f59854d5c2af327cd64a62cf35abd2ba6fab4");
        assert_eq!(aes_gcm_decrypt(&key, &iv, &[], 16, &ct).unwrap(), pt);
        // A 96-bit IV goes in directly; any other length is GHASHed, and the two must differ.
        let long_iv = aes_gcm_encrypt(&key, &[0u8; 16], &[], 16, &pt).unwrap();
        assert_ne!(long_iv, ct);
        assert_eq!(aes_gcm_decrypt(&key, &[0u8; 16], &[], 16, &long_iv).unwrap(), pt);
        // Authentication is real: a flipped bit in the body or a changed AAD refuses.
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert!(aes_gcm_decrypt(&key, &iv, &[], 16, &bad).is_none());
        assert!(aes_gcm_decrypt(&key, &iv, b"aad", 16, &ct).is_none());
        // A truncated tag authenticates over the same bytes, just fewer of them.
        let t4 = aes_gcm_encrypt(&key, &iv, b"aad", 4, &pt).unwrap();
        assert_eq!(t4.len(), pt.len() + 4);
        assert_eq!(aes_gcm_decrypt(&key, &iv, b"aad", 4, &t4).unwrap(), pt);
        assert!(aes_gcm_encrypt(&key, &[], &[], 16, &pt).is_none());
    }

    #[test]
    fn aes_kw_matches_rfc3394() {
        let data = hex_bytes("00112233445566778899aabbccddeeff");
        for (kek_hex, want) in [
            ("000102030405060708090a0b0c0d0e0f", "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5"),                 // 4.1
            ("000102030405060708090a0b0c0d0e0f1011121314151617", "96778b25ae6ca435f92b5b97c050aed2468ab8a17ad84e5d"), // 4.2
            ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "64e8c3f9ce0f5ba263e9777905818a2a93c8191e7d6e8ae7"), // 4.3
        ] {
            let kek = hex_bytes(kek_hex);
            let w = aes_kw_wrap(&kek, &data).unwrap();
            assert_eq!(hex(&w), want, "kek {kek_hex}");
            assert_eq!(aes_kw_unwrap(&kek, &w).unwrap(), data);
            // The integrity check is the whole point of KW: one flipped bit must not unwrap.
            let mut bad = w.clone();
            bad[0] ^= 1;
            assert!(aes_kw_unwrap(&kek, &bad).is_none());
        }
        // Fewer than two semiblocks in, or three out, is not a key wrap at all.
        assert!(aes_kw_wrap(&[0u8; 16], &[0u8; 8]).is_none());
        assert!(aes_kw_wrap(&[0u8; 16], &[0u8; 20]).is_none());
        assert!(aes_kw_unwrap(&[0u8; 16], &[0u8; 16]).is_none());
    }

    #[test]
    fn b64url_is_unpadded_and_refuses_padding() {
        assert_eq!(b64url_encode(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]), "AAECAwQFBgcICQoLDA0ODw");
        assert_eq!(b64url_decode("AAECAwQFBgcICQoLDA0ODw").unwrap(), (0..16).collect::<Vec<u8>>());
        assert!(b64url_decode("AAAAAAAAAAAAAAAAAAAAAA==").is_none()); // Chrome refuses padding
        assert!(b64url_decode("!!!!").is_none());
        assert!(b64url_decode("+/+/").is_none()); // the standard alphabet is not base64url
    }

    #[test]
    fn x25519_agreement_is_symmetric() {
        let (a_priv, a_pub) = x25519_keypair();
        let (b_priv, b_pub) = x25519_keypair();
        let ab = x25519_derive(&a_priv, &b_pub, 256).unwrap();
        let ba = x25519_derive(&b_priv, &a_pub, 256).unwrap();
        assert_eq!(ab, ba);
        assert_eq!(ab.len(), 32);
        assert_eq!(x25519_derive(&a_priv, &b_pub, 128).unwrap().len(), 16);
        assert!(x25519_derive(&[0u8; 5], &b_pub, 256).is_none());
    }

    #[test]
    fn helpers() {
        assert!(is_supported_hash("SHA-256") && !is_supported_hash("SHA-3"));
        // 192 is OUT, and that is a reading of Chrome 150 rather than of the spec: it answers
        // `192-bit AES keys are not supported` to generateKey, importKey("raw") and importKey("jwk").
        assert!(aes_key_len_ok(128) && aes_key_len_ok(256) && !aes_key_len_ok(192) && !aes_key_len_ok(200));
        assert!(gcm_tag_len_ok(32) && gcm_tag_len_ok(104) && gcm_tag_len_ok(128));
        assert!(!gcm_tag_len_ok(0) && !gcm_tag_len_ok(8) && !gcm_tag_len_ok(127) && !gcm_tag_len_ok(256));
        assert!(usages_ok(&["sign".into()], &["sign", "verify"]));
        assert!(!usages_ok(&[], &["sign"]));
        assert!(!usages_ok(&["wrapKey".into()], &["sign", "verify"]));
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
    }
}
