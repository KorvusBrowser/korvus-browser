//! `modules/crypto/asym_keys.rs` — the v8 half of RSA and elliptic-curve keys.
//!
//! `src/asym.rs` is the bytes-in/bytes-out layer on BoringSSL. This file is everything a PAGE can
//! see about those keys: the `algorithm` dictionary, the four `KeyFormat`s, the JWK members, the
//! usage rules, and — the part that took the measuring — the exact error each refusal produces.
//!
//! EVERY STRING AND EVERY ORDERING HERE IS CHROME 150.0.7871.46's, RECORDED, not the spec's.
//! `tests/fixtures/webcrypto-probe.js` §12 asks it 126 questions and
//! `webcrypto-probe.chrome-150.txt` is what it answered, twice, identically. Where the two
//! disagree the browser wins (§5). Three places they do:
//!
//!   * **There is no modulus floor on import.** A 512-bit RSA key imports and reports
//!     `modulusLength: 512`. So does 1024. `generateKey` accepts 512 too.
//!   * **`publicExponent` is policed on generate and not on import.** `generateKey` answers
//!     `OperationError: The "publicExponent" must be either 3 or 65537`; `importKey` takes whatever
//!     the DER carries.
//!   * **A malformed DER is a `DataError` with an EMPTY message.** Truncated, trailing byte,
//!     wrong-kind key, five bytes of garbage — one indistinguishable answer, which is why
//!     `asym::public_from_spki` returns `Option` and not a reason.

use deno_core::v8;

use crate::asym;
use crate::binding::protos::{get_prop, str_val};
use crate::modules::crypto::shared::{crypto_ab, crypto_bytes, crypto_num, crypto_obj, ScErr};
use crate::crypto as wc;

use super::subtle_crypto::{
    canonical_usages, crypto_hash_of, data_err, jwk_str, make_crypto_key, sc_where, JWK_ALG_INCONSISTENT, JWK_SUPERSET,
};

/// The five registered algorithms whose keys are RSA or elliptic-curve.
pub(super) const ASYM: [&str; 5] = ["RSA-OAEP", "RSA-PSS", "RSASSA-PKCS1-v1_5", "ECDSA", "ECDH"];

pub(super) fn is_asym(name: &str) -> bool {
    ASYM.contains(&name)
}

fn is_rsa(name: &str) -> bool {
    name.starts_with("RSA")
}

const USAGES_BAD: &str = "Cannot create a key using the specified key usages.";
const USAGES_EMPTY: &str = "Usages cannot be empty when creating a key.";
const WRONG_TYPE: &str = "The key is not of the expected type";
const BAD_FORMAT_IN: &str = "Unsupported import key format for algorithm";
const BAD_FORMAT_OUT: &str = "Unsupported export key format for algorithm";

fn syntax(m: &str) -> ScErr {
    ScErr::Dom("SyntaxError".into(), m.to_string())
}
/// Chrome's empty-message `DataError`, which is every way a key can fail to parse.
fn bad_der() -> ScErr {
    data_err("")
}
fn op_err(m: &str) -> ScErr {
    ScErr::Dom("OperationError".into(), m.to_string())
}

/// `publicExponent` is a `Uint8Array` on the algorithm dictionary, which is observable:
/// `JSON.stringify(key.algorithm)` prints `"publicExponent":{"0":1,"1":0,"2":1}` and would print
/// `{}` for an `ArrayBuffer` and `[1,0,1]` for an Array.
fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let n = bytes.len();
    let v = crypto_ab(scope, bytes);
    match v8::Local::<v8::ArrayBuffer>::try_from(v) {
        Ok(ab) => match v8::Uint8Array::new(scope, ab, 0, n) {
            Some(a) => a.into(),
            None => v,
        },
        Err(_) => v,
    }
}

/// The `algorithm` a page reads off an RSA key, in Chrome's property ORDER: `name`, `hash`,
/// `modulusLength`, `publicExponent` — which is what `JSON.stringify` prints and the probe pins.
fn rsa_algo<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, bits: u32, e: Vec<u8>, hash: &str) -> v8::Local<'s, v8::Value> {
    let n = str_val(scope, name);
    let hn = str_val(scope, hash);
    let hobj = crypto_obj(scope, &[("name", hn)]);
    let ml = crypto_num(scope, f64::from(bits));
    let pe = u8_array(scope, e);
    crypto_obj(scope, &[("name", n), ("hash", hobj), ("modulusLength", ml), ("publicExponent", pe)])
}

/// The `algorithm` of an EC key: `{name, namedCurve}` and nothing else — no `hash`, because for
/// ECDSA the digest is a parameter of `sign`, not of the key.
fn ec_algo<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, curve: &str) -> v8::Local<'s, v8::Value> {
    let n = str_val(scope, name);
    let c = str_val(scope, curve);
    crypto_obj(scope, &[("name", n), ("namedCurve", c)])
}

/// The usages a key of this algorithm and type may carry. A PUBLIC key's list may be empty and a
/// private key's may not, which is the difference between Chrome's two SyntaxErrors.
fn allowed_usages(name: &str, private: bool) -> &'static [&'static str] {
    match (name, private) {
        ("RSA-OAEP", false) => &["encrypt", "wrapKey"],
        ("RSA-OAEP", true) => &["decrypt", "unwrapKey"],
        ("RSA-PSS" | "RSASSA-PKCS1-v1_5" | "ECDSA", false) => &["verify"],
        ("RSA-PSS" | "RSASSA-PKCS1-v1_5" | "ECDSA", true) => &["sign"],
        ("ECDH", false) => &[],
        ("ECDH", true) => &["deriveKey", "deriveBits"],
        _ => &[],
    }
}

/// Chrome's two usage refusals, in its order: a usage the key cannot hold beats an empty list, and
/// an empty list is only an error for a PRIVATE key (`importKey spki RSA-OAEP []` is `ok`).
fn check_usages(name: &str, private: bool, usages: &[String]) -> Result<(), ScErr> {
    let allowed = allowed_usages(name, private);
    if !usages.iter().all(|u| allowed.contains(&u.as_str())) {
        return Err(syntax(USAGES_BAD));
    }
    if usages.is_empty() && private {
        return Err(syntax(USAGES_EMPTY));
    }
    Ok(())
}

/// `EcKeyImportParams.namedCurve` / `EcKeyGenParams.namedCurve`, with Chrome's two distinct errors:
/// absent is a `TypeError`, present-but-unknown (`secp256k1`, `P-257`) is a `NotSupportedError`.
fn named_curve(scope: &mut v8::PinScope, o: Option<v8::Local<v8::Object>>, op: &str, dict: &str) -> Result<String, ScErr> {
    let v = o.and_then(|o| get_prop(scope, o, "namedCurve")).filter(|v| v.is_string());
    let Some(v) = v else {
        return Err(ScErr::Type(sc_where(op, &format!("{dict}: namedCurve: Missing or not a string"))));
    };
    let c = v.to_rust_string_lossy(scope);
    if asym::CURVES.contains(&c.as_str()) {
        return Ok(c);
    }
    Err(ScErr::Dom("NotSupportedError".into(), sc_where(op, &format!("{dict}: Unrecognized namedCurve"))))
}

/// The JWK `alg` an RSA key of this algorithm and hash carries. EC keys have none, which is why
/// this answers only for the three RSA names.
fn rsa_jwk_alg(name: &str, hash: &str) -> Option<String> {
    let n = match hash {
        "SHA-1" => "1",
        "SHA-256" => "256",
        "SHA-384" => "384",
        "SHA-512" => "512",
        _ => return None,
    };
    Some(match name {
        // `RSA-OAEP` with SHA-1 is spelled without a suffix in JWA, and Chrome follows it.
        "RSA-OAEP" => if n == "1" { "RSA-OAEP".to_string() } else { format!("RSA-OAEP-{n}") },
        "RSASSA-PKCS1-v1_5" => format!("RS{n}"),
        "RSA-PSS" => format!("PS{n}"),
        _ => return None,
    })
}

/// A JWK's base64url member as bytes, with Chrome's two errors: absent and undecodable are
/// different sentences and it names the member in both.
fn jwk_b64(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, member: &str) -> Result<Vec<u8>, ScErr> {
    let Some(s) = jwk_str(scope, o, member) else {
        return Err(data_err(&format!("The required JWK member \"{member}\" was missing")));
    };
    wc::b64url_decode(&s).ok_or_else(|| {
        data_err(&format!("The JWK member \"{member}\" could not be base64url decoded or contained padding"))
    })
}

/// The `use`, `key_ops` and `ext` checks, which are the SAME for every key type and are already
/// spelled once for `oct` keys; this is the asymmetric caller of the same three rules.
fn jwk_common(
    scope: &mut v8::PinScope,
    o: v8::Local<v8::Object>,
    usages: &[String],
    extractable: bool,
) -> Result<(), ScErr> {
    if let Some(u) = jwk_str(scope, o, "use") {
        let allowed: &[&str] = match u.as_str() {
            "enc" => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
            "sig" => &["sign", "verify"],
            _ => &[],
        };
        if !usages.iter().all(|x| allowed.contains(&x.as_str())) {
            return Err(data_err(&format!(
                "The JWK \"use\" member was inconsistent with that specified by the Web Crypto call{JWK_SUPERSET}"
            )));
        }
    }
    if let Some(ops) = get_prop(scope, o, "key_ops").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let have: Vec<String> = (0..ops.length())
            .filter_map(|i| ops.get_index(scope, i))
            .filter(|x| x.is_string())
            .map(|x| x.to_rust_string_lossy(scope))
            .collect();
        if !usages.iter().all(|x| have.contains(x)) {
            return Err(data_err(&format!(
                "The JWK \"key_ops\" member was inconsistent with that specified by the Web Crypto call{JWK_SUPERSET}"
            )));
        }
    }
    if let Some(ext) = get_prop(scope, o, "ext").filter(|v| v.is_boolean()) {
        if !ext.boolean_value(scope) && extractable {
            // Chrome's wording, typo and all: "inconsistent what that specified".
            return Err(data_err("The \"ext\" member of the JWK dictionary is inconsistent what that specified by the Web Crypto call"));
        }
    }
    Ok(())
}

// ── importKey ───────────────────────────────────────────────────────────────────────────────────

/// Mint an RSA or EC `CryptoKey`. Shared by `importKey` and `unwrapKey`, which the spec defines as
/// the same step.
pub(super) fn import<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &'static str,
    algo_obj: Option<v8::Local<v8::Object>>,
    format: &str,
    material: v8::Local<v8::Value>,
    extractable: bool,
    usages: Vec<String>,
    op: &str,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let rsa = is_rsa(name);
    // The params dictionary is read BEFORE anything is parsed: `importKey("spki", <a secp256k1
    // key>, {name:"ECDSA", namedCurve:"secp256k1"})` answers `Unrecognized namedCurve` and not a
    // DataError, measured.
    let hash = if rsa { crypto_hash_of(scope, algo_obj) } else { String::new() };
    if rsa && !wc::is_supported_hash(&hash) {
        return Err(ScErr::Dom("NotSupportedError".into(), sc_where(op, "RsaHashedImportParams: hash: Unrecognized name")));
    }
    let want_curve = if rsa { String::new() } else { named_curve(scope, algo_obj, op, "EcKeyImportParams")? };

    match format {
        "spki" => {
            check_usages(name, false, &usages)?;
            let der = crypto_bytes(scope, material).ok_or_else(bad_der)?;
            let key = asym::public_from_spki(&der).ok_or_else(bad_der)?;
            finish_public(scope, name, &key, &hash, &want_curve, extractable, usages)
        }
        "pkcs8" => {
            check_usages(name, true, &usages)?;
            let der = crypto_bytes(scope, material).ok_or_else(bad_der)?;
            let key = asym::private_from_pkcs8(&der).ok_or_else(bad_der)?;
            finish_private(scope, name, &key, &hash, &want_curve, extractable, usages)
        }
        "raw" => {
            // `raw` carries an uncompressed EC POINT, so only the two EC algorithms take it. RSA
            // has no raw form and Chrome says so with the format sentence, not a DataError.
            if rsa {
                return Err(ScErr::Dom("NotSupportedError".into(), BAD_FORMAT_IN.into()));
            }
            check_usages(name, false, &usages)?;
            let point = crypto_bytes(scope, material).ok_or_else(bad_der)?;
            let key = asym::ec_public_from_point(&want_curve, &point).ok_or_else(bad_der)?;
            finish_public(scope, name, &key, &hash, &want_curve, extractable, usages)
        }
        _ => import_jwk(scope, name, material, &hash, &want_curve, extractable, usages),
    }
}

fn finish_public<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    key: &btls::pkey::PKey<btls::pkey::Public>,
    hash: &str,
    want_curve: &str,
    extractable: bool,
    usages: Vec<String>,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let info = asym::info_public(key).ok_or_else(bad_der)?;
    let der = asym::spki_of(key).ok_or_else(bad_der)?;
    let algo = algo_for(scope, name, &info, hash, want_curve)?;
    Ok(make_crypto_key(scope, "public", algo, extractable, usages, der, hash))
}

fn finish_private<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    key: &btls::pkey::PKey<btls::pkey::Private>,
    hash: &str,
    want_curve: &str,
    extractable: bool,
    usages: Vec<String>,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let info = asym::info_private(key).ok_or_else(bad_der)?;
    let der = asym::pkcs8_of(key).ok_or_else(bad_der)?;
    let algo = algo_for(scope, name, &info, hash, want_curve)?;
    Ok(make_crypto_key(scope, "private", algo, extractable, usages, der, hash))
}

/// The `algorithm` object, and the two ways a parsed key can be the WRONG key: an RSA blob under an
/// EC algorithm (empty `DataError`, same as a malformed one) and an EC key on a curve the call did
/// not ask for (a sentence of its own, because the page CAN act on that one).
fn algo_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    info: &asym::Info,
    hash: &str,
    want_curve: &str,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    if is_rsa(name) {
        if info.kind != asym::Kind::Rsa {
            return Err(bad_der());
        }
        return Ok(rsa_algo(scope, name, info.bits, info.e.clone(), hash));
    }
    if info.kind != asym::Kind::Ec {
        return Err(bad_der());
    }
    let curve = info.curve.ok_or_else(bad_der)?;
    if curve != want_curve {
        return Err(data_err("The imported EC key specifies a different curve than requested"));
    }
    Ok(ec_algo(scope, name, curve))
}

fn import_jwk<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &'static str,
    material: v8::Local<v8::Value>,
    hash: &str,
    want_curve: &str,
    extractable: bool,
    usages: Vec<String>,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let o = v8::Local::<v8::Object>::try_from(material)
        .map_err(|_| data_err("JWK input could not be parsed to a JSON dictionary"))?;
    let want_kty = if is_rsa(name) { "RSA" } else { "EC" };
    match jwk_str(scope, o, "kty") {
        None => return Err(data_err("The required JWK member \"kty\" was missing")),
        Some(k) if k != want_kty => {
            return Err(data_err(&format!("The JWK \"kty\" member was not \"{want_kty}\"")))
        }
        Some(_) => {}
    }
    // `d` is what makes a JWK private, for both key kinds.
    let private = jwk_str(scope, o, "d").is_some();
    check_usages(name, private, &usages)?;

    if is_rsa(name) {
        if let Some(alg) = jwk_str(scope, o, "alg") {
            if rsa_jwk_alg(name, hash).as_deref() != Some(alg.as_str()) {
                return Err(data_err(JWK_ALG_INCONSISTENT));
            }
        }
        jwk_common(scope, o, &usages, extractable)?;
        let n = jwk_b64(scope, o, "n")?;
        let e = jwk_b64(scope, o, "e")?;
        if !private {
            let key = asym::rsa_public_from_components(&n, &e).ok_or_else(bad_der)?;
            return finish_public(scope, name, &key, hash, want_curve, extractable, usages);
        }
        // Chrome requires the CRT members and names the FIRST missing one; `p` is first.
        let d = jwk_b64(scope, o, "d")?;
        let p = jwk_b64(scope, o, "p")?;
        let q = jwk_b64(scope, o, "q")?;
        let dp = jwk_b64(scope, o, "dp")?;
        let dq = jwk_b64(scope, o, "dq")?;
        let qi = jwk_b64(scope, o, "qi")?;
        let key = asym::rsa_private_from_components(&n, &e, &d, &p, &q, &dp, &dq, &qi).ok_or_else(bad_der)?;
        return finish_private(scope, name, &key, hash, want_curve, extractable, usages);
    }

    // EC. `crv` is checked against the call's `namedCurve` before the coordinates are read.
    match jwk_str(scope, o, "crv") {
        None => return Err(data_err("The required JWK member \"crv\" was missing")),
        Some(c) if c != want_curve => {
            return Err(data_err("The JWK's \"crv\" member specifies a different curve than requested"))
        }
        Some(_) => {}
    }
    jwk_common(scope, o, &usages, extractable)?;
    let width = asym::field_bytes(want_curve);
    let x = jwk_b64(scope, o, "x")?;
    let y = jwk_b64(scope, o, "y")?;
    for (member, v) in [("x", &x), ("y", &y)] {
        if v.len() != width {
            return Err(data_err(&format!(
                "The JWK's \"{member}\" member defines an octet string of length {} bytes but should be {width}",
                v.len()
            )));
        }
    }
    if !private {
        let key = asym::ec_public_from_xy(want_curve, &x, &y).ok_or_else(bad_der)?;
        return finish_public(scope, name, &key, hash, want_curve, extractable, usages);
    }
    let d = jwk_b64(scope, o, "d")?;
    if d.len() != width {
        return Err(data_err(&format!(
            "The JWK's \"d\" member defines an octet string of length {} bytes but should be {width}",
            d.len()
        )));
    }
    let key = asym::ec_private_from_components(want_curve, &x, &y, &d).ok_or_else(bad_der)?;
    finish_private(scope, name, &key, hash, want_curve, extractable, usages)
}

// ── exportKey ───────────────────────────────────────────────────────────────────────────────────

/// `exportKey` for an RSA or EC key. `der` is the stored canonical encoding — SPKI for a public key
/// and PKCS#8 for a private one — so `spki`/`pkcs8` are the bytes themselves.
pub(super) fn export<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    private: bool,
    der: &[u8],
    hash: &str,
    usages: &[String],
    extractable: bool,
    format: &str,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let wrong_type = || ScErr::Dom("InvalidAccessError".into(), WRONG_TYPE.to_string());
    match format {
        "spki" => {
            if private {
                return Err(wrong_type());
            }
            Ok(crypto_ab(scope, der.to_vec()))
        }
        "pkcs8" => {
            if !private {
                return Err(wrong_type());
            }
            Ok(crypto_ab(scope, der.to_vec()))
        }
        "raw" => {
            // The uncompressed point of a PUBLIC EC key, and nothing else. A private EC key and an
            // RSA key of either type are both `InvalidAccessError: The key is not of the expected
            // type`, measured for the private one.
            if is_rsa(name) {
                return Err(ScErr::Dom("NotSupportedError".into(), BAD_FORMAT_OUT.into()));
            }
            if private {
                return Err(wrong_type());
            }
            let key = asym::public_from_spki(der).ok_or_else(bad_der)?;
            Ok(crypto_ab(scope, asym::ec_point_of(&key).ok_or_else(bad_der)?))
        }
        _ => export_jwk(scope, name, private, der, hash, usages, extractable),
    }
}

/// A JWK with Chrome's member order, which is ALPHABETICAL and therefore what `JSON.stringify` —
/// and `wrapKey("jwk", ..)`'s plaintext — prints:
///   RSA public  `alg, e, ext, key_ops, kty, n`
///   RSA private `alg, d, dp, dq, e, ext, key_ops, kty, n, p, q, qi`
///   EC  public  `crv, ext, key_ops, kty, x, y`
///   EC  private `crv, d, ext, key_ops, kty, x, y`
fn export_jwk<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    private: bool,
    der: &[u8],
    hash: &str,
    usages: &[String],
    extractable: bool,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let ext_v: v8::Local<v8::Value> = v8::Boolean::new(scope, extractable).into();
    let ops: Vec<v8::Local<v8::Value>> = usages.iter().map(|u| str_val(scope, u)).collect();
    let ops_v: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &ops).into();

    if is_rsa(name) {
        let alg = rsa_jwk_alg(name, hash)
            .ok_or_else(|| ScErr::Dom("NotSupportedError".into(), BAD_FORMAT_OUT.into()))?;
        let alg_v = str_val(scope, &alg);
        let kty_v = str_val(scope, "RSA");
        if !private {
            let key = asym::public_from_spki(der).ok_or_else(bad_der)?;
            let (n, e) = asym::rsa_public_components(&key).ok_or_else(bad_der)?;
            let n_v = str_val(scope, &wc::b64url_encode(&n));
            let e_v = str_val(scope, &wc::b64url_encode(&e));
            return Ok(crypto_obj(scope, &[
                ("alg", alg_v), ("e", e_v), ("ext", ext_v), ("key_ops", ops_v), ("kty", kty_v), ("n", n_v),
            ]));
        }
        let key = asym::private_from_pkcs8(der).ok_or_else(bad_der)?;
        let c = asym::rsa_private_components(&key).ok_or_else(bad_der)?;
        let [n, e, d, p, q, dp, dq, qi] = c;
        let n_v = str_val(scope, &wc::b64url_encode(&n));
        let e_v = str_val(scope, &wc::b64url_encode(&e));
        let d_v = str_val(scope, &wc::b64url_encode(&d));
        let p_v = str_val(scope, &wc::b64url_encode(&p));
        let q_v = str_val(scope, &wc::b64url_encode(&q));
        let dp_v = str_val(scope, &wc::b64url_encode(&dp));
        let dq_v = str_val(scope, &wc::b64url_encode(&dq));
        let qi_v = str_val(scope, &wc::b64url_encode(&qi));
        return Ok(crypto_obj(scope, &[
            ("alg", alg_v), ("d", d_v), ("dp", dp_v), ("dq", dq_v), ("e", e_v), ("ext", ext_v),
            ("key_ops", ops_v), ("kty", kty_v), ("n", n_v), ("p", p_v), ("q", q_v), ("qi", qi_v),
        ]));
    }

    let kty_v = str_val(scope, "EC");
    if !private {
        let key = asym::public_from_spki(der).ok_or_else(bad_der)?;
        let curve = asym::info_public(&key).and_then(|i| i.curve).ok_or_else(bad_der)?;
        let (x, y) = asym::ec_xy_of(&key).ok_or_else(bad_der)?;
        let crv_v = str_val(scope, curve);
        let x_v = str_val(scope, &wc::b64url_encode(&x));
        let y_v = str_val(scope, &wc::b64url_encode(&y));
        return Ok(crypto_obj(scope, &[
            ("crv", crv_v), ("ext", ext_v), ("key_ops", ops_v), ("kty", kty_v), ("x", x_v), ("y", y_v),
        ]));
    }
    let key = asym::private_from_pkcs8(der).ok_or_else(bad_der)?;
    let curve = asym::info_private(&key).and_then(|i| i.curve).ok_or_else(bad_der)?;
    let (x, y, d) = asym::ec_xyd_of(&key).ok_or_else(bad_der)?;
    let crv_v = str_val(scope, curve);
    let d_v = str_val(scope, &wc::b64url_encode(&d));
    let x_v = str_val(scope, &wc::b64url_encode(&x));
    let y_v = str_val(scope, &wc::b64url_encode(&y));
    Ok(crypto_obj(scope, &[
        ("crv", crv_v), ("d", d_v), ("ext", ext_v), ("key_ops", ops_v), ("kty", kty_v), ("x", x_v), ("y", y_v),
    ]))
}

// ── the operations ──────────────────────────────────────────────────────────────────────────────

/// `sign` for the three asymmetric signature algorithms. `hash` is the KEY's for the two RSA ones
/// and the PARAMS' for ECDSA, which is why they are read separately.
pub(super) fn sign<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    o: Option<v8::Local<v8::Object>>,
    der: &[u8],
    key_hash: &str,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let key = asym::private_from_pkcs8(der).ok_or_else(|| op_err(""))?;
    let sig = match name {
        "RSASSA-PKCS1-v1_5" => asym::rsa_pkcs1_sign(&key, key_hash, data),
        "RSA-PSS" => {
            let salt = pss_salt_len(scope, o, "sign")?;
            asym::rsa_pss_sign(&key, key_hash, salt, data)
        }
        "ECDSA" => {
            let hash = ecdsa_hash(scope, o, "sign")?;
            asym::ecdsa_sign(&key, &hash, data)
        }
        _ => None,
    };
    Ok(crypto_ab(scope, sig.ok_or_else(|| op_err(""))?))
}

pub(super) fn verify<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    o: Option<v8::Local<v8::Object>>,
    der: &[u8],
    key_hash: &str,
    sig: &[u8],
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let key = asym::public_from_spki(der).ok_or_else(|| op_err(""))?;
    let ok = match name {
        "RSASSA-PKCS1-v1_5" => asym::rsa_pkcs1_verify(&key, key_hash, sig, data),
        "RSA-PSS" => {
            let salt = pss_salt_len(scope, o, "verify")?;
            asym::rsa_pss_verify(&key, key_hash, salt, sig, data)
        }
        "ECDSA" => {
            let hash = ecdsa_hash(scope, o, "verify")?;
            asym::ecdsa_verify(&key, &hash, sig, data)
        }
        _ => false,
    };
    Ok(v8::Boolean::new(scope, ok).into())
}

/// `RsaPssParams.saltLength`, a REQUIRED `[EnforceRange] unsigned long`. Chrome names it when it is
/// absent and lets BoringSSL refuse one too large for the modulus (a bare `OperationError`).
fn pss_salt_len(scope: &mut v8::PinScope, o: Option<v8::Local<v8::Object>>, op: &str) -> Result<i32, ScErr> {
    let v = o.and_then(|o| get_prop(scope, o, "saltLength")).filter(|v| !v.is_undefined() && !v.is_null());
    let Some(v) = v else {
        return Err(ScErr::Type(sc_where(op, "RsaPssParams: saltLength: Missing required property")));
    };
    let n = v.number_value(scope).unwrap_or(-1.0);
    if !(0.0..=f64::from(i32::MAX)).contains(&n) {
        return Err(ScErr::Type(sc_where(op, "RsaPssParams: saltLength: Outside of numeric range")));
    }
    Ok(n as i32)
}

/// `EcdsaParams.hash`, required — the digest is not on the key for ECDSA.
fn ecdsa_hash(scope: &mut v8::PinScope, o: Option<v8::Local<v8::Object>>, op: &str) -> Result<String, ScErr> {
    let present = o
        .and_then(|o| get_prop(scope, o, "hash"))
        .is_some_and(|v| !v.is_undefined() && !v.is_null());
    if !present {
        return Err(ScErr::Type(sc_where(op, "EcdsaParams: hash: Missing or not an AlgorithmIdentifier")));
    }
    let hash = crypto_hash_of(scope, o);
    if !wc::is_supported_hash(&hash) {
        return Err(ScErr::Dom("NotSupportedError".into(), sc_where(op, "EcdsaParams: Unrecognized name")));
    }
    Ok(hash)
}

/// `RSA-OAEP` encrypt/decrypt. `RsaOaepParams.label` is optional and defaults to empty.
pub(super) fn oaep<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: Option<v8::Local<v8::Object>>,
    private: bool,
    der: &[u8],
    key_hash: &str,
    data: &[u8],
    decrypting: bool,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let label = o
        .and_then(|o| get_prop(scope, o, "label"))
        .filter(|v| !v.is_undefined() && !v.is_null())
        .and_then(|v| crypto_bytes(scope, v))
        .unwrap_or_default();
    let out = if decrypting {
        if !private {
            return Err(op_err(""));
        }
        let key = asym::private_from_pkcs8(der).ok_or_else(|| op_err(""))?;
        asym::rsa_oaep_decrypt(&key, key_hash, &label, data)
    } else {
        if private {
            return Err(op_err(""));
        }
        let key = asym::public_from_spki(der).ok_or_else(|| op_err(""))?;
        asym::rsa_oaep_encrypt(&key, key_hash, &label, data)
    };
    Ok(crypto_ab(scope, out.ok_or_else(|| op_err(""))?))
}

/// `ECDH` `deriveBits`. `length` is in BITS and Chrome truncates to them, not to whole bytes.
pub(super) fn ecdh_bits(
    scope: &mut v8::PinScope,
    o: Option<v8::Local<v8::Object>>,
    der: &[u8],
    length: v8::Local<v8::Value>,
    op: &str,
) -> Result<Vec<u8>, ScErr> {
    let pub_v = o.and_then(|o| get_prop(scope, o, "public")).filter(|v| !v.is_undefined() && !v.is_null());
    let Some(pub_v) = pub_v else {
        return Err(ScErr::Type(sc_where(op, "EcdhKeyDeriveParams: public: Missing required property")));
    };
    let not_public = || {
        ScErr::Dom(
            "InvalidAccessError".into(),
            "The public parameter for ECDH key derivation is not a public EC key".to_string(),
        )
    };
    let peer = super::subtle_crypto::asym_peer(scope, pub_v).ok_or_else(not_public)?;
    let (peer_type, peer_der) = peer;
    if peer_type != "public" {
        return Err(not_public());
    }
    let peer_key = asym::public_from_spki(&peer_der).ok_or_else(not_public)?;
    let priv_key = asym::private_from_pkcs8(der).ok_or_else(|| op_err(""))?;
    let curve = asym::info_private(&priv_key).and_then(|i| i.curve).ok_or_else(|| op_err(""))?;
    let max = asym::field_bits(curve);
    // `null` means "all of it", which is the curve's own width.
    let bits = if length.is_null_or_undefined() {
        max
    } else {
        let n = length.number_value(scope).unwrap_or(-1.0);
        if n < 0.0 {
            return Err(op_err(""));
        }
        n as usize
    };
    if bits > max {
        return Err(op_err(&format!(
            "Length specified for ECDH key derivation is too large. Maximum allowed is {max} bits"
        )));
    }
    let secret = asym::ecdh_derive(&priv_key, &peer_key).ok_or_else(|| op_err(""))?;
    Ok(asym::truncate_bits(&secret, bits))
}

// ── generateKey ─────────────────────────────────────────────────────────────────────────────────

/// A generated key pair, with the usages SPLIT: the public half keeps the ones a public key can
/// hold and the private half the rest. `generateKey RSA-OAEP 2048` with `["encrypt","decrypt"]`
/// answers `["encrypt"]` and `["decrypt"]`, measured.
fn split<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    hash: &str,
    spki: Vec<u8>,
    pkcs8: Vec<u8>,
    extractable: bool,
    usages: &[String],
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let pick = |private: bool| -> Vec<String> {
        let allowed = allowed_usages(name, private);
        canonical_usages(usages.iter().filter(|u| allowed.contains(&u.as_str())).cloned().collect())
    };
    let pubk = {
        let key = asym::public_from_spki(&spki).ok_or_else(|| op_err(""))?;
        let info = asym::info_public(&key).ok_or_else(|| op_err(""))?;
        let algo = if is_rsa(name) {
            rsa_algo(scope, name, info.bits, info.e.clone(), hash)
        } else {
            ec_algo(scope, name, info.curve.ok_or_else(|| op_err(""))?)
        };
        // A public key is always extractable, whatever the call asked for.
        make_crypto_key(scope, "public", algo, true, pick(false), spki, hash)
    };
    let privk = {
        let key = asym::private_from_pkcs8(&pkcs8).ok_or_else(|| op_err(""))?;
        let info = asym::info_private(&key).ok_or_else(|| op_err(""))?;
        let algo = if is_rsa(name) {
            rsa_algo(scope, name, info.bits, info.e.clone(), hash)
        } else {
            ec_algo(scope, name, info.curve.ok_or_else(|| op_err(""))?)
        };
        make_crypto_key(scope, "private", algo, extractable, pick(true), pkcs8, hash)
    };
    Ok(crypto_obj(scope, &[("privateKey", privk), ("publicKey", pubk)]))
}

pub(super) fn generate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &'static str,
    o: Option<v8::Local<v8::Object>>,
    extractable: bool,
    usages: Vec<String>,
) -> Result<v8::Local<'s, v8::Value>, ScErr> {
    let op = "generateKey";
    // NORMALIZE THE ALGORITHM FIRST, then check the usages. The order is observable and WPT asserts
    // it: `generateKey/failures_bad_algorithm.https.any.js` passes an unrecognized `hash` together
    // with usages the key cannot hold and requires `NotSupportedError`, not `SyntaxError`. Written
    // the other way round first, which cost 32 of that file's 360 subtests -- the ONLY regression
    // the before/after sweep showed, and the reason the sweep is run at all.
    let mut rsa_hash = String::new();
    let mut rsa_bits = 0.0f64;
    let mut ec_curve = String::new();
    if is_rsa(name) {
        let present = o
            .and_then(|o| get_prop(scope, o, "hash"))
            .is_some_and(|v| !v.is_undefined() && !v.is_null());
        if !present {
            return Err(ScErr::Type(sc_where(op, "RsaHashedKeyGenParams: hash: Missing or not an AlgorithmIdentifier")));
        }
        rsa_hash = crypto_hash_of(scope, o);
        if !wc::is_supported_hash(&rsa_hash) {
            return Err(ScErr::Dom("NotSupportedError".into(), sc_where(op, "RsaHashedKeyGenParams: Unrecognized name")));
        }
        rsa_bits = o
            .and_then(|o| get_prop(scope, o, "modulusLength"))
            .and_then(|v| v.number_value(scope))
            .unwrap_or(-1.0);
        if !(0.0..=f64::from(u32::MAX)).contains(&rsa_bits) {
            return Err(ScErr::Type(sc_where(op, "RsaHashedKeyGenParams: modulusLength: Outside of numeric range")));
        }
    } else {
        ec_curve = named_curve(scope, o, op, "EcKeyGenParams")?;
    }
    // A usage neither half can hold is refused before any key is made.
    let any: Vec<&str> = allowed_usages(name, false).iter().chain(allowed_usages(name, true)).copied().collect();
    if !usages.iter().all(|u| any.contains(&u.as_str())) || usages.is_empty() {
        return Err(syntax(USAGES_BAD));
    }
    if is_rsa(name) {
        let hash = rsa_hash;
        let bits = rsa_bits;
        let e = o
            .and_then(|o| get_prop(scope, o, "publicExponent"))
            .filter(|v| !v.is_undefined() && !v.is_null())
            .and_then(|v| crypto_bytes(scope, v))
            .unwrap_or_default();
        // The ONE exponent rule Chrome applies here and not on import, measured.
        let as_num = e.iter().fold(0u64, |n, &x| n.wrapping_mul(256).wrapping_add(u64::from(x)));
        if as_num != 3 && as_num != 65537 {
            return Err(op_err("The \"publicExponent\" must be either 3 or 65537"));
        }
        let (spki, pkcs8) = asym::rsa_generate(bits as u32, &e).ok_or_else(|| op_err(""))?;
        return split(scope, name, &hash, spki, pkcs8, extractable, &usages);
    }
    let (spki, pkcs8) = asym::ec_generate(&ec_curve).ok_or_else(|| op_err(""))?;
    split(scope, name, "", spki, pkcs8, extractable, &usages)
}

// ── wrapKey ─────────────────────────────────────────────────────────────────────────────────────

/// The bytes `wrapKey(format, <an asymmetric key>, ..)` encrypts. `raw` and `jwk` route through the
/// very functions `exportKey` answers with, so the two spellings cannot drift.
pub(super) fn wrap_bytes<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    private: bool,
    der: &[u8],
    hash: &str,
    usages: &[String],
    extractable: bool,
    format: &str,
) -> Result<Vec<u8>, ScErr> {
    let v = export(scope, name, private, der, hash, usages, extractable, format)?;
    if format == "jwk" {
        let s = v8::json::stringify(scope, v).ok_or_else(|| op_err(""))?;
        return Ok(s.to_rust_string_lossy(scope).into_bytes());
    }
    crypto_bytes(scope, v).ok_or_else(|| op_err(""))
}
