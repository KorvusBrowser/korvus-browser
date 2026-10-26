//! `modules/crypto/kdf.rs` -- the two key-derivation functions WebCrypto names, PBKDF2 (RFC 8018 §5.2)
//! and HKDF (RFC 5869), over the same RustCrypto HMAC the `HMAC` algorithm signs with.
//!
//! No v8 here: these are the byte functions, unit-tested against the RFC vectors and against Chrome's
//! own output for the fixed inputs `SubtleCrypto`'s live test uses. The WebCrypto half -- parameter
//! normalization, the key checks and Chrome's error messages -- is `subtle_crypto.rs`.

use hmac::{Hmac, Mac};

/// The output length in bytes of `hash`, or `None` for a hash this engine does not implement.
pub(crate) fn hash_len(hash: &str) -> Option<usize> {
    Some(match hash {
        "SHA-1" => 20,
        "SHA-256" => 32,
        "SHA-384" => 48,
        "SHA-512" => 64,
        _ => return None,
    })
}

fn pbkdf2_with<M: Mac + Clone>(prf: M, salt: &[u8], iterations: u32, out_len: usize) -> Vec<u8> {
    let h_len = <M as hmac::digest::OutputSizeUser>::output_size();
    let mut out = Vec::with_capacity(out_len.div_ceil(h_len) * h_len);
    let mut block: u32 = 1;
    while out.len() < out_len {
        let mut m = prf.clone();
        m.update(salt);
        m.update(&block.to_be_bytes());
        let mut u = m.finalize().into_bytes().to_vec();
        let mut t = u.clone();
        for _ in 1..iterations {
            let mut m = prf.clone();
            m.update(&u);
            u = m.finalize().into_bytes().to_vec();
            for (a, b) in t.iter_mut().zip(&u) {
                *a ^= b;
            }
        }
        out.extend_from_slice(&t);
        block = block.wrapping_add(1);
    }
    out.truncate(out_len);
    out
}

/// PBKDF2-HMAC-`hash`: `out_len` bytes from `password`, `salt` and `iterations` (which the caller has
/// already required to be non-zero). `None` for an unsupported hash.
pub(crate) fn pbkdf2(hash: &str, password: &[u8], salt: &[u8], iterations: u32, out_len: usize) -> Option<Vec<u8>> {
    macro_rules! run {
        ($d:ty) => {
            pbkdf2_with(<Hmac<$d> as Mac>::new_from_slice(password).expect("hmac takes any key length"), salt, iterations, out_len)
        };
    }
    Some(match hash {
        "SHA-1" => run!(sha1::Sha1),
        "SHA-256" => run!(sha2::Sha256),
        "SHA-384" => run!(sha2::Sha384),
        "SHA-512" => run!(sha2::Sha512),
        _ => return None,
    })
}

/// HKDF-`hash` (extract, then expand): `out_len` bytes, at most 255 x the hash length (the caller
/// rejects longer with Chrome's message). An empty `salt` is the RFC's string of `HashLen` zeros.
pub(crate) fn hkdf(hash: &str, key: &[u8], salt: &[u8], info: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let h_len = hash_len(hash)?;
    if out_len > 255 * h_len {
        return None;
    }
    let zeros = vec![0u8; h_len];
    let salt = if salt.is_empty() { &zeros[..] } else { salt };
    let prk = crate::crypto::hmac_sign(hash, salt, key)?;
    let mut out = Vec::with_capacity(out_len + h_len);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < out_len {
        let mut msg = Vec::with_capacity(prev.len() + info.len() + 1);
        msg.extend_from_slice(&prev);
        msg.extend_from_slice(info);
        msg.push(counter);
        prev = crate::crypto::hmac_sign(hash, &prk, &msg)?;
        out.extend_from_slice(&prev);
        counter = counter.wrapping_add(1);
    }
    out.truncate(out_len);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn pbkdf2_matches_rfc6070() {
        assert_eq!(hex(&pbkdf2("SHA-1", b"password", b"salt", 1, 20).unwrap()), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
        assert_eq!(hex(&pbkdf2("SHA-1", b"password", b"salt", 2, 20).unwrap()), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        assert_eq!(hex(&pbkdf2("SHA-1", b"password", b"salt", 4096, 20).unwrap()), "4b007901b765489abead49d926f721d065a429c1");
        assert_eq!(
            hex(&pbkdf2("SHA-1", b"passwordPASSWORDpassword", b"saltSALTsaltSALTsaltSALTsaltSALTsalt", 4096, 25).unwrap()),
            "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038"
        );
    }

    #[test]
    fn pbkdf2_matches_chrome_150() {
        // `deriveBits({name:"PBKDF2", salt:"salt", iterations:1000, hash:"SHA-256"}, importKey("raw",
        // "password"), 256)` in google-chrome 150.0.7871.46.
        assert_eq!(
            hex(&pbkdf2("SHA-256", b"password", b"salt", 1000, 32).unwrap()),
            "632c2812e46d4604102ba7618e9d6d7d2f8128f6266b4a03264d2a0460b7dcb3"
        );
        assert_eq!(pbkdf2("SHA-256", b"password", b"salt", 1, 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hkdf_matches_rfc5869() {
        // Test case 1 (SHA-256).
        let okm = hkdf(
            "SHA-256",
            &unhex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"),
            &unhex("000102030405060708090a0b0c"),
            &unhex("f0f1f2f3f4f5f6f7f8f9"),
            42,
        )
        .unwrap();
        assert_eq!(hex(&okm), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865");
        // Test case 3 (zero-length salt and info).
        let okm = hkdf("SHA-256", &unhex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"), &[], &[], 42).unwrap();
        assert_eq!(hex(&okm), "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8");
    }

    #[test]
    fn hkdf_matches_chrome_150_and_its_length_cap() {
        // `deriveBits({name:"HKDF", salt:"salt", info:"info", hash:"SHA-256"}, importKey("raw",
        // "secret-key"), 256)` in google-chrome 150.0.7871.46.
        assert_eq!(
            hex(&hkdf("SHA-256", b"secret-key", b"salt", b"info", 32).unwrap()),
            "274d210b61ee4e53a0b98935371805a5ace78d1d1a1481da5c5209f39b19b32f"
        );
        assert_eq!(hkdf("SHA-256", b"k", b"s", b"i", 255 * 32).map(|v| v.len()), Some(255 * 32));
        assert_eq!(hkdf("SHA-256", b"k", b"s", b"i", 255 * 32 + 1), None);
    }
}
