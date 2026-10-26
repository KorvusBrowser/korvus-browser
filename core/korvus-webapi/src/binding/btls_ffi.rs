//! `binding/btls_ffi.rs` — the four BoringSSL entry points `btls` 0.5.6 does not wrap, and the
//! only `unsafe` in this crate's WebCrypto layer.
//!
//! WHY THIS FILE EXISTS. Two jobs, both of which `btls`'s safe API gets subtly wrong for WebCrypto.
//!
//! **1. Key parsing that refuses trailing data.** `btls` parses DER with the `d2i_*` family
//! (`PKey::public_key_from_der` is `d2i_PUBKEY`). `d2i_*` parses a prefix and reports how far it
//! got; it does **not** insist that the caller's buffer is used up. Chrome's WebCrypto does:
//!
//! ```text
//! importKey spki trailing byte = THREW DataError:        (Chrome 150.0.7871.46, measured)
//! ```
//!
//! and `d2i_PUBKEY` on those same bytes returns a KEY. That is not a judgement call — it was found
//! by a red unit test (`asym::tests::malformed_der_is_refused_every_way_it_can_be_malformed`)
//! asserting Chrome's line and failing. The parser that matches is BoringSSL's own
//! `EVP_parse_public_key` over a `CBS`, with the caller checking `CBS` is exhausted — which is
//! precisely what Chrome's `blink::WebCryptoImpl` does. `btls` exposes no `CBS` route at all.
//!
//! **2. RSA-OAEP with a chosen digest.** WebCrypto takes the OAEP digest and the MGF1 digest from
//! the KEY's `hash` and the label from `RsaOaepParams.label`. BoringSSL configures all three on an
//! `EVP_PKEY_CTX`, and `btls` has **no `Encrypter`/`Decrypter`** — rust-openssl's `encrypt` module
//! is simply absent — and keeps `extern crate btls_sys as ffi` PRIVATE, re-exporting only
//! `ffi::init`. The one OAEP door its safe API opens, `Rsa::public_encrypt(.., PKCS1_OAEP)`,
//! hardcodes BoringSSL's default OAEP digest, which is **SHA-1**, and takes no label. A page that
//! asks for `hash:"SHA-256"` and silently gets SHA-1 is the worst outcome available here: a
//! rejection is visible, a wrong ciphertext is not.
//!
//! MEASURED, not assumed, against the `btls` this workspace pins (`btls`/`btls-sys` 0.5.6 at git
//! rev `10ac71fc`, `[patch.crates-io]` in the root `Cargo.toml`) and the BoringSSL it vendors:
//!
//!   * `btls/src/rsa.rs` declares `Padding::PKCS1_OAEP` and no OAEP digest setter.
//!   * `grep -rl 'struct (Encrypter|Decrypter)' btls/src` → nothing.
//!   * `btls/src/lib.rs:98` is `extern crate btls_sys as ffi;` — private — and line 108 re-exports
//!     `ffi::init` alone.
//!   * `btls/src/pkey.rs:438,531` are `from_der!` over `d2i_AutoPrivateKey` / `d2i_PUBKEY`; no
//!     `EVP_parse_*` appears anywhere in `btls/src`.
//!   * every symbol used below IS in the bindings `btls-sys` generates for that BoringSSL
//!     (`target/*/build/btls-sys-*/out/bindings.rs`, 21,204 lines). `CBS_init` is `static inline`
//!     in the header so bindgen cannot see it, which is why `btls_sys::cbs_init` exists.
//!
//! So this file reaches `btls-sys` — the SAME crate `btls` itself is built on, already in
//! `Cargo.lock` and already linked into this binary — and does the four things `btls` cannot
//! express. It adds no crate to the graph and no C to the build.
//!
//! WHAT BREAKS IT AND HOW YOU FIND OUT. These are `btls_sys`'s own declarations, not hand-written
//! `extern` blocks, so a `btls-sys` bump that renames or re-signs any of them **fails the build, by
//! name**. The day `btls` grows a `CBS` parser or an `Encrypter`/`Decrypter`, delete the
//! corresponding function here and call that instead: the semantics are BoringSSL's either way.
//!
//! WHY THE FFI IS CONFINED HERE. `btls` not wrapping these is a fact about the wrapper, not a
//! licence to scatter `unsafe` through the WebCrypto layer. `crypto/asym.rs` uses the safe `btls`
//! API for everything else and calls the four safe functions below.

use btls::pkey::{PKey, Private, Public};
use foreign_types::ForeignType;
use std::ptr;

/// Parse an `spki` (`SubjectPublicKeyInfo`) and REFUSE any trailing byte.
///
/// `None` is the whole of Chrome's empty-message `DataError`: a truncated key, a trailing byte, a
/// PKCS#8 handed in as an SPKI and a five-byte `SEQUENCE` are all indistinguishable to a page.
pub fn parse_spki(der: &[u8]) -> Option<PKey<Public>> {
    if der.is_empty() {
        return None;
    }
    // SAFETY: `cbs` borrows `der` for the duration of the call and BoringSSL only reads it.
    // `EVP_parse_public_key` returns an owned `EVP_PKEY` or null, and advances `cbs` past what it
    // consumed; `PKey::from_ptr` takes that ownership exactly once.
    unsafe {
        btls_sys::init();
        let mut cbs = btls_sys::cbs_init(der);
        let p = btls_sys::EVP_parse_public_key(&mut cbs);
        if p.is_null() {
            return None;
        }
        let key = PKey::<Public>::from_ptr(p);
        // THE POINT OF THIS FUNCTION. `d2i_PUBKEY` would have stopped here and returned the key.
        if cbs.len != 0 {
            return None;
        }
        Some(key)
    }
}

/// Parse a `pkcs8` (`PrivateKeyInfo`) and REFUSE any trailing byte. Same rule.
pub fn parse_pkcs8(der: &[u8]) -> Option<PKey<Private>> {
    if der.is_empty() {
        return None;
    }
    // SAFETY: as `parse_spki`.
    unsafe {
        btls_sys::init();
        let mut cbs = btls_sys::cbs_init(der);
        let p = btls_sys::EVP_parse_private_key(&mut cbs);
        if p.is_null() {
            return None;
        }
        let key = PKey::<Private>::from_ptr(p);
        if cbs.len != 0 {
            return None;
        }
        Some(key)
    }
}

/// BoringSSL's `EVP_MD` for a WebCrypto hash name, or `None` for one it does not name.
fn md(hash: &str) -> Option<*const btls_sys::EVP_MD> {
    // SAFETY: each of these returns a pointer to a `static const EVP_MD` with program lifetime.
    unsafe {
        Some(match hash {
            "SHA-1" => btls_sys::EVP_sha1(),
            "SHA-256" => btls_sys::EVP_sha256(),
            "SHA-384" => btls_sys::EVP_sha384(),
            "SHA-512" => btls_sys::EVP_sha512(),
            _ => return None,
        })
    }
}

/// An `EVP_PKEY_CTX`, freed on drop however the caller leaves.
struct Ctx(*mut btls_sys::EVP_PKEY_CTX);

impl Drop for Ctx {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` came from `EVP_PKEY_CTX_new` and is freed exactly once.
            unsafe { btls_sys::EVP_PKEY_CTX_free(self.0) };
        }
    }
}

impl Ctx {
    /// Set padding, both digests and the label. `label` is copied into an `OPENSSL_malloc`
    /// allocation because `EVP_PKEY_CTX_set0_rsa_oaep_label` TAKES OWNERSHIP of it (the `set0`
    /// suffix is BoringSSL's word for that) and frees it with the context.
    fn configure(&self, hash: &str, label: &[u8]) -> Option<()> {
        let digest = md(hash)?;
        // SAFETY: `self.0` is a live encrypt/decrypt-initialized context and `digest` has program
        // lifetime. Each call returns 1 on success, which is checked.
        unsafe {
            if btls_sys::EVP_PKEY_CTX_set_rsa_padding(self.0, btls_sys::RSA_PKCS1_OAEP_PADDING) != 1 {
                return None;
            }
            if btls_sys::EVP_PKEY_CTX_set_rsa_oaep_md(self.0, digest) != 1 {
                return None;
            }
            if btls_sys::EVP_PKEY_CTX_set_rsa_mgf1_md(self.0, digest) != 1 {
                return None;
            }
            if !label.is_empty() {
                let buf = btls_sys::OPENSSL_malloc(label.len()) as *mut u8;
                if buf.is_null() {
                    return None;
                }
                ptr::copy_nonoverlapping(label.as_ptr(), buf, label.len());
                if btls_sys::EVP_PKEY_CTX_set0_rsa_oaep_label(self.0, buf, label.len()) != 1 {
                    // Ownership did NOT transfer on failure, so this side still owns the buffer.
                    btls_sys::OPENSSL_free(buf as *mut std::ffi::c_void);
                    return None;
                }
            }
        }
        Some(())
    }
}

/// `RSA-OAEP` encrypt with `hash` as both the OAEP and the MGF1 digest. `None` on any refusal —
/// BoringSSL's reasons (a message too long for the modulus, a digest the key cannot carry) are the
/// ones Chrome reports as a bare `OperationError`, so a caller need not tell them apart.
pub fn oaep_encrypt(key: &PKey<Public>, hash: &str, label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: `key.as_ptr()` is live for this call; the context borrows it and is freed first.
    unsafe {
        let ctx = Ctx(btls_sys::EVP_PKEY_CTX_new(key.as_ptr(), ptr::null_mut()));
        if ctx.0.is_null() || btls_sys::EVP_PKEY_encrypt_init(ctx.0) != 1 {
            return None;
        }
        ctx.configure(hash, label)?;
        let mut len = 0usize;
        if btls_sys::EVP_PKEY_encrypt(ctx.0, ptr::null_mut(), &mut len, data.as_ptr(), data.len()) != 1 {
            return None;
        }
        let mut out = vec![0u8; len];
        if btls_sys::EVP_PKEY_encrypt(ctx.0, out.as_mut_ptr(), &mut len, data.as_ptr(), data.len()) != 1 {
            return None;
        }
        out.truncate(len);
        Some(out)
    }
}

/// `RSA-OAEP` decrypt. `None` covers a wrong label, a tampered ciphertext and a wrong-size input
/// alike, which is exactly the resolution Chrome's empty-message `OperationError` offers a page.
pub fn oaep_decrypt(key: &PKey<Private>, hash: &str, label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: as `oaep_encrypt`.
    unsafe {
        let ctx = Ctx(btls_sys::EVP_PKEY_CTX_new(key.as_ptr(), ptr::null_mut()));
        if ctx.0.is_null() || btls_sys::EVP_PKEY_decrypt_init(ctx.0) != 1 {
            return None;
        }
        ctx.configure(hash, label)?;
        let mut len = 0usize;
        if btls_sys::EVP_PKEY_decrypt(ctx.0, ptr::null_mut(), &mut len, data.as_ptr(), data.len()) != 1 {
            return None;
        }
        let mut out = vec![0u8; len];
        if btls_sys::EVP_PKEY_decrypt(ctx.0, out.as_mut_ptr(), &mut len, data.as_ptr(), data.len()) != 1 {
            return None;
        }
        out.truncate(len);
        Some(out)
    }
}
