//! `modules/crypto/subtle_crypto.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::modules::crypto::shared::{ScErr, crypto_ab, crypto_bytes, crypto_num, crypto_obj, sc_settle};
use crate::binding::protos::{get_prop, str_val, throw_type_error};
use crate::crypto as wc; // the crypto backend (src/crypto.rs)
use super::asym_keys; // RSA and elliptic-curve keys
use super::crypto_key::CryptoKey; // sibling interface (moved same batch)
use crate::install::live::method_row;
use crate::install::MemberDef;

/// The algorithm's name as Blink 150 reads an `AlgorithmIdentifier` (`ParseAlgorithmIdentifier`):
/// a string is the name; an object's `name` is asked for with `Has`, then read with `Get` (Blink's
/// legacy `Dictionary`), and made a string -- so `{name: 5}` names "5" (Unrecognized), and an object with no
/// `name` (or `undefined`) is "Algorithm: name: Missing or not a string"; any other value is made a string too
/// (the union's DOMString). korvus read `name` with `Get` alone and refused a non-string, in a sentence of its own.
fn algorithm_name(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, op: &str) -> Result<String, ScErr> {
    let missing = || ScErr::Type(sc_where(op, "Algorithm: name: Missing or not a string"));
    let named = match v8::Local::<v8::Object>::try_from(v) {
        Ok(o) => {
            let key = str_val(scope, "name");
            if !o.has(scope, key).unwrap_or(false) {
                return Err(missing());
            }
            match o.get(scope, key) {
                Some(n) if !n.is_undefined() => n,
                _ => return Err(missing()),
            }
        }
        Err(_) => v,
    };
    if named.is_symbol() {
        return Err(ScErr::Type(sc_where(op, "Cannot convert a Symbol value to a string")));
    }
    Ok(named.to_rust_string_lossy(scope))
}

/// The normalized (UPPERCASE) algorithm name of `v`, read as [`algorithm_name`] reads it.
fn crypto_algo_name(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, op: &str) -> Result<String, ScErr> {
    algorithm_name(scope, v, op).map(|n| n.to_ascii_uppercase())
}
fn crypto_algo_obj<'a>(v: v8::Local<'a, v8::Value>) -> Option<v8::Local<'a, v8::Object>> { v8::Local::<v8::Object>::try_from(v).ok() }
/// The normalized (UPPERCASE) hash of an algorithm's `hash` member (string or `{name}`); "" if absent.
pub(super) fn crypto_hash_of(scope: &mut v8::PinScope, o: Option<v8::Local<v8::Object>>) -> String {
    let h = match o.and_then(|o| get_prop(scope, o, "hash")) { Some(h) => h, None => return String::new() };
    if h.is_string() { return h.to_rust_string_lossy(scope).to_ascii_uppercase(); }
    if let Ok(ho) = v8::Local::<v8::Object>::try_from(h) {
        if let Some(n) = get_prop(scope, ho, "name").filter(|n| n.is_string()) { return n.to_rust_string_lossy(scope).to_ascii_uppercase(); }
    }
    String::new()
}
fn crypto_usages(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<String> {
    match v8::Local::<v8::Array>::try_from(v) {
        Ok(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).filter(|x| x.is_string()).map(|x| x.to_rust_string_lossy(scope)).collect(),
        Err(_) => Vec::new(),
    }
}

/// WebCrypto's usage order. A key's `usages` is the requested set in THIS order, not the caller's --
/// Chrome 150, measured: `importKey("raw", k, "PBKDF2", false, ["deriveBits","deriveKey"]).usages` is
/// `["deriveKey","deriveBits"]` and an HMAC key imported for `["verify","sign"]` reads
/// `["sign","verify"]`.
const USAGE_ORDER: [&str; 8] = ["encrypt", "decrypt", "sign", "verify", "deriveKey", "deriveBits", "wrapKey", "unwrapKey"];

pub(super) fn canonical_usages(usages: Vec<String>) -> Vec<String> {
    let mut known: Vec<String> = USAGE_ORDER.iter().filter(|u| usages.iter().any(|x| x == *u)).map(|u| u.to_string()).collect();
    for u in usages {
        if !USAGE_ORDER.contains(&u.as_str()) && !known.contains(&u) {
            known.push(u);
        }
    }
    known
}

// ── the algorithm registry, and `normalize an algorithm` ───────────────────────────
//
// Until this lane, `encrypt` admitted the single name `AES-CTR` and every other name -- registered or
// invented -- got one string, `unsupported encrypt algorithm`, which no browser has ever emitted. Six
// live parity rows died on it and their paired oracle emitted it zero times.
//
// Chrome 150 distinguishes THREE outcomes and this table is the reading, not the spec
// (`webcrypto-probe.chrome-150.txt`, section `registry`):
//
//   * a name no registry has      -> NotSupportedError  `...: Algorithm: Unrecognized name`
//   * a registered name that does
//     not do this operation       -> NotSupportedError  `...: <Canonical>: Unsupported operation: <op>`
//   * a registered name that does -> on to the key checks
//
// The set below is what Chrome 150 answers `Unsupported operation` for -- `Ed25519` and `X25519` are
// in, `Ed448`, `X448`, `AES-CFB`, `AES-CMAC`, `RSAES-PKCS1-v1_5`, `CONCAT`, `DH`, `SHA-3`,
// `X963-KDF`, `ML-DSA-44` and `ML-KEM-768` are all `Unrecognized name`. Lookup is
// ASCII-case-insensitive and the CANONICAL spelling is what comes back in the message: `hmac` is
// echoed as `HMAC` and `ed25519` as `Ed25519`, measured, which is why this is a table of canonical
// strings and not a set of uppercase ones.
const ALGORITHMS: [&str; 18] = [
    "AES-CBC", "AES-CTR", "AES-GCM", "AES-KW", "ECDH", "ECDSA", "Ed25519", "HKDF", "HMAC",
    "PBKDF2", "RSA-OAEP", "RSA-PSS", "RSASSA-PKCS1-v1_5", "SHA-1", "SHA-256", "SHA-384",
    "SHA-512", "X25519",
];

/// The canonical spelling of `name`, or `None` when no registry has it.
fn canonical_algorithm(name: &str) -> Option<&'static str> {
    ALGORITHMS.iter().copied().find(|a| a.eq_ignore_ascii_case(name))
}

/// Which registered algorithms define each SubtleCrypto operation. Only the operations whose gate
/// this lane owns are listed; the others keep their own checks until a lane measures them.
fn algorithm_supports(canonical: &str, op: &str) -> bool {
    let table: &[&str] = match op {
        "encrypt" | "decrypt" => &["AES-CBC", "AES-CTR", "AES-GCM", "RSA-OAEP"],
        "wrapKey" | "unwrapKey" => &["AES-CBC", "AES-CTR", "AES-GCM", "AES-KW", "RSA-OAEP"],
        "digest" => &["SHA-1", "SHA-256", "SHA-384", "SHA-512"],
        "importKey" | "exportKey" => &[
            "AES-CBC", "AES-CTR", "AES-GCM", "AES-KW", "ECDH", "ECDSA", "Ed25519", "HKDF", "HMAC",
            "PBKDF2", "RSA-OAEP", "RSA-PSS", "RSASSA-PKCS1-v1_5", "X25519",
        ],
        _ => return true,
    };
    table.contains(&canonical)
}

/// `Failed to execute '<op>' on 'SubtleCrypto': <rest>`, the prefix Chrome puts on every error that
/// comes out of argument and algorithm processing (but NOT on the ones the operation itself raises:
/// `key.algorithm does not match that of operation` and `Unsupported import key format for
/// algorithm` carry no prefix, measured).
pub(super) fn sc_where(op: &str, rest: &str) -> String {
    format!("Failed to execute '{op}' on 'SubtleCrypto': {rest}")
}

/// WebCrypto's "normalize an algorithm" for `op`: the canonical name and the algorithm object.
fn sc_normalize<'a>(scope: &mut v8::PinScope, v: v8::Local<'a, v8::Value>, op: &str) -> Result<(&'static str, Option<v8::Local<'a, v8::Object>>), ScErr> {
    sc_normalize_for(scope, v, op, op)
}

/// The same, for the two arguments whose OPERATION is not the method they were passed to.
///
/// `unwrapKey`'s fifth argument is the algorithm of the key being CREATED, so the spec normalizes it
/// "with op `importKey`" -- while the error still says `Failed to execute 'unwrapKey'`. Collapsing
/// the two rejected every legal `unwrapKey` into an HMAC or AES-KW key with
/// `HMAC: Unsupported operation: unwrapKey`, and 17 subtests of
/// `wrapKey_unwrapKey/wrapKey_unwrapKey.https.any.js` said so. `method` names the message, `op`
/// chooses the table.
fn sc_normalize_for<'a>(scope: &mut v8::PinScope, v: v8::Local<'a, v8::Value>, method: &str, op: &str) -> Result<(&'static str, Option<v8::Local<'a, v8::Object>>), ScErr> {
    let raw = algorithm_name(scope, v, method)?;
    let Some(canonical) = canonical_algorithm(&raw) else {
        return Err(ScErr::Dom("NotSupportedError".into(), sc_where(method, "Algorithm: Unrecognized name")));
    };
    if !algorithm_supports(canonical, op) {
        return Err(ScErr::Dom("NotSupportedError".into(), sc_where(method, &format!("{canonical}: Unsupported operation: {op}"))));
    }
    Ok((canonical, crypto_algo_obj(v)))
}

/// A required `BufferSource` member of an algorithm params dictionary.
fn sc_buffer_member(scope: &mut v8::PinScope, o: Option<v8::Local<v8::Object>>, op: &str, dict: &str, member: &str) -> Result<Vec<u8>, ScErr> {
    let v = o.and_then(|o| get_prop(scope, o, member)).filter(|v| !v.is_undefined());
    let Some(v) = v else {
        return Err(ScErr::Type(sc_where(op, &format!("{dict}: {member}: Missing required property"))));
    };
    crypto_bytes(scope, v).ok_or_else(|| ScErr::Type(sc_where(op, &format!("{dict}: {member}: Not a BufferSource"))))
}

/// WebIDL `[EnforceRange] octet` -- `AesCtrParams.length` and `AeadParams.tagLength`.
fn sc_octet_member(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, op: &str, dict: &str, member: &str) -> Result<i64, ScErr> {
    let n = v.number_value(scope).unwrap_or(f64::NAN);
    let t = if n.is_finite() { n.trunc() } else { f64::NAN };
    if !t.is_finite() || !(0.0..=255.0).contains(&t) {
        return Err(ScErr::Type(sc_where(op, &format!("{dict}: {member}: Outside of numeric range"))));
    }
    Ok(t as i64)
}

/// The parameters of the three AES modes this engine computes, already validated as far as their
/// DICTIONARY goes. Everything that is an `OperationError` rather than a `TypeError` -- an IV of the
/// wrong length, a counter width outside 1..=128, a tag length off the list -- is left to the mode,
/// because Chrome raises those after the key checks and this struct is built before them.
enum AesParams {
    Cbc { iv: Vec<u8> },
    Ctr { counter: Vec<u8>, length: i64 },
    Gcm { iv: Vec<u8>, aad: Vec<u8>, tag_bits: i64 },
}

impl AesParams {
    fn read(scope: &mut v8::PinScope, name: &str, o: Option<v8::Local<v8::Object>>, op: &str) -> Result<AesParams, ScErr> {
        Ok(match name {
            "AES-CBC" => AesParams::Cbc { iv: sc_buffer_member(scope, o, op, "AesCbcParams", "iv")? },
            "AES-CTR" => {
                let counter = sc_buffer_member(scope, o, op, "AesCtrParams", "counter")?;
                let lv = o.and_then(|o| get_prop(scope, o, "length")).filter(|v| !v.is_undefined());
                let Some(lv) = lv else {
                    return Err(ScErr::Type(sc_where(op, "AesCtrParams: length: Missing required property")));
                };
                AesParams::Ctr { counter, length: sc_octet_member(scope, lv, op, "AesCtrParams", "length")? }
            }
            "AES-GCM" => {
                let iv = sc_buffer_member(scope, o, op, "AeadParams", "iv")?;
                let adv = o.and_then(|o| get_prop(scope, o, "additionalData")).filter(|v| !v.is_undefined() && !v.is_null());
                let aad = match adv {
                    Some(v) => crypto_bytes(scope, v).ok_or_else(|| ScErr::Type(sc_where(op, "AeadParams: additionalData: Not a BufferSource")))?,
                    None => Vec::new(),
                };
                let tv = o.and_then(|o| get_prop(scope, o, "tagLength")).filter(|v| !v.is_undefined() && !v.is_null());
                let tag_bits = match tv {
                    Some(v) => sc_octet_member(scope, v, op, "AeadParams", "tagLength")?,
                    None => 128,
                };
                AesParams::Gcm { iv, aad, tag_bits }
            }
            // `RSA-OAEP` normalizes for encrypt and reaches here, but no key this engine can mint
            // carries `algorithm.name == "RSA-OAEP"`, so the key check below refuses it first --
            // which is exactly what Chrome answers for an RSA-OAEP call with a secret key.
            _ => AesParams::Cbc { iv: Vec::new() },
        })
    }
}

/// Run the mode. The messages are Chrome 150's, measured: the empty-string `OperationError` is not
/// an oversight in this file, it is what Chrome sends for a padding, tag or integrity failure.
fn sc_aes_run(params: &AesParams, secret: &[u8], data: &[u8], decrypting: bool) -> Result<Vec<u8>, ScErr> {
    let op_err = |m: &str| ScErr::Dom("OperationError".into(), m.to_string());
    match params {
        AesParams::Cbc { iv } => {
            if iv.len() != 16 {
                return Err(op_err("The \"iv\" has an unexpected length -- must be 16 bytes"));
            }
            let out = if decrypting { wc::aes_cbc_decrypt(secret, iv, data) } else { wc::aes_cbc_encrypt(secret, iv, data) };
            out.ok_or_else(|| op_err(""))
        }
        AesParams::Ctr { counter, length } => {
            if counter.len() != 16 {
                return Err(op_err("The \"counter\" has an unexpected length -- must be 16 bytes"));
            }
            if !(1..=128).contains(length) {
                return Err(op_err("The \"length\" member must be >= 1 and <= 128"));
            }
            // A message longer than the counter space is a DataError, NOT an OperationError. Chrome
            // 150, measured: `The input is too large for the counter length.` an agent-browser reference fixture
            // beside this file asserted OperationError and was wrong; it is corrected in this commit.
            wc::aes_ctr(secret, counter, *length as i32, data)
                .ok_or_else(|| ScErr::Dom("DataError".into(), "The input is too large for the counter length.".into()))
        }
        AesParams::Gcm { iv, aad, tag_bits } => {
            if !wc::gcm_tag_len_ok(*tag_bits) {
                return Err(op_err("The tag length is invalid: Must be 32, 64, 96, 104, 112, 120, or 128 bits"));
            }
            let tag_bytes = (*tag_bits / 8) as usize;
            if decrypting {
                if data.len() < tag_bytes {
                    return Err(op_err("The provided data is too small"));
                }
                wc::aes_gcm_decrypt(secret, iv, aad, tag_bytes, data).ok_or_else(|| op_err(""))
            } else {
                wc::aes_gcm_encrypt(secret, iv, aad, tag_bytes, data).ok_or_else(|| op_err(""))
            }
        }
    }
}

pub(super) fn make_crypto_key<'s>(scope: &mut v8::PinScope<'s, '_>, key_type: &str, algorithm: v8::Local<v8::Value>, extractable: bool, usages: Vec<String>, secret: Vec<u8>, hash: &str) -> v8::Local<'s, v8::Value> {
    let usages = canonical_usages(usages);
    let k = CryptoKey {
        key_type: key_type.to_string(),
        algorithm: std::cell::RefCell::new(Some(v8::Global::new(scope, algorithm))),
        extractable: std::cell::Cell::new(extractable),
        usages: std::cell::RefCell::new(usages),
        secret: std::cell::RefCell::new(secret),
        hash: std::cell::RefCell::new(hash.to_string()),
    };
    let obj = crate::binding::class::mint(scope, k);
    // The one CryptoKey mint site: every key a page can reach carries the TABLE's prototype, so
    // `instanceof`, `constructor` and the four getters answer the interface the page can see (the
    // cppgc unwrap reads the wrapper tag, never the prototype, so the brand is untouched).
    if let Some(pr) = crate::binding::protos::shell_proto(scope, "CryptoKey") {
        obj.set_prototype(scope, pr.into());
    }
    obj.into()
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct SubtleCrypto {
    _p: u8,
}

pub(crate) trait SubtleCryptoBehavior {
   fn construct() -> SubtleCrypto where Self: Sized { stub!("SubtleCrypto::constructor") }
}

#[webapi(base, own)]
impl SubtleCrypto {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<SubtleCrypto, ::deno_error::JsErrorBox> { dispatch!("SubtleCrypto::constructor", <SubtleCrypto as SubtleCryptoBehavior>::construct()) }
}

impl SubtleCryptoBehavior for SubtleCrypto {
    fn construct() -> SubtleCrypto { SubtleCrypto { _p: 0 } }
}
/// A key's whole private state: its `type`, the bytes (a DER for an asymmetric key), its hash,
/// its usages and `extractable`. The asymmetric paths need all five and `crypto_key_secret` hands
/// back two, so this is the same unwrap with nothing dropped.
pub(super) fn crypto_key_parts(
    scope: &mut v8::PinScope,
    v: v8::Local<v8::Value>,
) -> Option<(String, Vec<u8>, String, Vec<String>, bool)> {
    let k = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, v)?;
    // Everything is cloned out before `k` drops; nothing borrowed from it escapes.
    let r: &CryptoKey = unsafe { &*(k.as_ref() as *const CryptoKey) };
    Some((
        r.key_type.clone(),
        r.secret.borrow().clone(),
        r.hash.borrow().clone(),
        r.usages.borrow().clone(),
        r.extractable.get(),
    ))
}

/// The ECDH peer argument: its `type` and its DER, or `None` when it is not a `CryptoKey` at all.
pub(super) fn asym_peer(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<(String, Vec<u8>)> {
    let (t, der, _, _, _) = crypto_key_parts(scope, v)?;
    Some((t, der))
}

fn crypto_key_secret(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<(Vec<u8>, String)> {
    let k = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, v)?;
    let r = unsafe { k.as_ref() };
    let secret = r.secret.borrow().clone();
    let hash = r.hash.borrow().clone();
    Some((secret, hash))
}

/// "If the [key]'s [[usages]] internal slot does not contain an entry that is `<op>`, then throw an
/// InvalidAccessError." Every SubtleCrypto operation has this step and NOT ONE of them had it here: a
/// key imported for `decrypt` would happily encrypt, and one imported for `encrypt` would wrap. Brave
/// 151 refuses both, with this exact message -- a page can read `e.message`, so the string is part of
/// the answer and not decoration.
///
/// Found while adding deriveKey/wrapKey/unwrapKey: the round-trip test passed here and threw in the
/// browser, and the reason was not the new members. It is applied to every operation that has a usage,
/// not only the three, because the check is the same four lines and a half-applied one is a worse
/// state than none -- it makes the surface inconsistent in a way nothing measures.
fn sc_require_usage(scope: &mut v8::PinScope, key: v8::Local<v8::Value>, usage: &str) -> Result<(), ScErr> {
    let Some(k) = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key) else {
        return Err(ScErr::Type("key is not a CryptoKey".into()));
    };
    let has = unsafe { k.as_ref() }.usages.borrow().iter().any(|u| u == usage);
    if has { Ok(()) } else {
        Err(ScErr::Dom("InvalidAccessError".into(), "key.usages does not permit this operation".into()))
    }
}

fn sc_digest_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    // normalized for `digest` as every other operation is: an unknown name "Algorithm: Unrecognized name", a
    // registered one that is no digest "<Canonical>: Unsupported operation: digest" (korvus: "unsupported digest
    // algorithm", a sentence no browser emits)
    let (canonical, _) = sc_normalize(scope, args.get(0), "digest")?;
    let name = canonical.to_ascii_uppercase();
    let data = crypto_bytes(scope, args.get(1)).ok_or_else(|| ScErr::Type("data is not a BufferSource".into()))?;
    let out = wc::digest(&name, &data).ok_or_else(|| ScErr::Dom("NotSupportedError".into(), sc_where("digest", &format!("{canonical}: Unsupported operation: digest"))))?;
    Ok(crypto_ab(scope, out))
}
fn cb_sc_digest(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_digest_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

fn sc_sign_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    sc_require_usage(scope, args.get(1), "sign")?;
    let (name, o) = sc_normalize(scope, args.get(0), "sign")?;
    if asym_keys::is_asym(name) {
        let (_t, der, hash, _u, _e) = crypto_key_parts(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
        let data = crypto_bytes(scope, args.get(2)).ok_or_else(|| ScErr::Type("data is not a BufferSource".into()))?;
        return asym_keys::sign(scope, name, o, &der, &hash, &data);
    }
    let name = name.to_string();
    if name != "HMAC" { return Err(ScErr::Dom("NotSupportedError".into(), "unsupported sign algorithm".into())); }
    let (secret, hash) = crypto_key_secret(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    let data = crypto_bytes(scope, args.get(2)).ok_or_else(|| ScErr::Type("data is not a BufferSource".into()))?;
    let sig = wc::hmac_sign(&hash, &secret, &data).ok_or_else(|| ScErr::Dom("NotSupportedError".into(), "unsupported hash".into()))?;
    Ok(crypto_ab(scope, sig))
}
fn cb_sc_sign(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_sign_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

fn sc_verify_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    sc_require_usage(scope, args.get(1), "verify")?;
    let (name, o) = sc_normalize(scope, args.get(0), "verify")?;
    if asym_keys::is_asym(name) {
        let (_t, der, hash, _u, _e) = crypto_key_parts(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
        let signature = crypto_bytes(scope, args.get(2)).ok_or_else(|| ScErr::Type("signature is not a BufferSource".into()))?;
        let data = crypto_bytes(scope, args.get(3)).ok_or_else(|| ScErr::Type("data is not a BufferSource".into()))?;
        return asym_keys::verify(scope, name, o, &der, &hash, &signature, &data);
    }
    let name = name.to_string();
    if name != "HMAC" { return Err(ScErr::Dom("NotSupportedError".into(), "unsupported verify algorithm".into())); }
    let (secret, hash) = crypto_key_secret(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    let signature = crypto_bytes(scope, args.get(2)).ok_or_else(|| ScErr::Type("signature is not a BufferSource".into()))?;
    let data = crypto_bytes(scope, args.get(3)).ok_or_else(|| ScErr::Type("data is not a BufferSource".into()))?;
    let expected = wc::hmac_sign(&hash, &secret, &data).ok_or_else(|| ScErr::Dom("NotSupportedError".into(), "unsupported hash".into()))?;
    Ok(v8::Boolean::new(scope, wc::ct_eq(&signature, &expected)).into())
}
fn cb_sc_verify(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_verify_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

/// `encrypt` and `decrypt`, in the order Chrome 150 runs their checks (measured -- the `order:` lines
/// of `webcrypto-probe.chrome-150.txt` exist to pin exactly this):
///
///   1. WebIDL argument conversion -- a TYPE check only: `key` is a `CryptoKey` and `data` is a
///      `BufferSource`. Nothing is copied here.
///   2. normalize the algorithm, which is the registry lookup, the per-operation check, and then the
///      params dictionary -- a missing `AesCbcParams.iv` beats a key with the wrong usages.
///   3. **THEN** copy `data`'s bytes. The order of 2 and 3 is observable and WPT asserts it: the
///      `... with altered plaintext during call` cases pass an algorithm whose `name` is a GETTER
///      that mutates the plaintext, and the `... with transferred plaintext during call` ones
///      transfer its buffer out from under the call. Copying before normalizing makes every one of
///      them read the wrong bytes -- 22 subtests of `encrypt_decrypt/aes_ctr.https.any.js`, which is
///      how this was caught, having been written the wrong way round first.
///   4. `key.algorithm.name` equals the normalized name, else `InvalidAccessError`. This step did not
///      exist here at all, so an AES-GCM key encrypted happily under AES-CBC parameters.
///   5. the key carries this usage.
///   6. run the mode.
///
/// `RSA-OAEP` skips step 2's `AesParams` -- its one member, `label`, is optional -- and then runs
/// steps 4, 5 and 6 exactly like the AES modes. Once it failed step 4 for every key
/// this engine could mint, because there was no RSA key to mint.
fn sc_encrypt_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, usage: &str) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = usage;
    let key = args.get(1);
    if deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key).is_none() {
        return Err(ScErr::Type(sc_where(op, "parameter 2 is not of type 'CryptoKey'.")));
    }
    let data_v = args.get(2);
    if !data_v.is_array_buffer() && !data_v.is_array_buffer_view() {
        return Err(ScErr::Type(sc_where(op, "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.")));
    }
    let (name, o) = sc_normalize(scope, args.get(0), op)?;
    // `RSA-OAEP` has no `AesParams` to read; its one member, `label`, is optional. Everything after
    // the params step is shared, including the order of the two key checks.
    if name == "RSA-OAEP" {
        let data = crypto_bytes(scope, data_v).unwrap_or_default();
        if key_algorithm_name(scope, key).as_deref() != Some(&name.to_ascii_uppercase()) {
            return Err(ScErr::Dom("InvalidAccessError".into(), "key.algorithm does not match that of operation".into()));
        }
        sc_require_usage(scope, key, usage)?;
        let (key_type, der, hash, _u, _e) = crypto_key_parts(scope, key).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
        return asym_keys::oaep(scope, o, key_type == "private", &der, &hash, &data, usage == "decrypt");
    }
    let params = AesParams::read(scope, name, o, op)?;
    // A detached buffer reads as zero bytes, which is the answer the transfer cases want.
    let data = crypto_bytes(scope, data_v).unwrap_or_default();
    if key_algorithm_name(scope, key).as_deref() != Some(&name.to_ascii_uppercase()) {
        return Err(ScErr::Dom("InvalidAccessError".into(), "key.algorithm does not match that of operation".into()));
    }
    sc_require_usage(scope, key, usage)?;
    let (secret, _h) = crypto_key_secret(scope, key).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    let out = sc_aes_run(&params, &secret, &data, usage == "decrypt")?;
    Ok(crypto_ab(scope, out))
}
fn cb_sc_encrypt(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_encrypt_impl(scope, &args, "encrypt"); let p = sc_settle(scope, r); rv.set(p); }
fn cb_sc_decrypt(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_encrypt_impl(scope, &args, "decrypt"); let p = sc_settle(scope, r); rv.set(p); }

fn sc_derive_bits_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    // `optional [EnforceRange] unsigned long? length = null` (webref `webcrypto.idl`), and WebIDL
    // converts arguments BEFORE the method body runs -- so the range is enforced ONCE, here, for
    // every algorithm, rather than four times with four different answers.
    //
    // WHAT IT WAS, AND WHY IT WAS A HANG AND NOT ONLY A WRONG ERROR (CONF P0c, 2026-09-21).
    // `kdf_derive` read the argument as `length.uint32_value(..)`, i.e. ToUint32, under a comment
    // that said "anything else ToUint32" -- a reading of the IDL that drops `[EnforceRange]`. So
    //
    //     crypto.subtle.deriveBits({name:"PBKDF2", iterations:100000, ...}, key, -8)
    //
    // became ToUint32(-8) = 4,294,967,288 bits = 512 MB of output: 16,777,215 PBKDF2 blocks at
    // 100,000 HMAC-SHA256 iterations each = 1.68 x 10^12 HMACs. That is unbounded in practice, and
    // it runs in Rust with NO JavaScript frame on the stack -- so the WPT runner's watchdog, which
    // works by `IsolateInterrupt::terminate()` unwinding JS frames, cannot reach it (the class is
    // documented at `tests/wpt.rs`'s watchdog comment, precedent `OPTIONS_LENGTH_CAP`).
    // One file therefore killed a whole 30-minute shard, `wpt-tiers.sh` reported INCOMPLETE, and
    // the `relevant` floor could not be banked at all.
    //
    // Isolated by running the 44 cases of `WebCryptoAPI/derive_bits_keys/derived_bits_length.https.any.js`
    // one at a time behind a SYNCHRONOUS XHR ping (console output does not survive the hang): 40
    // cases completed, the 41st -- `PBKDF2 len=-8` -- never returned. The same value on HKDF
    // answered `OperationError` in 1 ms, because HKDF has its own `255 * hash_len` cap that
    // happens to catch it; PBKDF2 has no such cap. Same defect, two outcomes, which is why the
    // fix belongs at the conversion and not at either algorithm.
    //
    // The four values WPT pins (`derived_bits_length_testcases.js`, `enforceRangeTestCases`) are
    // NaN, Infinity, -8 and 2**32+8, all `TypeError`. `enforce_range_u32` already answers exactly
    // that -- it was added for `HmacKeyGenParams.length` after the same class of defect aborted
    // the process on `length: 1e18`.
    //
    // RESIDUAL, NOT PAPERED OVER: a length that IS a valid unsigned long -- say 4,294,967,288 --
    // still asks PBKDF2 for 512 MB and still runs unbounded. No test in the corpus exercises it and
    // nothing here invents a cap for it; capping would be guessing at Chrome's behaviour rather
    // than measuring it, and `components/webcrypto` is not in this sparse checkout to read.
    let length_arg = args.get(2);
    if !length_arg.is_null_or_undefined() {
        enforce_range_u32(scope, length_arg, "length")?;
    }
    if let Ok(kdf) = crypto_algo_name(scope, args.get(0), "deriveBits") {
        if kdf == "PBKDF2" || kdf == "HKDF" {
            let length = args.get(2);
            let out = kdf_derive(scope, "deriveBits", &kdf, args.get(0), args.get(1), "deriveBits", length)?;
            return Ok(crypto_ab(scope, out));
        }
    }
    sc_require_usage(scope, args.get(1), "deriveBits")?;
    let name = crypto_algo_name(scope, args.get(0), "deriveBits")?;
    if name == "ECDH" {
        let o = crypto_algo_obj(args.get(0));
        let (_t, der, _h, _u, _e) = crypto_key_parts(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
        let out = asym_keys::ecdh_bits(scope, o, &der, args.get(2), "deriveBits")?;
        return Ok(crypto_ab(scope, out));
    }
    if name != "X25519" { return Err(ScErr::Dom("NotSupportedError".into(), "unsupported deriveBits algorithm".into())); }
    let o = crypto_algo_obj(args.get(0));
    let (private, _h) = crypto_key_secret(scope, args.get(1)).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    let pub_val = o.and_then(|o| get_prop(scope, o, "public")).ok_or_else(|| ScErr::Type("algorithm.public is required".into()))?;
    let (public, _h2) = crypto_key_secret(scope, pub_val).ok_or_else(|| ScErr::Type("algorithm.public is not a CryptoKey".into()))?;
    let bits = args.get(2).number_value(scope).unwrap_or(0.0) as i32;
    let out = wc::x25519_derive(&private, &public, bits).ok_or_else(|| ScErr::Dom("OperationError".into(), "X25519 derive failed".into()))?;
    Ok(crypto_ab(scope, out))
}
fn cb_sc_derive_bits(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_derive_bits_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

// ── key FORMATS: `raw` and `jwk` ───────────────────────────────────────────────────
//
// `importKey` admitted `"raw"` and nothing else, and the four other format gates beside it said the
// same. Five live parity rows died there and their paired oracle emitted the message zero times.
//
// `jwk` is the format the other four algorithms in this file were already able to carry: it is a
// JSON dictionary around the same secret bytes, so nothing about the CIPHERS had to change for it.
// `pkcs8` and `spki` carry no symmetric key at all -- every algorithm that accepts them is RSA or an
// elliptic curve -- so they are dispatched to `asym_keys` and refused here only for
// the secret algorithms, where every browser refuses them too.
//
// Every string below is Chrome 150's, measured (`webcrypto-probe.chrome-150.txt`, sections
// `importKey`, `jwk` and `exportKey`), including the typo in the `ext` message.

pub(super) const JWK_ALG_INCONSISTENT: &str = "The JWK \"alg\" member was inconsistent with that specified by the Web Crypto call";
pub(super) const JWK_SUPERSET: &str = ". The JWK usage must be a superset of those requested";

pub(super) fn data_err(m: &str) -> ScErr { ScErr::Dom("DataError".into(), m.to_string()) }

/// The four `KeyFormat` values, and Chrome's TypeError for anything else. NOT a WebIDL enum error:
/// Chrome 150 says `Invalid keyFormat argument: bogus`, which is what a page sees.
fn sc_key_format(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, op: &str) -> Result<String, ScErr> {
    let f = v.to_rust_string_lossy(scope);
    if matches!(f.as_str(), "raw" | "pkcs8" | "spki" | "jwk") {
        return Ok(f);
    }
    Err(ScErr::Type(sc_where(op, &format!("Invalid keyFormat argument: {f}"))))
}

/// The AES key lengths this browser family accepts, as the two DIFFERENT errors it answers with.
fn aes_key_bytes_ok(len: usize) -> Result<(), ScErr> {
    match len {
        16 | 32 => Ok(()),
        24 => Err(ScErr::Dom("OperationError".into(), wc::AES_192_UNSUPPORTED.into())),
        _ => Err(data_err("AES key data must be 128 or 256 bits")),
    }
}

/// The JWK `alg` a secret key of this algorithm carries -- `A128CBC`, `A256GCM`, `A128KW`, `HS256`.
/// `A<n>CTR` is not in JWA at all and Chrome mints and accepts it anyway, measured.
fn jwk_alg_for(name: &str, bits: usize, hash: &str) -> Option<String> {
    Some(match name {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" => format!("A{bits}{}", &name[4..]),
        "HMAC" => match hash {
            "SHA-1" => "HS1".to_string(),
            "SHA-256" => "HS256".to_string(),
            "SHA-384" => "HS384".to_string(),
            "SHA-512" => "HS512".to_string(),
            _ => return None,
        },
        _ => return None,
    })
}

/// A JWK's string member, or `None` when it is absent or not a string.
pub(super) fn jwk_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) -> Option<String> {
    get_prop(scope, o, key).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

/// Read an `oct` JWK into its key bytes, running Chrome's checks in Chrome's order.
fn read_jwk_oct(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, name: &str, hash: &str, usages: &[String], extractable: bool) -> Result<Vec<u8>, ScErr> {
    let o = v8::Local::<v8::Object>::try_from(v).map_err(|_| data_err("JWK input could not be parsed to a JSON dictionary"))?;
    match jwk_str(scope, o, "kty") {
        None => return Err(data_err("The required JWK member \"kty\" was missing")),
        Some(k) if k != "oct" => return Err(data_err("The JWK \"kty\" member was not \"oct\"")),
        Some(_) => {}
    }
    let Some(k) = jwk_str(scope, o, "k") else {
        return Err(data_err("The required JWK member \"k\" was missing"));
    };
    let bytes = wc::b64url_decode(&k)
        .ok_or_else(|| data_err("The JWK member \"k\" could not be base64url decoded or contained padding"))?;
    if let Some(alg) = jwk_str(scope, o, "alg") {
        if name == "HMAC" {
            if jwk_alg_for(name, 0, hash).as_deref() != Some(alg.as_str()) {
                return Err(data_err(JWK_ALG_INCONSISTENT));
            }
        } else {
            // The SUFFIX names the mode and the DIGITS name the length, and Chrome reports the two
            // separately: `A128GCM` for an AES-CBC call is an `alg` inconsistency, while `A256CBC`
            // over sixteen bytes of `k` is a LENGTH one.
            let suffix = &name[4..];
            let digits: String = alg.chars().skip(1).take_while(char::is_ascii_digit).collect();
            let matches_shape = alg.starts_with('A') && !digits.is_empty() && alg[1 + digits.len()..] == *suffix;
            if !matches_shape {
                return Err(data_err(JWK_ALG_INCONSISTENT));
            }
            if digits.parse::<usize>().ok() != Some(bytes.len() * 8) {
                return Err(data_err("The JWK \"k\" member did not include the right length of key data for the given algorithm."));
            }
        }
    }
    if let Some(u) = jwk_str(scope, o, "use") {
        let allowed: &[&str] = match u.as_str() {
            "enc" => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
            "sig" => &["sign", "verify"],
            _ => &[],
        };
        if !usages.iter().all(|x| allowed.contains(&x.as_str())) {
            return Err(data_err(&format!("The JWK \"use\" member was inconsistent with that specified by the Web Crypto call{JWK_SUPERSET}")));
        }
    }
    if let Some(ops) = get_prop(scope, o, "key_ops").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let have: Vec<String> = (0..ops.length())
            .filter_map(|i| ops.get_index(scope, i))
            .filter(|x| x.is_string())
            .map(|x| x.to_rust_string_lossy(scope))
            .collect();
        if !usages.iter().all(|x| have.contains(x)) {
            return Err(data_err(&format!("The JWK \"key_ops\" member was inconsistent with that specified by the Web Crypto call{JWK_SUPERSET}")));
        }
    }
    if let Some(ext) = get_prop(scope, o, "ext").filter(|v| v.is_boolean()) {
        if !ext.boolean_value(scope) && extractable {
            // Chrome's wording, typo and all: "inconsistent what that specified".
            return Err(data_err("The \"ext\" member of the JWK dictionary is inconsistent what that specified by the Web Crypto call"));
        }
    }
    Ok(bytes)
}

/// A secret key as a JWK object, with Chrome's member ORDER (`alg, ext, k, key_ops, kty`), which is
/// what `JSON.stringify` on the result -- and therefore `wrapKey("jwk", ...)`'s plaintext -- prints.
fn write_jwk_oct<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, secret: &[u8], hash: &str, usages: &[String], extractable: bool) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    if name.starts_with("AES-") {
        aes_key_bytes_ok(secret.len())?;
    }
    let alg = jwk_alg_for(name, secret.len() * 8, hash)
        .ok_or_else(|| ScErr::Dom("NotSupportedError".into(), "Unsupported export key format for algorithm".into()))?;
    let alg_v = str_val(scope, &alg);
    let ext_v: v8::Local<v8::Value> = v8::Boolean::new(scope, extractable).into();
    let k_v = str_val(scope, &wc::b64url_encode(secret));
    let ops: Vec<v8::Local<v8::Value>> = usages.iter().map(|u| str_val(scope, u)).collect();
    let ops_v: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &ops).into();
    let kty_v = str_val(scope, "oct");
    Ok(crypto_obj(scope, &[("alg", alg_v), ("ext", ext_v), ("k", k_v), ("key_ops", ops_v), ("kty", kty_v)]))
}

/// The usages an algorithm's secret key may carry, or `&[]` for one this engine cannot import.
fn secret_key_usages(name: &str) -> &'static [&'static str] {
    match name {
        "AES-CBC" | "AES-CTR" | "AES-GCM" => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
        "AES-KW" => &["wrapKey", "unwrapKey"],
        "HMAC" => &["sign", "verify"],
        _ => &[],
    }
}

/// The two SyntaxErrors every secret-key import runs, in Chrome's order and with its two messages
/// (they are DIFFERENT strings and `usages_ok` collapsed them into one).
fn check_secret_usages(name: &str, usages: &[String]) -> Result<(), ScErr> {
    let allowed = secret_key_usages(name);
    if allowed.is_empty() {
        // RSA and the P-curves are handled above. What is left with no allowed
        // usages is `Ed25519` and `X25519`, whose key IMPORT this engine still does not do.
        return Err(ScErr::Dom("NotSupportedError".into(), format!("{name} key import is not implemented")));
    }
    if !usages.iter().all(|u| allowed.contains(&u.as_str())) {
        return Err(ScErr::Dom("SyntaxError".into(), "Cannot create a key using the specified key usages.".into()));
    }
    if usages.is_empty() {
        return Err(ScErr::Dom("SyntaxError".into(), "Usages cannot be empty when creating a key.".into()));
    }
    Ok(())
}

/// Mint a secret `CryptoKey` of `name` from `material`, which is a BufferSource for `raw` and a JWK
/// object for `jwk`. Shared by `importKey` and `unwrapKey`, which the spec defines as the same step.
fn secret_key_from<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &'static str,
    algo_obj: Option<v8::Local<v8::Object>>,
    format: &str,
    material: v8::Local<v8::Value>,
    extractable: bool,
    usages: Vec<String>,
    op: &str,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    // A KDF key can be UNWRAPPED as well as imported (Chrome 150, measured: `unwrapKey into a
    // PBKDF2 key` answers `{"name":"PBKDF2"}`), so its rules live here rather than in `importKey`.
    if name == "PBKDF2" || name == "HKDF" {
        return import_kdf_key(scope, material, format, name, extractable, usages);
    }
    // RSA and the P-curves. `pkcs8` and `spki` carry no symmetric key, so every
    // algorithm that accepts them is one of these five; `raw` and `jwk` are shared with the secret
    // path and are dispatched by NAME, not by format.
    if asym_keys::is_asym(name) {
        return asym_keys::import(scope, name, algo_obj, format, material, extractable, usages, op);
    }
    check_secret_usages(name, &usages)?;
    if format == "pkcs8" || format == "spki" {
        return Err(ScErr::Dom("NotSupportedError".into(), "Unsupported import key format for algorithm".into()));
    }
    let hash = if name == "HMAC" { crypto_hash_of(scope, algo_obj) } else { String::new() };
    let bytes = if format == "jwk" {
        read_jwk_oct(scope, material, name, &hash, &usages, extractable)?
    } else {
        crypto_bytes(scope, material).ok_or_else(|| ScErr::Type("keyData is not a BufferSource".into()))?
    };
    if name != "HMAC" {
        aes_key_bytes_ok(bytes.len())?;
    }
    let bits = (bytes.len() * 8) as f64;
    let algo = if name == "HMAC" {
        let n = str_val(scope, "HMAC");
        let hn = str_val(scope, &hash);
        let hobj = crypto_obj(scope, &[("name", hn)]);
        let len = crypto_num(scope, bits);
        crypto_obj(scope, &[("name", n), ("hash", hobj), ("length", len)])
    } else {
        let n = str_val(scope, name);
        let len = crypto_num(scope, bits);
        crypto_obj(scope, &[("name", n), ("length", len)])
    };
    Ok(make_crypto_key(scope, "secret", algo, extractable, usages, bytes, &hash))
}

fn sc_export_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = "exportKey";
    let format = sc_key_format(scope, args.get(0), op)?;
    let key = args.get(1);
    let Some(k) = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key) else {
        return Err(ScErr::Type(sc_where(op, "parameter 2 is not of type 'CryptoKey'.")));
    };
    // "If the [[extractable]] internal slot of key is false, then throw an InvalidAccessError." It was
    // not checked at all, so a KDF key -- which can ONLY be imported unextractable -- handed its secret
    // back. Chrome 150's message, measured, and it runs BEFORE the format switch: an
    // unextractable PBKDF2 key asked for `raw` answers this and not "unsupported format".
    let (extractable, usages) = {
        let r = unsafe { k.as_ref() };
        (r.extractable.get(), r.usages.borrow().clone())
    };
    if !extractable {
        return Err(ScErr::Dom("InvalidAccessError".into(), sc_where(op, "key is not extractable")));
    }
    let (secret, hash) = crypto_key_secret(scope, key).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    // An asymmetric key's `secret` slot is a DER, and all four formats mean something different
    // for it than they do for a bag of bytes.
    if let Some((key_type, der, h, u, ext)) = crypto_key_parts(scope, key) {
        if key_type == "public" || key_type == "private" {
            let name = key_algorithm_name(scope, key).unwrap_or_default();
            if let Some(canonical) = canonical_algorithm(&name) {
                if asym_keys::is_asym(canonical) {
                    return asym_keys::export(scope, canonical, key_type == "private", &der, &h, &u, ext, &format);
                }
            }
        }
    }
    match format.as_str() {
        "raw" => Ok(crypto_ab(scope, secret)),
        "jwk" => {
            let name = key_algorithm_name(scope, key).unwrap_or_default();
            write_jwk_oct(scope, &name, &secret, &hash, &usages, extractable)
        }
        _ => Err(ScErr::Dom("NotSupportedError".into(), "Unsupported export key format for algorithm".into())),
    }
}
fn cb_sc_export_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_export_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

fn sc_import_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = "importKey";
    let format = sc_key_format(scope, args.get(0), op)?;
    let (name, o) = sc_normalize(scope, args.get(2), op)?;
    // The WebIDL union `(BufferSource or JsonWebKey)`, then the format's own requirement -- three
    // different TypeErrors, all measured: a string matches neither arm, a `Uint8Array` matches the
    // union and then fails `jwk`, and a plain object matches the union and then fails `raw`.
    let material = args.get(1);
    let is_buf = material.is_array_buffer() || material.is_array_buffer_view();
    let is_dict = !is_buf && material.is_object();
    if !is_buf && !is_dict {
        return Err(ScErr::Type(sc_where(op, "The provided value is not of type '(ArrayBuffer or ArrayBufferView or JsonWebKey)'.")));
    }
    if format == "jwk" && !is_dict {
        return Err(ScErr::Type(sc_where(op, "Key data must be an object for JWK import")));
    }
    if format != "jwk" && !is_buf {
        return Err(ScErr::Type(sc_where(op, "Key data must be a BufferSource for non-JWK formats")));
    }
    let extractable = args.get(3).boolean_value(scope);
    let usages = crypto_usages(scope, args.get(4));
    secret_key_from(scope, name, o, &format, material, extractable, usages, op)
}
fn cb_sc_import_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_import_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

// ── deriveKey / wrapKey / unwrapKey ──────────────────────────────────────────────────────────────
//
// The last three members of SubtleCrypto the Brave-151 capture records and this one did not have. Each
// is defined by the spec as a COMPOSITION of operations already here, and is written that way rather
// than as a second implementation of the same primitive:
//
//   deriveKey  = derive bits, then import them as a key of the requested type
//   wrapKey    = export the key, then encrypt the exported bytes
//   unwrapKey  = decrypt the wrapped bytes, then import them as a key
//
// They therefore inherit this engine's algorithm coverage exactly -- X25519 for derivation, AES-CTR for
// wrapping, `raw` for import/export -- and refuse anything else with the same NotSupportedError the
// underlying operation would have raised. That is the honest boundary: a wrapKey that quietly accepted
// AES-KW and returned the unwrapped bytes would be worse than one that says no.

/// The bit length a derived key should have, from its `derivedKeyAlgorithm`.
fn sc_derived_key_bits(scope: &mut v8::PinScope, algo: v8::Local<v8::Value>, name: &str) -> Result<i32, ScErr> {
    match name {
        // AES key length is stated by the caller and is the only legal set.
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" => {
            let o = crypto_algo_obj(algo);
            let n = o.and_then(|o| get_prop(scope, o, "length")).and_then(|v| v.number_value(scope)).unwrap_or(0.0) as i32;
            if n == 192 {
                return Err(ScErr::Dom("OperationError".into(), wc::AES_192_UNSUPPORTED.into()));
            }
            if !matches!(n, 128 | 256) {
                return Err(ScErr::Dom("OperationError".into(), "AES key length must be 128 or 256 bits".into()));
            }
            Ok(n)
        }
        // An HMAC key defaults to the hash's block size; `length` overrides it.
        "HMAC" => {
            let o = crypto_algo_obj(algo);
            if let Some(n) = o.and_then(|o| get_prop(scope, o, "length")).and_then(|v| v.number_value(scope)) {
                if n > 0.0 { return Ok(n as i32); }
            }
            let hash = crypto_hash_of(scope, o);
            Ok(match hash.as_str() { "SHA-1" => 512, "SHA-384" | "SHA-512" => 1024, _ => 512 })
        }
        _ => Err(ScErr::Dom("NotSupportedError".into(), "unsupported derivedKeyAlgorithm".into())),
    }
}

fn sc_derive_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    if let Ok(kdf) = crypto_algo_name(scope, args.get(0), "deriveKey") {
        if kdf == "PBKDF2" || kdf == "HKDF" {
            return kdf_derive_key(scope, args, &kdf);
        }
    }
    sc_require_usage(scope, args.get(1), "deriveKey")?;
    let name = crypto_algo_name(scope, args.get(0), "deriveKey")?;
    if name != "X25519" { return Err(ScErr::Dom("NotSupportedError".into(), "unsupported deriveKey algorithm".into())); }
    let o = crypto_algo_obj(args.get(0));
    let (private, _h) = crypto_key_secret(scope, args.get(1)).ok_or_else(|| ScErr::Type("baseKey is not a CryptoKey".into()))?;
    let pub_val = o.and_then(|o| get_prop(scope, o, "public")).ok_or_else(|| ScErr::Type("algorithm.public is required".into()))?;
    let (public, _h2) = crypto_key_secret(scope, pub_val).ok_or_else(|| ScErr::Type("algorithm.public is not a CryptoKey".into()))?;

    let derived_algo = args.get(2);
    let derived_name = crypto_algo_name(scope, derived_algo, "deriveKey")?;
    let bits = sc_derived_key_bits(scope, derived_algo, &derived_name)?;
    let extractable = args.get(3).boolean_value(scope);
    let usages = crypto_usages(scope, args.get(4));

    let secret = wc::x25519_derive(&private, &public, bits).ok_or_else(|| ScErr::Dom("OperationError".into(), "X25519 derive failed".into()))?;
    match derived_name.as_str() {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" => {
            let algo = { let n = str_val(scope, &derived_name); let len = crypto_num(scope, bits as f64); crypto_obj(scope, &[("name", n), ("length", len)]) };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, secret, ""))
        }
        "HMAC" => {
            let hash = crypto_hash_of(scope, crypto_algo_obj(derived_algo));
            let algo = { let n = str_val(scope, "HMAC"); let hn = str_val(scope, &hash); let hobj = crypto_obj(scope, &[("name", hn)]); crypto_obj(scope, &[("name", n), ("hash", hobj)]) };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, secret, &hash))
        }
        _ => Err(ScErr::Dom("NotSupportedError".into(), "unsupported derivedKeyAlgorithm".into())),
    }
}
fn cb_sc_derive_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_derive_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

/// WebIDL `[EnforceRange] unsigned long` -- the declared type of every `length` member in
/// WebCryptoAPI's key-generation dictionaries.
///
/// MEASURED ON CHROME 150 rather than read off the IDL, because the two halves of `[EnforceRange]`
/// are easy to state backwards: a FRACTIONAL value is truncated and ACCEPTED, while an out-of-range
/// one THROWS rather than wrapping.
///
/// ```text
/// length        Chrome 150     this function
/// 256           resolved       Ok(256)
/// 1.5           resolved       Ok(1)          <- truncated, not rejected
/// 4294967295    resolved       Ok(u32::MAX)
/// 4294967296    TypeError      Err(Type)
/// -1            TypeError      Err(Type)
/// NaN           TypeError      Err(Type)
/// Infinity      TypeError      Err(Type)
/// ```
///
/// `0` converts fine here and is the CALLER's business: WebCrypto makes a zero-length HMAC key an
/// `OperationError`, which is a different error from a range failure and belongs at the use site.
fn enforce_range_u32(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str) -> Result<u32, ScErr> {
    let n = v.number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() {
        return Err(ScErr::Type(format!("`{member}` is not a finite number")));
    }
    let t = n.trunc();
    if t < 0.0 || t > f64::from(u32::MAX) {
        return Err(ScErr::Type(format!("`{member}` is outside the range of an unsigned long: {n}")));
    }
    Ok(t as u32)
}

/// Run the wrapping cipher. AES-KW is its own transform; the other three are the same modes
/// `encrypt`/`decrypt` run, which is exactly the spec's definition of wrapping.
fn sc_wrap_cipher(name: &str, params: &AesParams, wrapping: &[u8], bytes: &[u8], unwrapping: bool) -> Result<Vec<u8>, ScErr> {
    if name != "AES-KW" {
        return sc_aes_run(params, wrapping, bytes, unwrapping);
    }
    if bytes.len() % 8 != 0 {
        return Err(data_err("The AES-KW input data length is invalid: not a multiple of 8 bytes"));
    }
    let min = if unwrapping { 24 } else { 16 };
    if bytes.len() < min {
        return Err(ScErr::Dom("OperationError".into(), "The provided data is too small".into()));
    }
    let out = if unwrapping { wc::aes_kw_unwrap(wrapping, bytes) } else { wc::aes_kw_wrap(wrapping, bytes) };
    out.ok_or_else(|| ScErr::Dom("OperationError".into(), String::new()))
}

/// `RSA-OAEP` is a legal WRAPPING algorithm, so the wrapping step is a cipher chosen by name and
/// not an AES mode by assumption. `wrapping` is the wrapping key's bytes -- a DER for RSA-OAEP.
fn sc_asym_or_aes_cipher(
    scope: &mut v8::PinScope,
    name: &str,
    params: &AesParams,
    wrapping_key: v8::Local<v8::Value>,
    wrapping: &[u8],
    bytes: &[u8],
    unwrapping: bool,
) -> Result<Vec<u8>, ScErr> {
    if name != "RSA-OAEP" {
        return sc_wrap_cipher(name, params, wrapping, bytes, unwrapping);
    }
    let (key_type, _der, hash, _u, _e) = crypto_key_parts(scope, wrapping_key)
        .ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    let label: Vec<u8> = Vec::new();
    let out = if unwrapping {
        let k = crate::asym::private_from_pkcs8(wrapping).ok_or_else(|| ScErr::Dom("OperationError".into(), String::new()))?;
        let _ = key_type;
        crate::asym::rsa_oaep_decrypt(&k, &hash, &label, bytes)
    } else {
        let k = crate::asym::public_from_spki(wrapping).ok_or_else(|| ScErr::Dom("OperationError".into(), String::new()))?;
        crate::asym::rsa_oaep_encrypt(&k, &hash, &label, bytes)
    };
    out.ok_or_else(|| ScErr::Dom("OperationError".into(), String::new()))
}

/// The wrapping key's three checks, shared by `wrapKey` and `unwrapKey`: it is a `CryptoKey`, its
/// algorithm is the one being asked for, and it carries the usage.
fn sc_wrapping_key(scope: &mut v8::PinScope, key: v8::Local<v8::Value>, name: &str, op: &str) -> Result<Vec<u8>, ScErr> {
    if deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key).is_none() {
        return Err(ScErr::Type(sc_where(op, "parameter 3 is not of type 'CryptoKey'.")));
    }
    if key_algorithm_name(scope, key).as_deref() != Some(&name.to_ascii_uppercase()) {
        return Err(ScErr::Dom("InvalidAccessError".into(), "key.algorithm does not match that of operation".into()));
    }
    sc_require_usage(scope, key, op)?;
    Ok(crypto_key_secret(scope, key).map(|(s, _)| s).unwrap_or_default())
}

fn sc_wrap_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = "wrapKey";
    let format = sc_key_format(scope, args.get(0), op)?;
    let (name, o) = sc_normalize(scope, args.get(3), op)?;
    let params = AesParams::read(scope, name, o, op)?;
    let wrapping = sc_wrapping_key(scope, args.get(2), name, op)?;
    // "If key.extractable is false, throw an InvalidAccessError." Wrapping is an export, and an
    // unextractable key must not leave through it any more than through exportKey.
    let key_v = args.get(1);
    let Some(k) = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key_v) else {
        return Err(ScErr::Type(sc_where(op, "parameter 2 is not of type 'CryptoKey'.")));
    };
    let (extractable, usages) = {
        let r = unsafe { k.as_ref() };
        (r.extractable.get(), r.usages.borrow().clone())
    };
    if !extractable {
        return Err(ScErr::Dom("InvalidAccessError".into(), "key is not extractable".into()));
    }
    let (secret, hash) = crypto_key_secret(scope, key_v).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    // The bytes wrapped are the EXPORTED key in `format`. For `jwk` that is the JSON text of the
    // very object `exportKey("jwk")` answers, which is why it is serialized from that object here
    // rather than formatted a second time: two spellings of one dictionary would drift.
    let key_name = key_algorithm_name(scope, key_v).unwrap_or_default();
    let asym_wrapped = canonical_algorithm(&key_name).filter(|c| asym_keys::is_asym(c));
    let bytes = match (asym_wrapped, format.as_str()) {
        // An RSA or EC key exports through the same four formats here as through `exportKey`.
        (Some(canonical), f) => {
            let (key_type, der, h, u, ext) = crypto_key_parts(scope, key_v).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
            asym_keys::wrap_bytes(scope, canonical, key_type == "private", &der, &h, &u, ext, f)?
        }
        (None, "raw") => secret,
        (None, "jwk") => {
            let jwk = write_jwk_oct(scope, &key_name, &secret, &hash, &usages, extractable)?;
            let s = v8::json::stringify(scope, jwk).ok_or_else(|| ScErr::Dom("OperationError".into(), String::new()))?;
            s.to_rust_string_lossy(scope).into_bytes()
        }
        (None, _) => return Err(ScErr::Dom("NotSupportedError".into(), "Unsupported export key format for algorithm".into())),
    };
    let out = sc_asym_or_aes_cipher(scope, name, &params, args.get(2), &wrapping, &bytes, false)?;
    Ok(crypto_ab(scope, out))
}
fn cb_sc_wrap_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_wrap_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

fn sc_unwrap_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = "unwrapKey";
    let format = sc_key_format(scope, args.get(0), op)?;
    let (name, o) = sc_normalize(scope, args.get(3), op)?;
    let params = AesParams::read(scope, name, o, op)?;
    // The unwrapped key's algorithm is normalized BEFORE the unwrapping runs -- measured: an
    // unrecognized name answers `Unrecognized name` even when the ciphertext could never decrypt.
    // It is normalized for `importKey`, not for `unwrapKey`: it names the key being CREATED.
    let (key_name, key_o) = sc_normalize_for(scope, args.get(4), op, "importKey")?;
    let unwrapping = sc_wrapping_key(scope, args.get(2), name, op)?;
    let wrapped = crypto_bytes(scope, args.get(1)).ok_or_else(|| ScErr::Type("wrappedKey is not a BufferSource".into()))?;
    let bytes = sc_asym_or_aes_cipher(scope, name, &params, args.get(2), &unwrapping, &wrapped, true)?;

    let extractable = args.get(5).boolean_value(scope);
    let usages = crypto_usages(scope, args.get(6));
    let material: v8::Local<v8::Value> = match format.as_str() {
        "jwk" => {
            let bad = || data_err("JWK input could not be parsed to a JSON dictionary");
            let text = String::from_utf8(bytes).map_err(|_| bad())?;
            let src = v8::String::new(scope, &text).ok_or_else(bad)?;
            let parsed: Option<v8::Global<v8::Value>> = {
                v8::tc_scope!(let tc, scope);
                match v8::json::parse(tc, src) {
                    Some(v) => Some(v8::Global::new(tc, v)),
                    None => { tc.reset(); None }
                }
            };
            let g = parsed.ok_or_else(bad)?;
            v8::Local::new(scope, &g)
        }
        _ => crypto_ab(scope, bytes),
    };
    secret_key_from(scope, key_name, key_o, &format, material, extractable, usages, op)
}
fn cb_sc_unwrap_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_unwrap_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

fn sc_generate_key_impl<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let name = crypto_algo_name(scope, args.get(0), "generateKey")?;
    let o = crypto_algo_obj(args.get(0));
    let extractable = args.get(1).boolean_value(scope);
    let usages = crypto_usages(scope, args.get(2));
    macro_rules! require { ($allowed:expr) => { if !wc::usages_ok(&usages, $allowed) { return Err(ScErr::Dom("SyntaxError".into(), "invalid key usages".into())); } }; }
    match name.as_str() {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" => {
            require!(if name == "AES-KW" { &["wrapKey", "unwrapKey"][..] } else { &["encrypt", "decrypt", "wrapKey", "unwrapKey"][..] });
            let len = o.and_then(|o| get_prop(scope, o, "length")).and_then(|v| v.number_value(scope)).unwrap_or(-1.0) as i64;
            // 192 is a length Chrome RECOGNIZES and declines, with its own sentence; everything else
            // off the list is simply invalid. Measured: `generateKey AES-CBC 192` in the fixture.
            if len == 192 { return Err(ScErr::Dom("OperationError".into(), wc::AES_192_UNSUPPORTED.into())); }
            if !wc::aes_key_len_ok(len) { return Err(ScErr::Dom("OperationError".into(), "invalid AES key length".into())); }
            let secret = wc::random_bytes((len / 8) as usize);
            let algo = { let n = str_val(scope, &name); let l = crypto_num(scope, len as f64); crypto_obj(scope, &[("name", n), ("length", l)]) };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, secret, ""))
        }
        "HMAC" => {
            let hash = crypto_hash_of(scope, o);
            if !wc::is_supported_hash(&hash) { return Err(ScErr::Dom("NotSupportedError".into(), "unsupported hash".into())); }
            require!(&["sign", "verify"][..]);
            let len_prop = o.and_then(|o| get_prop(scope, o, "length"));
            let bits: i64 = match len_prop {
                Some(v) if !v.is_undefined() && !v.is_null() => {
                    // `HmacKeyGenParams.length` is `[EnforceRange] unsigned long`, and this line was
                    // `v.number_value(..) as i64` with NO range check -- so one line of ordinary page
                    // script aborted the process:
                    //
                    //   crypto.subtle.generateKey({name:"HMAC",hash:"SHA-256",length:1e18},true,["sign"])
                    //   -> memory allocation of 125000000000000000 bytes failed -> SIGABRT (rc 134)
                    //
                    // Found by L11, whose replay sweep runs the WPT scoreboard unfiltered and reached
                    // `WebCryptoAPI/generateKey/failures_HMAC.https.any.js`; reproduced by me on two
                    // independently built binaries before this change.
                    let n = enforce_range_u32(scope, v, "length")?;
                    if n == 0 { return Err(ScErr::Dom("OperationError".into(), "zero-length HMAC key".into())); }
                    i64::from(n)
                }
                _ => if hash == "SHA-1" || hash == "SHA-256" { 512 } else { 1024 },
            };
            let secret = wc::random_bytes(((bits + 7) / 8) as usize);
            let algo = { let n = str_val(scope, "HMAC"); let hn = str_val(scope, &hash); let hobj = crypto_obj(scope, &[("name", hn)]); let l = crypto_num(scope, bits as f64); crypto_obj(scope, &[("name", n), ("hash", hobj), ("length", l)]) };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, secret, &hash))
        }
        "X25519" => {
            require!(&["deriveKey", "deriveBits"][..]);
            let (priv_b, pub_b) = wc::x25519_keypair();
            let ap = { let n = str_val(scope, "X25519"); crypto_obj(scope, &[("name", n)]) };
            let privk = make_crypto_key(scope, "private", ap, extractable, usages.clone(), priv_b.to_vec(), "");
            let au = { let n = str_val(scope, "X25519"); crypto_obj(scope, &[("name", n)]) };
            let pubk = make_crypto_key(scope, "public", au, true, Vec::new(), pub_b.to_vec(), "");
            Ok(crypto_obj(scope, &[("privateKey", privk), ("publicKey", pubk)]))
        }
        // The five asymmetric algorithms generate a PAIR, and every rule about which usages each
        // half keeps, which curves exist and which exponents are legal lives in `asym_keys`
        // beside the import rules that have to agree with them.
        "ECDSA" | "ECDH" | "RSASSA-PKCS1-V1_5" | "RSA-PSS" | "RSA-OAEP" => {
            let canonical = canonical_algorithm(&name)
                .ok_or_else(|| ScErr::Dom("NotSupportedError".into(), format!("unrecognized algorithm: {name}")))?;
            asym_keys::generate(scope, canonical, o, extractable, usages)
        }
        "ED25519" | "ED448" => { require!(&["sign", "verify"][..]); Err(ScErr::Dom("NotSupportedError".into(), "not implemented".into())) }
        "X448" => { require!(&["deriveKey", "deriveBits"][..]); Err(ScErr::Dom("NotSupportedError".into(), "not implemented".into())) }
        _ => Err(ScErr::Dom("NotSupportedError".into(), format!("unrecognized algorithm: {name}"))),
    }
}
fn cb_sc_generate_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { if !sc_recv(scope, args.this().into()) { return; } let r = sc_generate_key_impl(scope, &args); let p = sc_settle(scope, r); rv.set(p); }

// ── PBKDF2 and HKDF ────────────────────────────────────────────────────────────────
//
// `importKey("raw", k, "PBKDF2" | "HKDF", ...)` rejected with "unsupported import algorithm", so a
// page deriving a key from a passphrase (a documentation site's client-side search did) died at the
// import. Both algorithms are here whole: import, `deriveBits`, `deriveKey`, with the bytes from
// `kdf.rs` and every check and message below measured on google-chrome 150.0.7871.46 (loopback page):
//
// ```text
// importKey("jwk"|"pkcs8", .., "PBKDF2")             NotSupportedError  Unsupported import key format for algorithm
// importKey("raw", .., "PBKDF2", false, ["sign"])     SyntaxError        Cannot create a key using the specified key usages.
// importKey("raw", .., "PBKDF2", false, [])           SyntaxError        Usages cannot be empty when creating a key.
// importKey("raw", .., "HKDF", true, ["deriveBits"])  SyntaxError        KDF keys must set extractable=false
// key.algorithm                                       {"name":"PBKDF2"}
// deriveBits(PBKDF2, k, 7)                            OperationError     Length for PBKDF2 key derivation must be a multiple of 8 bits.
// deriveBits(PBKDF2, k, 0)                            an empty ArrayBuffer
// deriveBits(PBKDF2, k, null | undefined)             OperationError     No length was specified for the PBKDF2 Derive Bits operation.
// deriveBits({.. no hash}, k, 256)                    TypeError          Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: hash: Missing or not an AlgorithmIdentifier
// deriveBits({.. hash: "MD5"}, k, 256)                NotSupportedError  Failed to execute 'deriveBits' on 'SubtleCrypto': Pbkdf2Params: hash: Algorithm: Unrecognized name
// deriveBits({.. no salt | no iterations}, k, 256)    TypeError          ...: Pbkdf2Params: salt | iterations: Missing required property
// deriveBits({.. iterations: 0}, k, 256)              OperationError     PBKDF2 requires iterations > 0
// deriveBits(HKDF, k, 7)                              OperationError     The length provided for HKDF is not a multiple of 8 bits.
// deriveBits(HKDF, k, 255*256+8)                      OperationError     The length provided for HKDF is too large.
// deriveBits({HKDF, no info}, k, 256)                 TypeError          ...: HkdfParams: info: Missing required property
// deriveBits(PBKDF2 params, an HKDF key, 256)         InvalidAccessError key.algorithm does not match that of operation
// deriveKey(PBKDF2, k, {HMAC, SHA-384}, ..)           {"name":"HMAC","hash":{"name":"SHA-384"},"length":1024}
// deriveKey(HKDF, k, {AES-CBC, length 100}, ..)       OperationError     AES key length must be 128 or 256 bits
// deriveKey(HKDF, k, "PBKDF2", ..)                    OperationError     No length was specified for the HKDF Derive Bits operation.
// ```

/// The algorithm name a `CryptoKey` was created with (`key.algorithm.name`), or `None` for a non-key.
pub(super) fn key_algorithm_name(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> Option<String> {
    let k = deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key)?;
    let g = unsafe { k.as_ref() }.algorithm.borrow().clone()?;
    let algo = v8::Local::new(scope, &g);
    let o = v8::Local::<v8::Object>::try_from(algo).ok()?;
    get_prop(scope, o, "name").map(|n| n.to_rust_string_lossy(scope).to_ascii_uppercase())
}

/// `importKey` for the two KDF algorithms: raw only, derive usages only (and some), never extractable.
fn import_kdf_key<'s>(scope: &mut v8::PinScope<'s, '_>, material: v8::Local<v8::Value>, format: &str, name: &str, extractable: bool, usages: Vec<String>) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    if format != "raw" {
        return Err(ScErr::Dom("NotSupportedError".into(), "Unsupported import key format for algorithm".into()));
    }
    let bytes = crypto_bytes(scope, material).ok_or_else(|| ScErr::Type("keyData is not a BufferSource".into()))?;
    if !usages.iter().all(|u| u == "deriveKey" || u == "deriveBits") {
        return Err(ScErr::Dom("SyntaxError".into(), "Cannot create a key using the specified key usages.".into()));
    }
    if extractable {
        return Err(ScErr::Dom("SyntaxError".into(), "KDF keys must set extractable=false".into()));
    }
    if usages.is_empty() {
        return Err(ScErr::Dom("SyntaxError".into(), "Usages cannot be empty when creating a key.".into()));
    }
    let algo = { let n = str_val(scope, name); crypto_obj(scope, &[("name", n)]) };
    Ok(make_crypto_key(scope, "secret", algo, false, usages, bytes, ""))
}

/// A required `BufferSource` member of a KDF params dictionary.
fn kdf_buffer_member(scope: &mut v8::PinScope, params: Option<v8::Local<v8::Object>>, op: &str, dict: &str, member: &str) -> Result<Vec<u8>, ScErr> {
    let v = params.and_then(|o| get_prop(scope, o, member)).filter(|v| !v.is_undefined());
    let Some(v) = v else {
        return Err(ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: {member}: Missing required property")));
    };
    crypto_bytes(scope, v).ok_or_else(|| ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: {member}: Not a BufferSource")))
}

/// The `hash` member of a KDF params dictionary, normalized to its canonical name.
fn kdf_hash_member(scope: &mut v8::PinScope, params: Option<v8::Local<v8::Object>>, op: &str, dict: &str) -> Result<String, ScErr> {
    let h = params.and_then(|o| get_prop(scope, o, "hash")).filter(|h| h.is_string() || h.is_object());
    let Some(h) = h else {
        return Err(ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: hash: Missing or not an AlgorithmIdentifier")));
    };
    let name = match crypto_algo_name(scope, h, op) {
        Ok(n) => n,
        Err(_) => return Err(ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: hash: Algorithm: Missing or not a string: name"))),
    };
    if !wc::is_supported_hash(&name) {
        return Err(ScErr::Dom("NotSupportedError".into(), format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: hash: Algorithm: Unrecognized name")));
    }
    Ok(name)
}

/// Derive `length` bits with PBKDF2 or HKDF from `key`: normalize the params (in the order Chrome
/// reports a missing member), require the key to be of THIS algorithm and to carry `usage`, then check
/// the length and run the function. `op` is the SubtleCrypto method name for the TypeError prefix.
fn kdf_derive(
    scope: &mut v8::PinScope,
    op: &str,
    name: &str,
    params_v: v8::Local<v8::Value>,
    key: v8::Local<v8::Value>,
    usage: &str,
    length: v8::Local<v8::Value>,
) -> Result<Vec<u8>, ScErr> {
    let params = crypto_algo_obj(params_v);
    enum P { Pbkdf2 { salt: Vec<u8>, iterations: u32, hash: String }, Hkdf { hash: String, salt: Vec<u8>, info: Vec<u8> } }
    let p = if name == "PBKDF2" {
        let dict = "Pbkdf2Params";
        let salt = kdf_buffer_member(scope, params, op, dict, "salt")?;
        let it = params.and_then(|o| get_prop(scope, o, "iterations")).filter(|v| !v.is_undefined());
        let Some(it) = it else {
            return Err(ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: iterations: Missing required property")));
        };
        let iterations = enforce_range_u32(scope, it, "iterations")
            .map_err(|_| ScErr::Type(format!("Failed to execute '{op}' on 'SubtleCrypto': {dict}: iterations: Outside of numeric range")))?;
        let hash = kdf_hash_member(scope, params, op, dict)?;
        P::Pbkdf2 { salt, iterations, hash }
    } else {
        let dict = "HkdfParams";
        let hash = kdf_hash_member(scope, params, op, dict)?;
        let salt = kdf_buffer_member(scope, params, op, dict, "salt")?;
        let info = kdf_buffer_member(scope, params, op, dict, "info")?;
        P::Hkdf { hash, salt, info }
    };
    if key_algorithm_name(scope, key).as_deref() != Some(name) {
        if deno_core::cppgc::try_unwrap_cppgc_object::<CryptoKey>(scope, key).is_none() {
            return Err(ScErr::Type("key is not a CryptoKey".into()));
        }
        return Err(ScErr::Dom("InvalidAccessError".into(), "key.algorithm does not match that of operation".into()));
    }
    sc_require_usage(scope, key, usage)?;
    // `optional [EnforceRange] unsigned long? length = null`: null/undefined is "no length". The
    // RANGE is enforced by the caller (`sc_derive_bits_impl`), where WebIDL puts it -- before the
    // algorithm is dispatched -- so by the time this runs the value is known to fit a u32. The
    // comment here used to read "anything else ToUint32", which is the reading that lost
    // `[EnforceRange]` and turned `length: -8` into a 512 MB PBKDF2 derivation.
    if length.is_null_or_undefined() {
        return Err(ScErr::Dom("OperationError".into(), format!("No length was specified for the {name} Derive Bits operation.")));
    }
    let bits = length.uint32_value(scope).unwrap_or(0);
    let (secret, _h) = crypto_key_secret(scope, key).ok_or_else(|| ScErr::Type("key is not a CryptoKey".into()))?;
    match p {
        P::Pbkdf2 { salt, iterations, hash } => {
            if bits % 8 != 0 {
                return Err(ScErr::Dom("OperationError".into(), "Length for PBKDF2 key derivation must be a multiple of 8 bits.".into()));
            }
            if iterations == 0 {
                return Err(ScErr::Dom("OperationError".into(), "PBKDF2 requires iterations > 0".into()));
            }
            super::kdf::pbkdf2(&hash, &secret, &salt, iterations, (bits / 8) as usize)
                .ok_or_else(|| ScErr::Dom("NotSupportedError".into(), "unsupported hash".into()))
        }
        P::Hkdf { hash, salt, info } => {
            if bits % 8 != 0 {
                return Err(ScErr::Dom("OperationError".into(), "The length provided for HKDF is not a multiple of 8 bits.".into()));
            }
            let h_len = super::kdf::hash_len(&hash).unwrap_or(0);
            if (bits / 8) as usize > 255 * h_len {
                return Err(ScErr::Dom("OperationError".into(), "The length provided for HKDF is too large.".into()));
            }
            super::kdf::hkdf(&hash, &secret, &salt, &info, (bits / 8) as usize)
                .ok_or_else(|| ScErr::Dom("OperationError".into(), "The length provided for HKDF is too large.".into()))
        }
    }
}

/// `deriveKey` over PBKDF2/HKDF: WebCrypto's "get key length" of the derived algorithm, derive that
/// many bits, import them as that algorithm's key.
fn kdf_derive_key<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, name: &str) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let derived_algo = args.get(2);
    let derived_name = crypto_algo_name(scope, derived_algo, "deriveKey")?;
    let dobj = crypto_algo_obj(derived_algo);
    let extractable = args.get(3).boolean_value(scope);
    let usages = crypto_usages(scope, args.get(4));
    // "Get key length". A `null` length (a KDF asked to derive a KDF key) is carried into the derive,
    // which is where Chrome reports it -- as the BASE algorithm's "No length was specified".
    let (length, hmac_hash): (v8::Local<v8::Value>, Option<String>) = match derived_name.as_str() {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" => {
            let n = dobj.and_then(|o| get_prop(scope, o, "length")).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
            if n != 128.0 && n != 256.0 {
                return Err(ScErr::Dom("OperationError".into(), "AES key length must be 128 or 256 bits".into()));
            }
            (crypto_num(scope, n), None)
        }
        "HMAC" => {
            let hash = kdf_hash_member(scope, dobj, "deriveKey", "HmacImportParams")?;
            let explicit = dobj.and_then(|o| get_prop(scope, o, "length")).filter(|v| !v.is_undefined());
            let bits = match explicit {
                Some(v) => {
                    let n = enforce_range_u32(scope, v, "length")?;
                    if n == 0 {
                        return Err(ScErr::Dom("OperationError".into(), "zero-length HMAC key".into()));
                    }
                    n
                }
                None => if hash == "SHA-384" || hash == "SHA-512" { 1024 } else { 512 },
            };
            (crypto_num(scope, bits as f64), Some(hash))
        }
        "PBKDF2" | "HKDF" => (v8::null(scope).into(), None),
        _ => return Err(ScErr::Dom("NotSupportedError".into(), "unsupported derivedKeyAlgorithm".into())),
    };
    let bytes = kdf_derive(scope, "deriveKey", name, args.get(0), args.get(1), "deriveKey", length)?;
    let bits = bytes.len() * 8;
    let allowed: &[&str] = match derived_name.as_str() {
        "AES-KW" => &["wrapKey", "unwrapKey"],
        "AES-CBC" | "AES-CTR" | "AES-GCM" => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
        _ => &["sign", "verify"],
    };
    if !usages.iter().all(|u| allowed.contains(&u.as_str())) {
        return Err(ScErr::Dom("SyntaxError".into(), "Cannot create a key using the specified key usages.".into()));
    }
    if usages.is_empty() {
        return Err(ScErr::Dom("SyntaxError".into(), "Usages cannot be empty when creating a key.".into()));
    }
    match hmac_hash {
        None => {
            let algo = { let n = str_val(scope, &derived_name); let len = crypto_num(scope, bits as f64); crypto_obj(scope, &[("name", n), ("length", len)]) };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, bytes, ""))
        }
        Some(hash) => {
            let algo = {
                let n = str_val(scope, "HMAC");
                let hn = str_val(scope, &hash);
                let hobj = crypto_obj(scope, &[("name", hn)]);
                let len = crypto_num(scope, bits as f64);
                crypto_obj(scope, &[("name", n), ("hash", hobj), ("length", len)])
            };
            Ok(make_crypto_key(scope, "secret", algo, extractable, usages, bytes, &hash))
        }
    }
}

// ── the table ──────────────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `SubtleCrypto` half of `dom/08f-crypto.js`: twelve `method(...)` calls and
// a `Symbol.toStringTag` accessor installed through `op_sc_*` builder round-trips onto the cppgc
// CLASS TEMPLATE's prototype, plus a throwing JS wrapper that shadowed the class while SHARING that
// prototype, plus `new SubtleCrypto()` from JS to mint the `crypto.subtle` singleton. All twelve are
// rows below, mounted by THE installer onto a prototype it owns; the singleton is minted here.

/// The receiver as a `SubtleCrypto`, or the WebIDL `TypeError` a foreign one gets. Every body is
/// stateless, so before this an `Object.create(SubtleCrypto.prototype)` could drive the whole of
/// WebCrypto -- which is not what a browser answers.
fn sc_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<SubtleCrypto>(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

/// The body every row of [`SUBTLECRYPTO`] names.
pub fn install_subtle_crypto(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "decrypt" => method_row(scope, proto, member, cb_sc_decrypt),
        "deriveBits" => method_row(scope, proto, member, cb_sc_derive_bits),
        "deriveKey" => method_row(scope, proto, member, cb_sc_derive_key),
        "digest" => method_row(scope, proto, member, cb_sc_digest),
        "encrypt" => method_row(scope, proto, member, cb_sc_encrypt),
        "exportKey" => method_row(scope, proto, member, cb_sc_export_key),
        "generateKey" => method_row(scope, proto, member, cb_sc_generate_key),
        "importKey" => method_row(scope, proto, member, cb_sc_import_key),
        "sign" => method_row(scope, proto, member, cb_sc_sign),
        "unwrapKey" => method_row(scope, proto, member, cb_sc_unwrap_key),
        "verify" => method_row(scope, proto, member, cb_sc_verify),
        "wrapKey" => method_row(scope, proto, member, cb_sc_wrap_key),
        other => debug_assert!(false, "install_subtle_crypto: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `SubtleCrypto` blocks of `install/generated_registry.rs` +
// `generated_worker_registry.rs`: the same twelve names in the capture's (alphabetical) order, the
// same lengths, all `enumerable + configurable`. `required` is the WINDOW capture's -- the worker
// registry records 0 for every one of them, and one table cannot carry two arities, so the stricter
// (and correct: WebIDL gives none of these an optional argument) one is kept in both realms. The
// bundle's oracle records TypeError for a zero-argument call to all twelve.
webapi::interface! {
    "SubtleCrypto" [interface, both, secure] {
        ctor   "SubtleCrypto"                        len 0 = stub,
        method "decrypt"     : Promise len 3 required 3     = install_subtle_crypto,
        method "deriveBits"  : Promise len 2 required 2     = install_subtle_crypto,
        method "deriveKey"   : Promise len 5 required 5     = install_subtle_crypto,
        method "digest"      : Promise len 2 required 2     = install_subtle_crypto,
        method "encrypt"     : Promise len 3 required 3     = install_subtle_crypto,
        method "exportKey"   : Promise len 2 required 2     = install_subtle_crypto,
        method "generateKey" : Promise len 3 required 3     = install_subtle_crypto,
        method "importKey"   : Promise len 5 required 5     = install_subtle_crypto,
        method "sign"        : Promise len 3 required 3     = install_subtle_crypto,
        method "unwrapKey"   : Promise len 7 required 7     = install_subtle_crypto,
        method "verify"      : Promise len 4 required 4     = install_subtle_crypto,
        method "wrapKey"     : Promise len 4 required 4     = install_subtle_crypto
    }
}

// ── `crypto.subtle` ─────────────────────────────────────────────────────────────────────────────
//
// `globalThis.crypto` is `shared-globals.js`'s object literal and its two methods have JS bodies
// there (W4.c's lane owns both). So `Crypto` is NOT a table in this lane -- it stays in the generated
// registry with its three rows -- and this lane owns exactly ONE of its members: `subtle`, which
// `dom/08f-crypto.js` wrote as an own DATA property of the `crypto` object where Chrome has an
// ACCESSOR on `Crypto.prototype` (the bundle records `kind: accessor, get, getName "get subtle"`).
// The row below goes through THE installer's member mount onto that prototype, so the descriptor is
// the capture's and the getter is `[native code]` by construction. W4.c's `Crypto` table absorbs the
// row when it takes the other two.

/// The private slot on the realm's global holding this realm's one `SubtleCrypto` instance; the
/// `subtle` getter reads it back, so every read is the same object (`crypto.subtle === crypto.subtle`).
const SUBTLE_SLOT: &str = "korvus_subtle_instance";

fn subtle_slot<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    let n = v8::String::new(scope, SUBTLE_SLOT)?;
    Some(v8::Private::for_api(scope, Some(n)))
}

/// Mint this realm's `SubtleCrypto` -- a cppgc instance re-prototyped onto the TABLE's prototype --
/// and stash it on the global's private slot. Returns it so the caller can also hand it out directly.
/// This realm's `SubtleCrypto` ([SameObject]): the one `mint_subtle` parked on the realm's global, or a
/// fresh one minted now for a realm that never installed `Crypto`.
pub(crate) fn realm_subtle<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let global = scope.get_current_context().global(scope);
    let k = subtle_slot(scope)?;
    match global.get_private(scope, k) {
        Some(v) if !v.is_undefined() => Some(v),
        _ => Some(mint_subtle(scope)),
    }
}

pub(crate) fn mint_subtle<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let obj = crate::binding::class::mint(scope, SubtleCrypto { _p: 0 });
    if let Some(p) = crate::binding::protos::shell_proto(scope, "SubtleCrypto") {
        obj.set_prototype(scope, p.into());
    }
    let v: v8::Local<v8::Value> = obj.into();
    let global = scope.get_current_context().global(scope);
    if let Some(k) = subtle_slot(scope) {
        global.set_private(scope, k, v);
    }
    v
}

// `cb_crypto_subtle`, `install_crypto_subtle` and the standalone `CRYPTO_SUBTLE` row lived here
// while `Crypto` had no table of its own and only this one member lived here. Then
// `Crypto` became a table (`modules/crypto/crypto.rs`), so the row moved into it with the other two and all
// three are installed together; `mint_subtle` above is still what mints the instance.

