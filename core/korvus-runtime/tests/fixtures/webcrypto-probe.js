// PARITY WC1 + RSA1 — `crypto.subtle`'s algorithm and key-format gates, and RSA/EC key import.
// Evaluated UNMODIFIED by `tests/webcrypto_live.rs` and by `google-chrome --headless=new`
// 150.0.7871.46 over CDP, so the two outputs diff line for line.
// `webcrypto-probe.chrome-150.txt` is Chrome's output, RECORDED (two runs, identical), not written.
// A line that differs is a finding about the engine, never a reason to edit the file.
//
// THE RECORDING IS FALSIFICATION-CHECKED, twice. WC1 corrupted two hex digits of its 213 lines and
// confirmed the harness named exactly those two. RSA1 did the same to the 339: two digits, in
// `RSASSA-PKCS1-v1_5 sign SHA-256` and `ECDH deriveBits 256`, and the harness reported
// `2 of 339 lines differ` naming exactly those -- with the engine's own column carrying Chrome's
// real bytes. A green comparison that is not running looks exactly like one that passes.
//
// NOTHING HERE TOUCHES THE NETWORK: WebCrypto is local, so `BASE` is taken and ignored, and this file
// shares its shape with the other `*-probe.js` fixtures beside it.
//
// EVERY KEY AND IV IS FIXED, so the ciphertext lines are KNOWN-ANSWER TESTS and not round-trips: a
// guard widened without the cipher behind it cannot pass them. The AES-KW vector is RFC 3394 §4.1.
//
// RSA AND THE P-CURVES ARE IN, as of PARITY RSA1: section 12 is 126 more lines covering
// RSA-OAEP / RSASSA-PKCS1-v1_5 / RSA-PSS and ECDSA / ECDH on P-256, P-384 and P-521, in all four
// formats. Its key material is FIXED for the same reason the AES keys are -- a signature, a derived
// secret and an export are only known-answer tests if the inputs never move.
(async (BASE) => {
  const R = [];
  const E = (e) => "THREW " + (e && e.name) + ": " + (e && e.message);
  const t = async (name, f) => {
    try { R.push(name + " = " + (await Promise.resolve().then(f))); } catch (e) { R.push(name + " = " + E(e)); }
  };
  const hex = (b) => Array.from(new Uint8Array(b)).map((x) => x.toString(16).padStart(2, "0")).join("");
  const seq = (n, from) => new Uint8Array(Array.from({ length: n }, (_, i) => from + i));
  const b64u = (b) => btoa(String.fromCharCode.apply(null, Array.from(new Uint8Array(b)))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const S = crypto.subtle;

  const K16 = seq(16, 0x00), K24 = seq(24, 0x00), K32 = seq(32, 0x00);
  const IV16 = seq(16, 0x10), IV12 = seq(12, 0x20);
  const DATA = new TextEncoder().encode("The quick brown fox jumps over the lazy dog.");
  const AAD = new Uint8Array([1, 2, 3]);
  const BOTH = ["encrypt", "decrypt"];

  const imp = (fmt, data, algo, ext, use) => S.importKey(fmt, data, algo, ext, use);
  const raw = (data, algo, use, ext) => imp("raw", data, algo, ext === undefined ? true : ext, use);

  // ── 1. which algorithm names `encrypt` and `decrypt` NORMALIZE ────────────────────────────────
  // The key is always an AES-CBC key, so a name that normalizes reaches the key check and answers
  // InvalidAccessError; a name that does not answers the normalization failure. The two are
  // distinguishable, which is the point: this section reads the GATE, not the cipher.
  const cbcKey = await raw(K16, "AES-CBC", BOTH);
  const NAMES = ["AES-CBC", "AES-CTR", "AES-GCM", "AES-KW", "HMAC", "PBKDF2", "HKDF", "RSA-OAEP",
                 "X25519", "ECDH", "ECDSA", "aes-gcm", "AES-GCM ", "NOPE", ""];
  for (const n of NAMES) {
    const p = { name: n, counter: IV16, length: 64, iv: IV16 };
    await t("encrypt name " + JSON.stringify(n), async () => { await S.encrypt(p, cbcKey, DATA); return "ok"; });
  }
  for (const n of ["AES-GCM", "AES-KW", "NOPE"]) {
    const p = { name: n, counter: IV16, length: 64, iv: IV16 };
    await t("decrypt name " + JSON.stringify(n), async () => { await S.decrypt(p, cbcKey, DATA); return "ok"; });
  }
  await t("encrypt algorithm as a bare string", async () => { await S.encrypt("AES-CBC", cbcKey, DATA); return "ok"; });
  await t("encrypt algorithm missing name", async () => { await S.encrypt({}, cbcKey, DATA); return "ok"; });

  // ── 2. AES-CBC ────────────────────────────────────────────────────────────────────────────────
  await t("AES-CBC-128 encrypt", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, DATA)));
  await t("AES-CBC-128 encrypt empty", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, new Uint8Array(0))));
  await t("AES-CBC-128 encrypt 16 bytes", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, seq(16, 0x41))));
  await t("AES-CBC-192 encrypt", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, await raw(K24, "AES-CBC", BOTH), DATA)));
  await t("AES-CBC-256 encrypt", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, await raw(K32, "AES-CBC", BOTH), DATA)));
  await t("AES-CBC round trip", async () => new TextDecoder().decode(await S.decrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, DATA))));
  await t("AES-CBC decrypt bad padding", async () => hex(await S.decrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, seq(16, 0x90))));
  await t("AES-CBC decrypt short block", async () => hex(await S.decrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, seq(15, 0x90))));
  await t("AES-CBC decrypt no data", async () => hex(await S.decrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, new Uint8Array(0))));
  await t("AES-CBC iv 15 bytes", async () => hex(await S.encrypt({ name: "AES-CBC", iv: seq(15, 0x10) }, cbcKey, DATA)));
  await t("AES-CBC iv missing", async () => hex(await S.encrypt({ name: "AES-CBC" }, cbcKey, DATA)));
  await t("AES-CBC key algorithm", async () => JSON.stringify(cbcKey.algorithm));

  // ── 3. AES-CTR ────────────────────────────────────────────────────────────────────────────────
  const ctrKey = await raw(K16, "AES-CTR", BOTH);
  await t("AES-CTR-128 encrypt", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 64 }, ctrKey, DATA)));
  await t("AES-CTR length 128", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 128 }, ctrKey, DATA)));
  await t("AES-CTR counter 15 bytes", async () => hex(await S.encrypt({ name: "AES-CTR", counter: seq(15, 0x10), length: 64 }, ctrKey, DATA)));
  await t("AES-CTR length 0", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 0 }, ctrKey, DATA)));
  await t("AES-CTR length 129", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 129 }, ctrKey, DATA)));

  // ── 4. AES-GCM ────────────────────────────────────────────────────────────────────────────────
  const gcmKey = await raw(K16, "AES-GCM", BOTH);
  await t("AES-GCM-128 encrypt", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, DATA)));
  await t("AES-GCM-128 encrypt empty", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, new Uint8Array(0))));
  await t("AES-GCM-128 with aad", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, additionalData: AAD }, gcmKey, DATA)));
  await t("AES-GCM-128 tagLength 32", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 32 }, gcmKey, DATA)));
  await t("AES-GCM-128 tagLength 96", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 96 }, gcmKey, DATA)));
  await t("AES-GCM-128 tagLength 127", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 127 }, gcmKey, DATA)));
  await t("AES-GCM-256 encrypt", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12 }, await raw(K32, "AES-GCM", BOTH), DATA)));
  await t("AES-GCM iv 16 bytes", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV16 }, gcmKey, DATA)));
  await t("AES-GCM iv 8 bytes", async () => hex(await S.encrypt({ name: "AES-GCM", iv: seq(8, 0x20) }, gcmKey, DATA)));
  await t("AES-GCM iv empty", async () => hex(await S.encrypt({ name: "AES-GCM", iv: new Uint8Array(0) }, gcmKey, DATA)));
  await t("AES-GCM iv missing", async () => hex(await S.encrypt({ name: "AES-GCM" }, gcmKey, DATA)));
  await t("AES-GCM round trip", async () => new TextDecoder().decode(await S.decrypt({ name: "AES-GCM", iv: IV12, additionalData: AAD }, gcmKey, await S.encrypt({ name: "AES-GCM", iv: IV12, additionalData: AAD }, gcmKey, DATA))));
  await t("AES-GCM decrypt wrong aad", async () => hex(await S.decrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, await S.encrypt({ name: "AES-GCM", iv: IV12, additionalData: AAD }, gcmKey, DATA))));
  await t("AES-GCM decrypt tampered", async () => {
    const ct = new Uint8Array(await S.encrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, DATA));
    ct[0] ^= 1;
    return hex(await S.decrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, ct));
  });
  await t("AES-GCM decrypt too short", async () => hex(await S.decrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, seq(8, 0))));

  // ── 5. AES-KW ─────────────────────────────────────────────────────────────────────────────────
  // RFC 3394 §4.1: KEK 000102..0f, key 00112233445566778899aabbccddeeff.
  const kek = await imp("raw", seq(16, 0x00), "AES-KW", true, ["wrapKey", "unwrapKey"]);
  const rfc = await imp("raw", new Uint8Array([0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]), "AES-CBC", true, BOTH);
  await t("AES-KW rfc3394 4.1", async () => hex(await S.wrapKey("raw", rfc, kek, "AES-KW")));
  await t("AES-KW unwrap round trip", async () => {
    const w = await S.wrapKey("raw", rfc, kek, "AES-KW");
    const k = await S.unwrapKey("raw", w, kek, "AES-KW", "AES-CBC", true, BOTH);
    return hex(await S.exportKey("raw", k));
  });
  await t("AES-KW encrypt", async () => hex(await S.encrypt({ name: "AES-KW" }, kek, DATA)));
  await t("AES-KW wrap 20 bytes", async () => hex(await S.wrapKey("raw", await imp("raw", seq(20, 0), { name: "HMAC", hash: "SHA-256" }, true, ["sign"]), kek, "AES-KW")));

  // ── 6. wrapKey / unwrapKey with the AES ciphers ───────────────────────────────────────────────
  const wrapCbc = await raw(K16, "AES-CBC", ["wrapKey", "unwrapKey"]);
  const wrapGcm = await raw(K16, "AES-GCM", ["wrapKey", "unwrapKey"]);
  await t("wrapKey AES-CBC raw", async () => hex(await S.wrapKey("raw", rfc, wrapCbc, { name: "AES-CBC", iv: IV16 })));
  await t("wrapKey AES-GCM raw", async () => hex(await S.wrapKey("raw", rfc, wrapGcm, { name: "AES-GCM", iv: IV12 })));
  await t("wrapKey AES-GCM jwk round trip", async () => {
    const w = await S.wrapKey("jwk", rfc, wrapGcm, { name: "AES-GCM", iv: IV12 });
    const k = await S.unwrapKey("jwk", w, wrapGcm, { name: "AES-GCM", iv: IV12 }, "AES-CBC", true, BOTH);
    return hex(await S.exportKey("raw", k));
  });
  await t("wrapKey unextractable", async () => hex(await S.wrapKey("raw", await raw(K16, "AES-CBC", BOTH, false), wrapCbc, { name: "AES-CBC", iv: IV16 })));
  await t("wrapKey format bogus", async () => hex(await S.wrapKey("bogus", rfc, wrapCbc, { name: "AES-CBC", iv: IV16 })));
  await t("wrapKey format pkcs8 on a secret key", async () => hex(await S.wrapKey("pkcs8", rfc, wrapCbc, { name: "AES-CBC", iv: IV16 })));
  await t("wrapKey format spki on a secret key", async () => hex(await S.wrapKey("spki", rfc, wrapCbc, { name: "AES-CBC", iv: IV16 })));
  await t("wrapKey with a key lacking wrapKey", async () => hex(await S.wrapKey("raw", rfc, cbcKey, { name: "AES-CBC", iv: IV16 })));
  await t("unwrapKey format bogus", async () => hex(await S.unwrapKey("bogus", IV16, wrapCbc, { name: "AES-CBC", iv: IV16 }, "AES-CBC", true, BOTH)));

  // ── 7. importKey / exportKey: which format each algorithm takes ───────────────────────────────
  const FORMATS = ["raw", "jwk", "pkcs8", "spki", "bogus"];
  const JWK = { kty: "oct", k: b64u(K16), ext: true };
  for (const alg of ["AES-CBC", "AES-CTR", "AES-GCM", "AES-KW", "HMAC", "PBKDF2", "HKDF"]) {
    const use = alg === "AES-KW" ? ["wrapKey", "unwrapKey"]
      : alg === "HMAC" ? ["sign", "verify"]
        : (alg === "PBKDF2" || alg === "HKDF") ? ["deriveBits"] : BOTH;
    const ext = !(alg === "PBKDF2" || alg === "HKDF");
    const a = alg === "HMAC" ? { name: "HMAC", hash: "SHA-256" } : alg;
    for (const f of FORMATS) {
      const data = f === "jwk" ? JWK : K16;
      await t("importKey " + alg + " " + f, async () => {
        const k = await imp(f, data, a, ext, use);
        return "ok " + k.type + " " + JSON.stringify(k.algorithm) + " " + JSON.stringify(k.usages) + " ext=" + k.extractable;
      });
    }
  }

  // ── 8. jwk's own fields ───────────────────────────────────────────────────────────────────────
  const jwkCase = async (label, jwk, algo, use, ext) => t("jwk " + label, async () => {
    const k = await imp("jwk", jwk, algo, ext === undefined ? true : ext, use || BOTH);
    return "ok " + JSON.stringify(k.algorithm) + " " + JSON.stringify(k.usages);
  });
  await jwkCase("alg A128CBC-correct", { kty: "oct", k: b64u(K16), alg: "A128CBC", ext: true }, "AES-CBC");
  await jwkCase("alg A256CBC-mismatched", { kty: "oct", k: b64u(K16), alg: "A256CBC", ext: true }, "AES-CBC");
  await jwkCase("alg A128GCM-on-AES-CBC", { kty: "oct", k: b64u(K16), alg: "A128GCM", ext: true }, "AES-CBC");
  await jwkCase("alg A128GCM-correct", { kty: "oct", k: b64u(K16), alg: "A128GCM", ext: true }, "AES-GCM");
  await jwkCase("alg A128CTR-is-not-a-thing", { kty: "oct", k: b64u(K16), alg: "A128CTR", ext: true }, "AES-CTR");
  await jwkCase("alg A128KW-correct", { kty: "oct", k: b64u(K16), alg: "A128KW", ext: true }, "AES-KW", ["wrapKey", "unwrapKey"]);
  await jwkCase("alg HS256-correct", { kty: "oct", k: b64u(K32), alg: "HS256", ext: true }, { name: "HMAC", hash: "SHA-256" }, ["sign", "verify"]);
  await jwkCase("alg HS512-mismatched", { kty: "oct", k: b64u(K32), alg: "HS512", ext: true }, { name: "HMAC", hash: "SHA-256" }, ["sign", "verify"]);
  await jwkCase("kty RSA", { kty: "RSA", k: b64u(K16), ext: true }, "AES-CBC");
  await jwkCase("kty missing", { k: b64u(K16), ext: true }, "AES-CBC");
  await jwkCase("k missing", { kty: "oct", ext: true }, "AES-CBC");
  await jwkCase("k not base64url", { kty: "oct", k: "!!!!", ext: true }, "AES-CBC");
  await jwkCase("k padded base64", { kty: "oct", k: "AAAAAAAAAAAAAAAAAAAAAA==", ext: true }, "AES-CBC");
  await jwkCase("k 15 bytes", { kty: "oct", k: b64u(seq(15, 0)), ext: true }, "AES-CBC");
  await jwkCase("ext false imported extractable", { kty: "oct", k: b64u(K16), ext: false }, "AES-CBC");
  await jwkCase("ext absent", { kty: "oct", k: b64u(K16) }, "AES-CBC");
  await jwkCase("key_ops agreeing", { kty: "oct", k: b64u(K16), ext: true, key_ops: ["encrypt", "decrypt"] }, "AES-CBC");
  await jwkCase("key_ops narrower", { kty: "oct", k: b64u(K16), ext: true, key_ops: ["encrypt"] }, "AES-CBC");
  await jwkCase("use sig on an AES key", { kty: "oct", k: b64u(K16), ext: true, use: "sig" }, "AES-CBC");
  await jwkCase("use enc", { kty: "oct", k: b64u(K16), ext: true, use: "enc" }, "AES-CBC");
  await t("jwk as a JSON string", async () => { await imp("jwk", JSON.stringify(JWK), "AES-CBC", true, BOTH); return "ok"; });
  await t("jwk as a BufferSource", async () => { await imp("jwk", K16, "AES-CBC", true, BOTH); return "ok"; });
  await t("raw as a plain object", async () => { await imp("raw", JWK, "AES-CBC", true, BOTH); return "ok"; });

  // ── 9. exportKey ──────────────────────────────────────────────────────────────────────────────
  const stable = (o) => JSON.stringify(o);
  await t("exportKey jwk AES-CBC-128", async () => stable(await S.exportKey("jwk", cbcKey)));
  await t("exportKey jwk AES-CBC-256", async () => stable(await S.exportKey("jwk", await raw(K32, "AES-CBC", BOTH))));
  await t("exportKey jwk AES-CBC-192", async () => stable(await S.exportKey("jwk", await raw(K24, "AES-CBC", BOTH))));
  await t("exportKey jwk AES-CTR", async () => stable(await S.exportKey("jwk", ctrKey)));
  await t("exportKey jwk AES-GCM-256", async () => stable(await S.exportKey("jwk", await raw(K32, "AES-GCM", BOTH))));
  await t("exportKey jwk AES-KW", async () => stable(await S.exportKey("jwk", kek)));
  await t("exportKey jwk HMAC SHA-256", async () => stable(await S.exportKey("jwk", await imp("raw", K32, { name: "HMAC", hash: "SHA-256" }, true, ["sign", "verify"]))));
  await t("exportKey jwk HMAC SHA-1", async () => stable(await S.exportKey("jwk", await imp("raw", K32, { name: "HMAC", hash: "SHA-1" }, true, ["sign"]))));
  await t("exportKey jwk HMAC SHA-384", async () => stable(await S.exportKey("jwk", await imp("raw", K32, { name: "HMAC", hash: "SHA-384" }, true, ["verify"]))));
  await t("exportKey jwk HMAC SHA-512", async () => stable(await S.exportKey("jwk", await imp("raw", K32, { name: "HMAC", hash: "SHA-512" }, true, ["sign", "verify"]))));
  await t("exportKey jwk one usage", async () => stable(await S.exportKey("jwk", await raw(K16, "AES-CBC", ["decrypt"]))));
  await t("exportKey jwk unextractable", async () => stable(await S.exportKey("jwk", await raw(K16, "AES-CBC", BOTH, false))));
  await t("exportKey jwk round trip", async () => {
    const j = await S.exportKey("jwk", await raw(K32, "AES-GCM", BOTH));
    return hex(await S.exportKey("raw", await imp("jwk", j, "AES-GCM", true, BOTH)));
  });
  await t("exportKey pkcs8 on a secret key", async () => hex(await S.exportKey("pkcs8", cbcKey)));
  await t("exportKey spki on a secret key", async () => hex(await S.exportKey("spki", cbcKey)));
  await t("exportKey bogus", async () => hex(await S.exportKey("bogus", cbcKey)));
  await t("exportKey raw PBKDF2", async () => hex(await S.exportKey("raw", await imp("raw", K16, "PBKDF2", false, ["deriveBits"]))));

  // ── 10. usages the algorithm does not have ────────────────────────────────────────────────────
  const USES = [["AES-CBC", ["sign"]], ["AES-CBC", []], ["AES-KW", ["encrypt"]],
                ["AES-GCM", ["wrapKey", "unwrapKey"]], ["AES-GCM", ["deriveBits"]],
                ["AES-CTR", ["encrypt", "encrypt"]]];
  for (const [alg, use] of USES) {
    await t("importKey usages " + alg + " " + JSON.stringify(use), async () => {
      const k = await raw(K16, alg, use);
      return "ok " + JSON.stringify(k.usages);
    });
  }
  await t("encrypt with a decrypt-only key", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, await raw(K16, "AES-CBC", ["decrypt"]), DATA)));
  await t("encrypt with an AES-GCM key under AES-CBC", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, gcmKey, DATA)));

  // ── 11. key lengths, and the bytes `wrapKey("jwk")` actually wraps ────────────────────────────
  await t("importKey AES-CBC raw 24 bytes", async () => JSON.stringify((await raw(K24, "AES-CBC", BOTH)).algorithm));
  await t("importKey AES-CBC raw 8 bytes", async () => JSON.stringify((await raw(seq(8, 0), "AES-CBC", BOTH)).algorithm));
  await t("importKey AES-CBC jwk A192CBC", async () => JSON.stringify((await imp("jwk", { kty: "oct", k: b64u(K24), alg: "A192CBC", ext: true }, "AES-CBC", true, BOTH)).algorithm));
  await t("importKey AES-CBC jwk 24 bytes no alg", async () => JSON.stringify((await imp("jwk", { kty: "oct", k: b64u(K24), ext: true }, "AES-CBC", true, BOTH)).algorithm));
  await t("generateKey AES-CBC 192", async () => JSON.stringify((await S.generateKey({ name: "AES-CBC", length: 192 }, true, BOTH)).algorithm));
  await t("generateKey AES-GCM 128 length", async () => JSON.stringify((await S.generateKey({ name: "AES-GCM", length: 128 }, true, BOTH)).algorithm));
  await t("wrapKey jwk plaintext", async () => {
    const stream = await raw(K16, "AES-CTR", ["wrapKey", "unwrapKey"]);
    const w = await S.wrapKey("jwk", rfc, stream, { name: "AES-CTR", counter: IV16, length: 64 });
    const p = await S.decrypt({ name: "AES-CTR", counter: IV16, length: 64 }, await raw(K16, "AES-CTR", BOTH), w);
    return new TextDecoder().decode(p);
  });
  await t("unwrapKey jwk of garbage", async () => {
    const stream = await raw(K16, "AES-CTR", ["wrapKey", "unwrapKey"]);
    await S.unwrapKey("jwk", seq(32, 0), stream, { name: "AES-CTR", counter: IV16, length: 64 }, "AES-CBC", true, BOTH);
    return "ok";
  });
  await t("unwrapKey format pkcs8", async () => { await S.unwrapKey("pkcs8", IV16, wrapCbc, { name: "AES-CBC", iv: IV16 }, "AES-CBC", true, BOTH); return "ok"; });
  await t("unwrapKey bad usages", async () => { await S.unwrapKey("raw", await S.wrapKey("raw", rfc, wrapCbc, { name: "AES-CBC", iv: IV16 }), wrapCbc, { name: "AES-CBC", iv: IV16 }, "AES-CBC", true, ["sign"]); return "ok"; });
  await t("AES-GCM tagLength 32 round trip", async () => {
    const ct = await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 32 }, gcmKey, DATA);
    return new TextDecoder().decode(await S.decrypt({ name: "AES-GCM", iv: IV12, tagLength: 32 }, gcmKey, ct));
  });
  await t("AES-GCM tagLength mismatch on decrypt", async () => {
    const ct = await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 32 }, gcmKey, DATA);
    return hex(await S.decrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, ct));
  });
  await t("AES-GCM long aad", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, additionalData: seq(37, 0x60) }, gcmKey, DATA)));
  await t("AES-GCM 33 byte body", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12 }, gcmKey, seq(33, 0x30))));
  await t("AES-CBC 32 byte body", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, seq(32, 0x30))));
  await t("AES-KW wrap 32 bytes", async () => hex(await S.wrapKey("raw", await raw(K32, "AES-CBC", BOTH), kek, "AES-KW")));
  await t("AES-KW unwrap corrupted", async () => {
    const w = new Uint8Array(await S.wrapKey("raw", rfc, kek, "AES-KW"));
    w[0] ^= 1;
    await S.unwrapKey("raw", w, kek, "AES-KW", "AES-CBC", true, BOTH);
    return "ok";
  });
  await t("AES-KW 256-bit kek", async () => hex(await S.wrapKey("raw", rfc, await imp("raw", K32, "AES-KW", true, ["wrapKey"]), "AES-KW")));
  await t("exportKey raw of a jwk-imported key", async () => hex(await S.exportKey("raw", await imp("jwk", JWK, "AES-CTR", true, BOTH))));
  await t("AES-KW wrap 8 bytes", async () => hex(await S.wrapKey("raw", await imp("raw", seq(8, 0), { name: "HMAC", hash: "SHA-256" }, true, ["sign"]), kek, "AES-KW")));

  // ── 12. Chrome's algorithm REGISTRY, read through `encrypt` ───────────────────────────────────
  // "Unrecognized name" means the name is in no registry at all; "Unsupported operation: encrypt"
  // means it is a known algorithm that simply does not encrypt. The pair separates the two, and the
  // second also shows the CASE Chrome echoes the name back in.
  const REGISTRY = ["SHA-1", "SHA-256", "SHA-384", "SHA-512", "sha-256", "hmac", "ecdsa", "ed25519",
                    "RSASSA-PKCS1-v1_5", "RSA-PSS", "Ed25519", "Ed448", "X448", "AES-CMAC", "AES-CFB",
                    "RSAES-PKCS1-v1_5", "CONCAT", "DH", "SHA-3", "X963-KDF", "ML-DSA-44", "ML-KEM-768"];
  for (const n of REGISTRY) {
    await t("registry " + JSON.stringify(n), async () => { await S.encrypt({ name: n }, cbcKey, DATA); return "ok"; });
  }

  // ── 13. the order the checks run in, and the param dictionaries ───────────────────────────────
  await t("order: wrong algorithm AND wrong usage", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, await raw(K16, "AES-GCM", ["decrypt"]), DATA)));
  await t("order: unsupported operation AND wrong usage", async () => hex(await S.encrypt({ name: "HMAC" }, await raw(K16, "AES-CBC", ["decrypt"]), DATA)));
  await t("order: bad params AND wrong usage", async () => hex(await S.encrypt({ name: "AES-CBC" }, await raw(K16, "AES-CBC", ["decrypt"]), DATA)));
  await t("AesCtrParams counter missing", async () => hex(await S.encrypt({ name: "AES-CTR", length: 64 }, ctrKey, DATA)));
  await t("AesCtrParams length missing", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16 }, ctrKey, DATA)));
  await t("AesCtrParams counter not a buffer", async () => hex(await S.encrypt({ name: "AES-CTR", counter: "no", length: 64 }, ctrKey, DATA)));
  await t("AesCbcParams iv not a buffer", async () => hex(await S.encrypt({ name: "AES-CBC", iv: 7 }, cbcKey, DATA)));
  await t("AeadParams tagLength 0", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 0 }, gcmKey, DATA)));
  await t("AeadParams tagLength 64", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 64 }, gcmKey, DATA)));
  await t("AeadParams tagLength 104", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 104 }, gcmKey, DATA)));
  await t("AeadParams tagLength 256", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, tagLength: 256 }, gcmKey, DATA)));
  await t("AeadParams additionalData not a buffer", async () => hex(await S.encrypt({ name: "AES-GCM", iv: IV12, additionalData: 7 }, gcmKey, DATA)));
  await t("data not a BufferSource", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, cbcKey, "text")));
  await t("key not a CryptoKey", async () => hex(await S.encrypt({ name: "AES-CBC", iv: IV16 }, {}, DATA)));
  await t("AES-CTR length 1 two blocks", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 1 }, ctrKey, seq(32, 0x30))));
  await t("AES-CTR length 1 three blocks", async () => hex(await S.encrypt({ name: "AES-CTR", counter: IV16, length: 1 }, ctrKey, seq(48, 0x30))));
  await t("wrapKey with an HMAC wrapping key", async () => hex(await S.wrapKey("raw", rfc, await imp("raw", K32, { name: "HMAC", hash: "SHA-256" }, true, ["sign"]), { name: "HMAC" })));
  await t("wrapKey algorithm unrecognized", async () => hex(await S.wrapKey("raw", rfc, wrapCbc, { name: "NOPE" })));
  await t("unwrapKey unwrappedKeyAlgorithm unrecognized", async () => { await S.unwrapKey("raw", IV16, wrapCbc, { name: "AES-CBC", iv: IV16 }, { name: "NOPE" }, true, BOTH); return "ok"; });
  // The unwrapped key's algorithm is normalized for `importKey`, not for `unwrapKey`: HMAC and
  // AES-KW are legal HERE and neither of them wraps anything.
  await t("unwrapKey into an HMAC key", async () => {
    const w = await S.wrapKey("raw", await imp("raw", K32, { name: "HMAC", hash: "SHA-256" }, true, ["sign"]), kek, "AES-KW");
    const k = await S.unwrapKey("raw", w, kek, "AES-KW", { name: "HMAC", hash: "SHA-256" }, true, ["sign", "verify"]);
    return JSON.stringify(k.algorithm) + " " + hex(await S.exportKey("raw", k));
  });
  await t("unwrapKey into an AES-KW key", async () => {
    const w = await S.wrapKey("raw", await imp("raw", K16, "AES-KW", true, ["wrapKey"]), kek, "AES-KW");
    const k = await S.unwrapKey("raw", w, kek, "AES-KW", "AES-KW", true, ["wrapKey", "unwrapKey"]);
    return JSON.stringify(k.algorithm) + " " + JSON.stringify(k.usages);
  });
  await t("unwrapKey into a PBKDF2 key", async () => {
    const w = await S.wrapKey("raw", rfc, kek, "AES-KW");
    const k = await S.unwrapKey("raw", w, kek, "AES-KW", "PBKDF2", false, ["deriveBits"]);
    return JSON.stringify(k.algorithm);
  });
  await t("wrapKey with an AES-KW key that only unwraps", async () => hex(await S.wrapKey("raw", rfc, await imp("raw", K16, "AES-KW", true, ["unwrapKey"]), "AES-KW")));

  // ── 12. RSA and elliptic-curve key import (PARITY RSA1) ───────────────────────────────────────
  // THE KEY MATERIAL IS FIXED and was generated once with OpenSSL; it is never regenerated, because
  // the signature, derived-bits and export lines below are KNOWN-ANSWER TESTS and mean nothing if
  // the keys move. They are throwaway keys that protect nothing and are published here on purpose.
  //
  // WHY THIS SECTION EXISTS. Five corpus rows call `importKey("spki", RSA-OAEP)` (ft.com,
  // harborfreight) or `importKey("jwk", RSA-OAEP)` (uline, wayfair). WC1 established that the
  // blocker is RSA key PARSING, not the format, so this reads Chrome's parser: which moduli and
  // curves it accepts, what each round-trip returns, and the exact error TYPE AND SENTENCE for a
  // malformed DER, an `alg` that disagrees with the algorithm, and a usage the key cannot satisfy.
  const b64d = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  const RSA2048_P8 = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDijdYXKIljrpY4Ot4ZsX8f9wwMqEyoB/3w2TZ+Bzixds1lDP27DxyWTjxYdW54TubEM44xHDUPyvBf877u7KuvwtItE4yI+t/ry4HRhrwJGWagWZwd9yq4E6FicIqBDTjdRtVa64fry2638ZR9Oyq2QT98JMrwUEb8shw+XJvtZRiNy26GtV4jxuKLu5sjOvXa/wMkrONJKlSbfR3nUtWh44u82wj3r5koxImsxwt+Ozr97PakgNcglU/35tQDuOdfgdhLjz6lMgxKrfVj3iNF94TFHmtPWPuIeD6J8YBwNu0NSsog7d+TC0auTGhvpw6P+RCbb6ke9uVReUPp9MXjAgMBAAECggEAT5tgSaXsVPMRhPOmfGfC0EQaX046FWctg5aHz9QQx0cEGEhu1e6S8HvQN5MX/L1Y2EN3aOlM+d4pBcZb5o+SKoHuIXm3jaB901igprPN2fKrfZfPRDZINANt123NNVikjRhpOvf9PE0w/ACu2SxTr1QRDznMd5H1x7B8PSuEfO86YaAzYGGR4DmxiyMxj5Q/BJlpMBzfp7vyqDqcA0aD7xFFkUjb1R1jOyvnIOKcx1yrpwgpfww38IisSXVDvS0lczwLZhX4IYzZdrX/n41+LL3UZ5vDWCoXUAdBzJgYiXN377i8CUE7uF7wnhzui8TwwYFKzyzWLlu5lpYGVMvUXQKBgQD1OebRlPNE2CIMk1EOxH4moK9fPo1tZ1FGBaVf15brF4lz8HecG5J4WtOYl0TxNaixyjftdNsbmLsPRJ2lvYANflWnve4RVwYmh2mesbGD6rE/wZBA3lnnGB+gWwfwTU5PForf0S7B1tvcZR5RxZpkK9T/uFCASmZJ0UCRsEx+NwKBgQDsge0bLb6J4yWCF2xp6TQHbgViIH7r/AKBs7GN09qVr6EdpScnq4amMU5S6yU/uvDwPuA8IxJys9+OsBQAhM9lpSX1EQkZQZlHnGXhJG4XbjecUyeBA2Awjp8h/sGTL/cR6JRfv35oBs3T5L77NwliyhSCoK5uvxPRZYMvLbA/tQKBgQCTAvBOasZgWlYnRdXNTZz3GVOpfL2afdrqaI4Y5A00yPp9SAbczmbDwmehzeXl2S5Vz44xy+FjLO/PmmoMJ4vyCu/czO9A8/Pa+0klew+ZrwQt3aPWT7aD5SFop8odlKrCnRgKT8SBLobGHNTNXvJM5T4fQ+b20iN6U3krHg7thwKBgQDMGrxzDiE3gi3mfSx0omriVpKSwpSGN3A6WsdKLKChOXaDzPljhOWpGzgK2x/ERiKUUs7VVzceghjWXSKF6BfMEqAVRGRjxXbwlApgKD5SPi4D3Mdk0XL7tLmkbuTDj0tbyynxD6OZRvXufaRLzI1vEhJtQRoIPSJvPXnfB0DpWQKBgAT0DpG2LvtSgQFUk2rBhoCjWniemiDUwgIyYy6wM6SVRLWWIhV2d9eSq25XW+4goQP6H6Fbc9NrFktr0dSM1hM3dXBsL4WMAme63RINTIC3NSy17Icbg9yaTKYV8wHwXYL8asxamrQLaESorY7nPYc8QgzrudPyT5Nwx28Y5s7i";
  const RSA2048_SPKI = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA4o3WFyiJY66WODreGbF/H/cMDKhMqAf98Nk2fgc4sXbNZQz9uw8clk48WHVueE7mxDOOMRw1D8rwX/O+7uyrr8LSLROMiPrf68uB0Ya8CRlmoFmcHfcquBOhYnCKgQ043UbVWuuH68tut/GUfTsqtkE/fCTK8FBG/LIcPlyb7WUYjctuhrVeI8bii7ubIzr12v8DJKzjSSpUm30d51LVoeOLvNsI96+ZKMSJrMcLfjs6/ez2pIDXIJVP9+bUA7jnX4HYS48+pTIMSq31Y94jRfeExR5rT1j7iHg+ifGAcDbtDUrKIO3fkwtGrkxob6cOj/kQm2+pHvblUXlD6fTF4wIDAQAB";
  const RSA2048B_P8 = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCixXRyRMp/jnViMLgjsd9I2xXAIOkJXgheOOcMnYWE8QmthLGCk/g8uMN5FelwCy6IiVLBTSet8i9wCIAusyiNu5lOM62QpNz2X5t6qEN0T7pb6j81qa2KilK6fBedENHRvP3R/bDbKBuv7r0tR1osC34ZnjdPrccQQEkJF0qHIDE2VgQDqbl4ocjwao9orRG15rJ3igAuiy8t+i3dsqlufjHS/ajc1NlUTZ6Qr/VApsafORuOrY3X5yq1lz5Kefnj4X69GHYUlIkIDG1uHPePokAbVNw/jADOypVYvXi6G7wECp7zMROeooULoI+l6qikepRdooh26k0iYqvTq2iVAgMBAAECggEABv9pKa52bDRyLHX+vJ4dhNLSZQ+lJR9zSaW1nS+6eUQpj9moDf6Rif7BfJeqEQSSDBGnSqMoBQzn6C+dXySZpjo99YQ4PoMv8rGajBPmzn3M+72e2Q5B5TZcJyNFmp2J/7PZz6jR7SIUyqP/Yw5xvHoEAlLwes70Q2kYGrlzHUr99VklX8yJzLAiztD0muxl5n5nwW3+/s4ZNen5kTc/Pv12keeE4goPysSAPjElxkpPVCyJZ/M11Y3kzBOXggpglq8ZegczglDJwcORpBazrEfkBqsxja5jBfutngJdfX/pzJHVYZkVFwniqO35ZRxCLt1kVcY3KSXCHkt4bWpdQQKBgQDMi6bMgPhSDPMPtE/l4WNqn2aTooaJkJgmIFT2n3URP6CwyZM6awKbZ2b3PmO/IcEXakcwPO55jXZPcfj+sOxcZi4f/ZRMmrlzU+PszYzz1+GiJZb/Cuz5OU5Yg8pH1DZoaJx2TQpGSqj1ovYYii+C3vjNU36RRxBiI7LPHehwtQKBgQDLt59jCayrl00vW6eA4Rprz6WT68v0S5784HiDmB5jJPFCw4kpYptsPAOXfoLpnyOvnf3/FS1buorPi7noaC5KZLJmxxZDO8VczClIjUTZVTVMuZ4K63vZEzfbGBj9NehMP71kzhaQ/EvoSZRPqsb6a3UJ8YM9KX+UCU9BzYpkYQKBgFtYJjX/hmWF+OnTeTbMr4wHAVT+6Jv4YantGGRGk44PXyBz9HbLIQ/rXnUdfIf3d/aP9JibLVhFD0etOyfqNLOSwhhxMs/+7MCO/yNaLA8dIls59DOTWn511AH9YNEw+NcpmG3QO71mTAveKY6N72e/UVbIaaZRGQTqskiCL7JdAoGBAMGCWg+sh+2y2imrKXCd4wgYBZN+P41+K7bPzwpJfEv3MomVzfuW5EYqt8q27MnYVgCi1jvwtNZWWzcM5fpiw938yw4f/QC+jRo6ANkfABBmBSwtgXej+k/YFo2y1UHpbBDJ1zEjJoJwdfZftlb6xOnpGxLn7B80BTDsqvG5ucThAoGBAKcIbX3wLOOvFutAdo1ApXWhEJL6iplXatIUTrB/mshNn0yyIKncCPeTnLwgfVxTyuoU/sdaKqTrfOaSgULRVY8UB9orNLRC3Ndysd7JdDLNQ1qZqd86n8u//qzexZrT9VOzj0H1f5s7EiQrNBabRgtLKq9Q9QlSfw8fxR8BRcAK";
  const RSA2048B_SPKI = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAosV0ckTKf451YjC4I7HfSNsVwCDpCV4IXjjnDJ2FhPEJrYSxgpP4PLjDeRXpcAsuiIlSwU0nrfIvcAiALrMojbuZTjOtkKTc9l+beqhDdE+6W+o/NamtiopSunwXnRDR0bz90f2w2ygbr+69LUdaLAt+GZ43T63HEEBJCRdKhyAxNlYEA6m5eKHI8GqPaK0Rteayd4oALosvLfot3bKpbn4x0v2o3NTZVE2ekK/1QKbGnzkbjq2N1+cqtZc+Snn54+F+vRh2FJSJCAxtbhz3j6JAG1TcP4wAzsqVWL14uhu8BAqe8zETnqKFC6CPpeqopHqUXaKIdupNImKr06tolQIDAQAB";
  const RSA1024_P8 = "MIICdgIBADANBgkqhkiG9w0BAQEFAASCAmAwggJcAgEAAoGBANDZl/+FbJjexxo8wzOWcEnF2XI1pJHLlnrLbZBnNR5lUAze7GzzNknopAFYArkfHk09QQgxx8P0J+mqXKFiwnrngDcPXpEWigIByrg8n0FEZ1J6pkU90MsL0mU/eo+xzd+NaUPJ4kRX/lWw+E251cl6qfhTkScWlmLuS0NO/ZGfAgMBAAECgYEAj1pL8p/qgqOnlmLOucIxIPIk5zfa1hggGfk4Iz40P8DayXraXY4zXVfJ+wImHFs3uVyqHq3MqLwWCVVeb4gSZHy+ZDG7ZWZykQ/RLEb0c/YH1P2Yvh3gAGhNBmHbAHdRUMtMqAVaLWhEqbVy3UqvewyYIBePBkqKGu15Bkf3b8ECQQDqL53MDJ5l3nH4FItp83X72IId4PW3rtZ0q/mMDmi04tcyJt9+qGh9hwi5kwn4VBaURVWR7k7adQkKMWtrNxfFAkEA5E3Q8TDyFngqRVb0Rv05z0BmEh5ulNlCE3/vU6AKSuVuoUt6dEd0rLMFblUWPhZzDCmmuBSVh0so3EsVHkp2EwJAE9mTY39SUcooGOIM6x8iygURCl1OFpKwRNVq4o86GtL7cJnQ6dh2euVePPSUrMLb4xd5aFQtxOCUspN7ymuyDQJAaKaG5VhTUBtq/Pfb54KB8k+Jujp64/4mvLuDH+kXbrd2m7b2fcCXaeZx0d5s4K73s5aQFodQ94BrTXeZSZZGQwJAa423O+HJtpvvnWEqZc62pxfux6AuOSqk9prCvl4tGR+vmaky5pkffP1PHTkFVbRcoDMDc7UXieurQ59/uCzk5w==";
  const RSA1024_SPKI = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDQ2Zf/hWyY3scaPMMzlnBJxdlyNaSRy5Z6y22QZzUeZVAM3uxs8zZJ6KQBWAK5Hx5NPUEIMcfD9CfpqlyhYsJ654A3D16RFooCAcq4PJ9BRGdSeqZFPdDLC9JlP3qPsc3fjWlDyeJEV/5VsPhNudXJeqn4U5EnFpZi7ktDTv2RnwIDAQAB";
  const RSA512_P8 = "MIIBVAIBADANBgkqhkiG9w0BAQEFAASCAT4wggE6AgEAAkEAq6CZG33tJkAnuHkUCsHvEBVFUyrY9MLcLtk03anHcxyAPZ6ppfOya5/qqlPVEe5Pw5LlnFM9TLhFgfYUPKPMtwIDAQABAkAMstBBQXr3do25z3OVLC41AW3krmnpCjXiXX3U3Mf+E0G7oxtd85J9jtuJxVau+bM9VHlUsEtoW7+5tQNzXJT5AiEA2ZQQWOIpQ+Zc/v2f4u2fSfG5ZsLY5GeXwPLV6vKBCgMCIQDJ70DCo6RlfF+iu33pyKMqZ5bwj1/W4XnXbBV9DfLOPQIgXsVRgYWxN7QlmaPberrKASla+lmwo3xD63VanCXY8wUCIDoDbkt8z+IIFUGfM9FSPu4TzJ1X02TcajClcJb4xuTRAiEAxg45YOlkAK3eo51f7qGhvHN6EGYc92WvLESkJDPhMv8=";
  const RSA512_SPKI = "MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBAKugmRt97SZAJ7h5FArB7xAVRVMq2PTC3C7ZNN2px3McgD2eqaXzsmuf6qpT1RHuT8OS5ZxTPUy4RYH2FDyjzLcCAwEAAQ==";
  const RSA3072_P8 = "MIIG/gIBADANBgkqhkiG9w0BAQEFAASCBugwggbkAgEAAoIBgQDu11ZGRKfWE4Q2+z5HnAS2WCTSvOf0KdIIduUjUpe6rCRaCAgXRWMDr7+z0otgGZpTWv41xgCwbz2o/RB4QsE3d1gJk6njm4/Fb6UZHKFHq7XnEbiPTlJJdIGlR2RScQoKtZNcsbiViM5PQSbrIILHgJVrIffjkSTes2UzVS6ft/YV2z/s8EdtsQ1w3Q+j9K1YiiFF6HSd+kULFSwcuuUrua64h99hmWsSbfBjSh5DlxKW7CaOvaW+/6gpyMyqBP6/c2YZ1i4dwk45Rj9El9XEYX0NTXal/PR7MkIJu70pNhAaJ/fXk9H5/C0cyuOXt7bUMbHqKt5pdFaKzUhcpFCGILffQm5SY+5SG/HlXneJAiZAJfd3TyRqUKn8+A1y15by//idid8qqdyD+4riTOtg+7QdQdKMfqIyJipanhZA3EomQCqDE06iKqNRrRC76REkpPb5dTZEgV/KBeUW7EEXuxszlx86yU8L292S9m0N/2qgJtuM7IO2TFZeWwXlJAsCAwEAAQKCAYA3y5peA//pZNnINl1QQURgjKrdiD/vrC1iyNOyssK7WyeK2MF7HjyJSe9QxOjGZHTG8ETerwjOLs6KwcOtNHsKyBiK9DIF9/tYNkKtYSDUK2rtNhiVcciLXGpFAC6Ri9K6ubxZNTNXy4PWw/OhjhC/JmzBgEf2o5unYuVLXqTrrwAQDw4Z/e/R0kC/kwTqWmh8fJhVSn6jwSmL80BA3fBXjKMifKtUk5CN150GtPPo+kvfcf937RnhSrwwNraJKW0eIdADlGFZDKygOlAq3kkYssxAPmIEZRmbiiAHfh4rK9XGyTQzEhIGqPxucdJxmTDv9QZYEPSOpIkC45yUvc9oKcBnyqKW+he+Fc5WZNRGrXjQziypDfPUaZz24Jnql2QjUoj/E6IiPejChAm/kyuXDIuKDco6o4VsKaCtYlTVP6S/rgLj5/I3CNwDBEFLrr1ay7sg76fWlr1kQi9v0px02uj/hcSO1oAb/7jgx+JfUdXxt1FdT3p0NP8bv8/MJ1ECgcEA/Jdbcv70890a48MrZErvhQ3SX1pvs0VP/9T2YguefRa7+NXqutwmvzSB8m31Nasflsa5vsBlWl+n1cFvADFRDISXSPOp5K/ni0SCydYnDTs4Ccy8pM2wp11d0TE6r7eDkHa+9HQFx32eAs/NP3i/O752md7zinhX89F8INf6d8d+dNxdceUx7c3IZYw3zdHdnQNQvNEEhAgdup1bpjSoZhaKRfQBFMDSZjlpL9mWalVtj4o8qhhk/SiMG56SBwz1AoHBAPIQef2u+hFZLvixuQ+Uf+5mArcipPugs3ybNEkLSfVcWERAT3T3hm5UtaR0y0dEoYVaRtdA7WCpVrr9E5P9esbgGxOqQj6a7suyvajwpJC/4ffC+6Eifrl2TRDUWqa+lFwJeIE+wx3yX1Zzq6aPGEbyWnbSuwRWFQJAREwd5ALLY93BQy+y6LgOjKtyVz4EUpJqRTW9OsbzqCR7564mUaNfpOtiPRKNVj88xpRkyWjV8aKZbPJDyZWqtRRNkNzM/wKBwHNIRUWj1XpjhH1FO02n4c+RMl2wseBZFx0FPnobIXen45y7TUmBZvHHbjOOOecHtgAjrOzBdMCdfcVSZm3pFDupXD7Abt7f5carOyIRantgdsAKQGLbOe/47fOnC0jdcd+CQr7hIS4eR/qyitOYMXd8AzdRCUg+iq3OA2AeEsOOOknttXxhBbuqOvcV2vZF/WZG+BdlMjFsXt0oKocd62nEidFJfyDQrHZZZ4eeQEdBvCJSDkP3pEfitKd6ZN1w7QKBwQDQn3i0NjEZ8JY2Ct4nWmIj8lFWywoMQEVRGGaoqY0VZYBBWnfgpFB9GI7wbSShiLn4t4SkkjXEAE5anDFac7V0X8OWYD/1uwwiDSu+xZ/fjTDQm4DJMHStnRLwdUSnv0sYQ6VTJud9EN4wIUMIKv3Ob6OeEyLRqusyZHc4bu6LpgWtIpqoKna94vRD8yjTr2AHBoYdubhFrcv1ncBgbHAh8RWvNJzUN3Hk0JmJXY/kxbca30ZxShEb5+Nm2y8FzbsCgcEA42G/lnk2OFLslj57qXnZSo1S59kO9bbKQXNVGyOsRIUGAVrXJeuKuhjLdavRw2ATXtOBXl3idpHuNyfrtIUgb0Bpz+JNXswU5CcXj+WOJk2z1JdHXCNjuylz+H8nE8gJMIXufPTWw7lSFVvMcsbWxwXuRk5DKhTiyFMH145oZb4vyYLGrlSLDl89QvVPTh9wxTvUPY8gkZ7UqxIWWgiVwYCQJEklSQOEXz/I/X3gkJ1aLOlme25k1vfLeH4fTD5B";
  const RSA3072_SPKI = "MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEA7tdWRkSn1hOENvs+R5wEtlgk0rzn9CnSCHblI1KXuqwkWggIF0VjA6+/s9KLYBmaU1r+NcYAsG89qP0QeELBN3dYCZOp45uPxW+lGRyhR6u15xG4j05SSXSBpUdkUnEKCrWTXLG4lYjOT0Em6yCCx4CVayH345Ek3rNlM1Uun7f2Fds/7PBHbbENcN0Po/StWIohReh0nfpFCxUsHLrlK7muuIffYZlrEm3wY0oeQ5cSluwmjr2lvv+oKcjMqgT+v3NmGdYuHcJOOUY/RJfVxGF9DU12pfz0ezJCCbu9KTYQGif315PR+fwtHMrjl7e21DGx6ireaXRWis1IXKRQhiC330JuUmPuUhvx5V53iQImQCX3d08kalCp/PgNcteW8v/4nYnfKqncg/uK4kzrYPu0HUHSjH6iMiYqWp4WQNxKJkAqgxNOoiqjUa0Qu+kRJKT2+XU2RIFfygXlFuxBF7sbM5cfOslPC9vdkvZtDf9qoCbbjOyDtkxWXlsF5SQLAgMBAAE=";
  const RSAE3_P8 = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7/Y8Ri8ED/hO0IxpDUI29FF5N+b9a7qvJyg+QUU+r3gxRA2xmOdt6WtFotgzuez//o3JMLNTYz6h+x9NOvuJnHUIfxTM/gQdbNt6h6w0THQjGEaBmiQpGbuTNKInPjy+krv7CREYqTH4QvfJ98+CIEGlHhGhaJABr9I4vcwxKhU2aGn0VZ66XCBJuim+V80nSjNQwOr1LuZ35YsJJkDP0D35iXNByaH0Ixo3F3ggDmDy4I1UK6PYH6+Mmf8GY1L1l996Gtna/lG+lHOQda2AVLdgqsqFbcR4ly1bgNqFvl8aVKbnFRo9EZUHlfXLJC08+UFpbl5iA384yMTSzuX7BAgEDAoIBAH1TtLZdK1f+t81svCzgXn4NlDP71OdJx9vcCmA2NR0+suCs8u7RPPw8i5skCJ781VUXoYgd4zs1Gv8v4jR/QZoTgWqDd3+rWjzPPxacs2ITWy62au8GBtmfQzNwW9+0ym3J/ywthBwy/rXT9v6ilbAK8NpYRZFtVZ1NtB+iCDGtDTYTdUpEhhbD48/oxHR8qA9AufOJ7dCuWA7BCOHfNTABHKU6igDjta1qExPE15FPR8XuttDpUmt3KAz772DQx9MvU8vHI6S2c6OLgAuvTYyQdyMzobta2hpRI73c/VcVlCNAKP2FrGVE0bDGH2b+xnvMRAy6croDCWKK4emf8eMCgYEA9rzPajApJHy1dMsfNWtobBAzDasEgdXglf0rryRRH4hGIeONZh2prZZlUz/xlCkVOh4tS5dFEcqGEySaphAMV5i86oPtal/jM3oH3N9LftDLkGIdqI8jJvhdCXVGe0kamvFiA6OqBVItp26zSMhCgYV/UfKfu9tRMcLhl8qi/YsCgYEAwwwt4vXXwPgsx+uOE3vP4at4r5fnVrzTg+YVhdJwRKPHsYb3m1NpPu5CHehFMICMFvEP9zpF6JwzE+5rNHePOhBz9lEeVuifjrXDxyyY7PGJlRPHhjNFsAX0l84kqiPczW7neKVUB1pQX+2g+uZKo/8eolXgMI6KDlt/SgqmlmMCgYEApH3fnCAbbah4+IdqI5zwSArMs8dYVo6VuVNydMLgv7AuwUJeRBPGc7mY4iqhDXC40Wlzh7ouC9xZYhhnGWAIOmXTRwKeRuqXd6av6JTc/zXdCuwTxbTCGfrosPjZp4YRvKDsAm0cA4web58iMIWBq65U4UxqfTzgy9dBD9xsqQcCgYEAgggel06P1frIhUe0DP01QRz7H7qaOdM3rUQOWTb1gxfadln6Z4zw1J7WvprYywBdZKC1T3wumxLMt/RHeE+00WBNTuC+5JsVCc6ChMhl80uxDg0vrszZIAP4ZTQYcW093knvpcONWjw1lUkV/JmHF/9pwY6VdbRcCZJU3AcZuZcCgYEAzKDNtAJF51tdOwHve6117Hopmx9uVKD49LWo23Yj3NQemXnY2e9f7Kx9rdqPu5BMD/KlNQEV79VktxHGB+XvykyLDp8mT5Xk4VAn70Jhncs3xmkzQiJd21FPprZz5xG+T8UTDGhEtPlenmuouvsBQl3dn5eRhXrwuKt3d2g7Maw=";
  const RSAE3_SPKI = "MIIBIDANBgkqhkiG9w0BAQEFAAOCAQ0AMIIBCAKCAQEAu/2PEYvBA/4TtCMaQ1CNvRReTfm/Wu6rycoPkFFPq94MUQNsZjnbelrRaLYM7ns//6NyTCzU2M+ofsfTTr7iZx1CH8UzP4EHWzbeoesNEx0IxhGgZokKRm7kzSiJz48vpK7+wkRGKkx+EL3yffPgiBBpR4RoWiQAa/SOL3MMSoVNmhp9FWeulwgSbopvlfNJ0ozUMDq9S7md+WLCSZAz9A9+YlzQcmh9CMaNxd4IA5g8uCNVCuj2B+vjJn/BmNS9ZffehrZ2v5RvpRzkHWtgFS3YKrKhW3EeJctW4Dahb5fGlSm5xUaPRGVB5X1yyQtPPlBaW5eYgN/OMjE0s7l+wQIBAw==";
  const P256_P8 = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgd7Qac0xNCmbLrXSUiIKsAvXDNeybEU3hHrGf/K263vShRANCAAS2D5AA0S8MDDaoGTg0yj0jbRWNIL9eO8iJ7ihKKN2IrFGHgrJ8oR1lQYamndX1CRPfnBsMInTWximTt53d3Wzw";
  const P256_SPKI = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEtg+QANEvDAw2qBk4NMo9I20VjSC/XjvIie4oSijdiKxRh4KyfKEdZUGGpp3V9QkT35wbDCJ01sYpk7ed3d1s8A==";
  const P256B_P8 = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg5r7M0nM2tG2kTI+7mRUUoDKtso9t7Wm0tuM0NqpLqDChRANCAARhMSpr/Qp2rQ0/n5TGtRrGs2+B+lx9Q0f+pAEbQxBiA6u4EY84BS24NiCgY0YGqAeyLECADYOxKvu5BtZhnKme";
  const P256B_SPKI = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEYTEqa/0Kdq0NP5+UxrUaxrNvgfpcfUNH/qQBG0MQYgOruBGPOAUtuDYgoGNGBqgHsixAgA2DsSr7uQbWYZypng==";
  const P384_P8 = "MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDA54BSyDw1VDaJwsrxGHqFStzGSvCyTo2dYouC/LXJHZbgk/wxRFC/zzwLj6U0XAdKhZANiAARjUUfYRDaO4PhwJrTIEeq+GhbaarniSrDcYLqy7HVD/Jx4NKY1LgZ82g9QuePlYGB1rfDw50oOhymO1preJAo0C26RAMsILrlZW8yvU7ZHqXV3vB/fZ0OOGztBDL1Q/i4=";
  const P384_SPKI = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEY1FH2EQ2juD4cCa0yBHqvhoW2mq54kqw3GC6sux1Q/yceDSmNS4GfNoPULnj5WBgda3w8OdKDocpjtaa3iQKNAtukQDLCC65WVvMr1O2R6l1d7wf32dDjhs7QQy9UP4u";
  const P521_P8 = "MIHuAgEAMBAGByqGSM49AgEGBSuBBAAjBIHWMIHTAgEBBEIAwxujlVJCWnyh74DVK/ZKRZAz4vKh/ADQgyQMGK0kQJBlM5bjgaiR1tAMYjkQGVa0ozdY6ogHdjse7b44rctdLoyhgYkDgYYABAFXMXYTybg85C7SDmjUaOosoKWTNZYxkIoxAhpk5JmPWrZLdZ18bTIiwThYjKTt1b+vmP3jaV+A9By+bJB7yiFQYQFYn2+q0zbO1WsmdD60A/6L+4Tx1qRlVJ69aZA08JCalYCYJlJnJmVbE9dBc5YnjHCIWeZAbwsyZ7/xcjFfPCzwBA==";
  const P521_SPKI = "MIGbMBAGByqGSM49AgEGBSuBBAAjA4GGAAQBVzF2E8m4POQu0g5o1GjqLKClkzWWMZCKMQIaZOSZj1q2S3WdfG0yIsE4WIyk7dW/r5j942lfgPQcvmyQe8ohUGEBWJ9vqtM2ztVrJnQ+tAP+i/uE8dakZVSevWmQNPCQmpWAmCZSZyZlWxPXQXOWJ4xwiFnmQG8LMme/8XIxXzws8AQ=";
  const K256_P8 = "MIGEAgEAMBAGByqGSM49AgEGBSuBBAAKBG0wawIBAQQgUXWtheb9/hYr7EcJ6vUNnCtHMY4vF9ErP44IUK25SqWhRANCAATFtUpYq6F8APeY3NCEK5h50nfqTF0TipZWFxGsVTQI0CcJnMFq42eq2ZKDglKc7e7pj4k9jpLXkHrQxscpLiFR";
  const K256_SPKI = "MFYwEAYHKoZIzj0CAQYFK4EEAAoDQgAExbVKWKuhfAD3mNzQhCuYedJ36kxdE4qWVhcRrFU0CNAnCZzBauNnqtmSg4JSnO3u6Y+JPY6S15B60MbHKS4hUQ==";
  const P256_RAW_B64 = "BLYPkADRLwwMNqgZODTKPSNtFY0gv147yInuKEoo3YisUYeCsnyhHWVBhqad1fUJE9+cGwwidNbGKZO3nd3dbPA=";
  const OAEP = { name: "RSA-OAEP", hash: "SHA-256" };
  const PKCS1 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };
  const PSS = { name: "RSA-PSS", hash: "SHA-256" };
  const ECDSA256 = { name: "ECDSA", namedCurve: "P-256" };
  const ECDH256 = { name: "ECDH", namedCurve: "P-256" };
  const desc = (k) => "ok " + k.type + " " + JSON.stringify(k.algorithm) + " " + JSON.stringify(k.usages) + " ext=" + k.extractable;

  // 12a. spki / pkcs8, the two formats that carry no symmetric key.
  await t("importKey spki RSA-OAEP", async () => desc(await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["encrypt"])));
  await t("importKey pkcs8 RSA-OAEP", async () => desc(await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["decrypt"])));
  await t("importKey spki RSASSA-PKCS1-v1_5", async () => desc(await imp("spki", b64d(RSA2048_SPKI), PKCS1, true, ["verify"])));
  await t("importKey pkcs8 RSASSA-PKCS1-v1_5", async () => desc(await imp("pkcs8", b64d(RSA2048_P8), PKCS1, true, ["sign"])));
  await t("importKey spki RSA-PSS", async () => desc(await imp("spki", b64d(RSA2048_SPKI), PSS, true, ["verify"])));
  await t("importKey spki ECDSA P-256", async () => desc(await imp("spki", b64d(P256_SPKI), ECDSA256, true, ["verify"])));
  await t("importKey pkcs8 ECDSA P-256", async () => desc(await imp("pkcs8", b64d(P256_P8), ECDSA256, true, ["sign"])));
  await t("importKey spki ECDH P-256", async () => desc(await imp("spki", b64d(P256_SPKI), ECDH256, true, [])));
  await t("importKey pkcs8 ECDH P-256", async () => desc(await imp("pkcs8", b64d(P256_P8), ECDH256, true, ["deriveBits"])));
  await t("importKey raw ECDSA P-256", async () => desc(await imp("raw", b64d(P256_RAW_B64), ECDSA256, true, ["verify"])));
  await t("importKey raw RSA-OAEP", async () => desc(await imp("raw", b64d(RSA2048_SPKI), OAEP, true, ["encrypt"])));
  await t("importKey spki AES-CBC", async () => desc(await imp("spki", b64d(RSA2048_SPKI), "AES-CBC", true, BOTH)));

  // 12b. WHICH MODULI. The spec sets no floor; Chrome does, and the floor is the finding.
  await t("importKey spki RSA 512", async () => (await imp("spki", b64d(RSA512_SPKI), OAEP, true, ["encrypt"])).algorithm.modulusLength);
  await t("importKey spki RSA 1024", async () => (await imp("spki", b64d(RSA1024_SPKI), OAEP, true, ["encrypt"])).algorithm.modulusLength);
  await t("importKey spki RSA 3072", async () => (await imp("spki", b64d(RSA3072_SPKI), OAEP, true, ["encrypt"])).algorithm.modulusLength);
  await t("importKey spki RSA e=3", async () => JSON.stringify((await imp("spki", b64d(RSAE3_SPKI), OAEP, true, ["encrypt"])).algorithm.publicExponent));
  await t("importKey pkcs8 RSA 512", async () => (await imp("pkcs8", b64d(RSA512_P8), OAEP, true, ["decrypt"])).algorithm.modulusLength);

  // 12c. WHICH CURVES.
  await t("importKey spki ECDSA P-384", async () => JSON.stringify((await imp("spki", b64d(P384_SPKI), { name: "ECDSA", namedCurve: "P-384" }, true, ["verify"])).algorithm));
  await t("importKey spki ECDSA P-521", async () => JSON.stringify((await imp("spki", b64d(P521_SPKI), { name: "ECDSA", namedCurve: "P-521" }, true, ["verify"])).algorithm));
  await t("importKey spki ECDSA secp256k1", async () => desc(await imp("spki", b64d(K256_SPKI), { name: "ECDSA", namedCurve: "secp256k1" }, true, ["verify"])));
  await t("importKey spki ECDSA P-256 curve K-256", async () => desc(await imp("spki", b64d(K256_SPKI), ECDSA256, true, ["verify"])));
  await t("importKey spki ECDSA curve mismatch", async () => desc(await imp("spki", b64d(P384_SPKI), ECDSA256, true, ["verify"])));
  await t("importKey spki ECDSA namedCurve bogus", async () => desc(await imp("spki", b64d(P256_SPKI), { name: "ECDSA", namedCurve: "P-257" }, true, ["verify"])));
  await t("importKey spki ECDSA namedCurve missing", async () => desc(await imp("spki", b64d(P256_SPKI), { name: "ECDSA" }, true, ["verify"])));

  // 12d. MALFORMED DER -- the error type and the exact sentence.
  await t("importKey spki truncated", async () => desc(await imp("spki", b64d(RSA2048_SPKI).slice(0, 200), OAEP, true, ["encrypt"])));
  await t("importKey spki empty", async () => desc(await imp("spki", new Uint8Array(0), OAEP, true, ["encrypt"])));
  await t("importKey spki garbage", async () => desc(await imp("spki", new Uint8Array([0x30, 0x03, 0x02, 0x01, 0x00]), OAEP, true, ["encrypt"])));
  await t("importKey spki trailing byte", async () => { const d = b64d(RSA2048_SPKI); const x = new Uint8Array(d.length + 1); x.set(d); return desc(await imp("spki", x, OAEP, true, ["encrypt"])); });
  await t("importKey pkcs8 truncated", async () => desc(await imp("pkcs8", b64d(RSA2048_P8).slice(0, 400), OAEP, true, ["decrypt"])));
  await t("importKey pkcs8 is really a spki", async () => desc(await imp("pkcs8", b64d(RSA2048_SPKI), OAEP, true, ["decrypt"])));
  await t("importKey spki is really a pkcs8", async () => desc(await imp("spki", b64d(RSA2048_P8), OAEP, true, ["encrypt"])));
  await t("importKey spki of an EC key as RSA", async () => desc(await imp("spki", b64d(P256_SPKI), OAEP, true, ["encrypt"])));
  await t("importKey spki of an RSA key as ECDSA", async () => desc(await imp("spki", b64d(RSA2048_SPKI), ECDSA256, true, ["verify"])));
  await t("importKey raw ECDSA not a point", async () => desc(await imp("raw", new Uint8Array(65), ECDSA256, true, ["verify"])));
  await t("importKey raw ECDSA 64 bytes", async () => desc(await imp("raw", b64d(P256_RAW_B64).slice(1), ECDSA256, true, ["verify"])));

  // 12e. USAGES the key cannot satisfy.
  await t("importKey spki RSA-OAEP [decrypt]", async () => desc(await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["decrypt"])));
  await t("importKey pkcs8 RSA-OAEP [encrypt]", async () => desc(await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["encrypt"])));
  await t("importKey spki RSA-OAEP []", async () => desc(await imp("spki", b64d(RSA2048_SPKI), OAEP, true, [])));
  await t("importKey pkcs8 RSA-OAEP []", async () => desc(await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, [])));
  await t("importKey spki RSA-OAEP [wrapKey]", async () => desc(await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["wrapKey"])));
  await t("importKey spki RSA-OAEP [sign]", async () => desc(await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["sign"])));
  await t("importKey spki ECDSA [sign]", async () => desc(await imp("spki", b64d(P256_SPKI), ECDSA256, true, ["sign"])));
  await t("importKey pkcs8 ECDH [deriveKey,deriveBits]", async () => desc(await imp("pkcs8", b64d(P256_P8), ECDH256, true, ["deriveKey", "deriveBits"])));
  await t("importKey spki ECDH [deriveBits]", async () => desc(await imp("spki", b64d(P256_SPKI), ECDH256, true, ["deriveBits"])));

  // 12f. jwk, the other half of the cluster (uline, wayfair).
  const oaepPub = await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["encrypt"]);
  const oaepPriv = await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["decrypt"]);
  const pkcs1Pub = await imp("spki", b64d(RSA2048_SPKI), PKCS1, true, ["verify"]);
  const pkcs1Priv = await imp("pkcs8", b64d(RSA2048_P8), PKCS1, true, ["sign"]);
  const ecdsaPub = await imp("spki", b64d(P256_SPKI), ECDSA256, true, ["verify"]);
  const ecdsaPriv = await imp("pkcs8", b64d(P256_P8), ECDSA256, true, ["sign"]);
  const ecdhPriv = await imp("pkcs8", b64d(P256_P8), ECDH256, true, ["deriveBits"]);
  const ecdhPeer = await imp("spki", b64d(P256B_SPKI), ECDH256, true, []);

  await t("exportKey jwk RSA-OAEP public", async () => JSON.stringify(await S.exportKey("jwk", oaepPub)));
  await t("exportKey jwk RSA-OAEP private keys", async () => { const j = await S.exportKey("jwk", oaepPriv); return Object.keys(j).sort().join(",") + " alg=" + j.alg + " kty=" + j.kty + " n=" + j.n.length + " d=" + j.d.length; });
  await t("exportKey jwk RSASSA public alg", async () => (await S.exportKey("jwk", pkcs1Pub)).alg);
  await t("exportKey jwk ECDSA public", async () => JSON.stringify(await S.exportKey("jwk", ecdsaPub)));
  await t("exportKey jwk ECDSA private keys", async () => { const j = await S.exportKey("jwk", ecdsaPriv); return Object.keys(j).sort().join(",") + " crv=" + j.crv + " d=" + j.d.length; });
  await t("exportKey jwk ECDH public", async () => JSON.stringify(await S.exportKey("jwk", ecdhPeer)));
  await t("importKey jwk RSA-OAEP public", async () => desc(await imp("jwk", await S.exportKey("jwk", oaepPub), OAEP, true, ["encrypt"])));
  await t("importKey jwk RSA-OAEP private", async () => desc(await imp("jwk", await S.exportKey("jwk", oaepPriv), OAEP, true, ["decrypt"])));
  await t("importKey jwk ECDSA public", async () => desc(await imp("jwk", await S.exportKey("jwk", ecdsaPub), ECDSA256, true, ["verify"])));
  await t("importKey jwk RSA alg mismatch", async () => { const j = await S.exportKey("jwk", oaepPub); j.alg = "RS256"; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA alg absent", async () => { const j = await S.exportKey("jwk", oaepPub); delete j.alg; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA kty oct", async () => { const j = await S.exportKey("jwk", oaepPub); j.kty = "oct"; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA n missing", async () => { const j = await S.exportKey("jwk", oaepPub); delete j.n; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA e missing", async () => { const j = await S.exportKey("jwk", oaepPub); delete j.e; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA n not base64url", async () => { const j = await S.exportKey("jwk", oaepPub); j.n = "!!!"; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA private d only", async () => { const j = await S.exportKey("jwk", oaepPriv); delete j.p; delete j.q; delete j.dp; delete j.dq; delete j.qi; return desc(await imp("jwk", j, OAEP, true, ["decrypt"])); });
  await t("importKey jwk RSA private p missing", async () => { const j = await S.exportKey("jwk", oaepPriv); delete j.p; return desc(await imp("jwk", j, OAEP, true, ["decrypt"])); });
  await t("importKey jwk EC crv mismatch", async () => { const j = await S.exportKey("jwk", ecdsaPub); j.crv = "P-384"; return desc(await imp("jwk", j, ECDSA256, true, ["verify"])); });
  await t("importKey jwk EC kty mismatch", async () => { const j = await S.exportKey("jwk", ecdsaPub); j.kty = "RSA"; return desc(await imp("jwk", j, ECDSA256, true, ["verify"])); });
  await t("importKey jwk EC y missing", async () => { const j = await S.exportKey("jwk", ecdsaPub); delete j.y; return desc(await imp("jwk", j, ECDSA256, true, ["verify"])); });
  await t("importKey jwk EC x short", async () => { const j = await S.exportKey("jwk", ecdsaPub); j.x = j.x.slice(0, 40); return desc(await imp("jwk", j, ECDSA256, true, ["verify"])); });
  await t("importKey jwk EC use sig on ECDH", async () => { const j = await S.exportKey("jwk", ecdsaPub); j.use = "sig"; return desc(await imp("jwk", j, ECDH256, true, [])); });
  await t("importKey jwk RSA key_ops narrower", async () => { const j = await S.exportKey("jwk", oaepPub); j.key_ops = ["wrapKey"]; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });
  await t("importKey jwk RSA ext false", async () => { const j = await S.exportKey("jwk", oaepPub); j.ext = false; return desc(await imp("jwk", j, OAEP, true, ["encrypt"])); });

  // 12g. ROUND TRIPS, byte for byte.
  await t("exportKey spki round trip", async () => hex(await S.exportKey("spki", oaepPub)) === hex(b64d(RSA2048_SPKI)));
  await t("exportKey pkcs8 round trip", async () => hex(await S.exportKey("pkcs8", oaepPriv)) === hex(b64d(RSA2048_P8)));
  await t("exportKey spki EC round trip", async () => hex(await S.exportKey("spki", ecdsaPub)) === hex(b64d(P256_SPKI)));
  await t("exportKey pkcs8 EC round trip", async () => hex(await S.exportKey("pkcs8", ecdsaPriv)) === hex(b64d(P256_P8)));
  await t("exportKey raw ECDSA public", async () => hex(await S.exportKey("raw", ecdsaPub)));
  await t("exportKey raw ECDSA private", async () => hex(await S.exportKey("raw", ecdsaPriv)));
  await t("exportKey spki on a private key", async () => hex(await S.exportKey("spki", oaepPriv)));
  await t("exportKey pkcs8 on a public key", async () => hex(await S.exportKey("pkcs8", oaepPub)));

  // 12h. THE CIPHERS BEHIND THE GUARDS. A rejection is visible; a wrong ciphertext is not, so the
  // deterministic ones are stated as bytes and the randomized ones as round trips.
  await t("RSASSA-PKCS1-v1_5 sign SHA-256", async () => hex(await S.sign(PKCS1, pkcs1Priv, DATA)));
  await t("RSASSA-PKCS1-v1_5 verify", async () => await S.verify(PKCS1, pkcs1Pub, await S.sign(PKCS1, pkcs1Priv, DATA), DATA));
  await t("RSASSA-PKCS1-v1_5 verify tampered", async () => await S.verify(PKCS1, pkcs1Pub, await S.sign(PKCS1, pkcs1Priv, DATA), seq(8, 0)));
  await t("RSASSA-PKCS1-v1_5 sign with the public key", async () => hex(await S.sign(PKCS1, pkcs1Pub, DATA)));
  await t("RSASSA-PKCS1-v1_5 sign SHA-1", async () => hex(await S.sign({ name: "RSASSA-PKCS1-v1_5" }, await imp("pkcs8", b64d(RSA2048_P8), { name: "RSASSA-PKCS1-v1_5", hash: "SHA-1" }, true, ["sign"]), DATA)));
  await t("RSASSA-PKCS1-v1_5 sign SHA-512", async () => hex(await S.sign({ name: "RSASSA-PKCS1-v1_5" }, await imp("pkcs8", b64d(RSA2048_P8), { name: "RSASSA-PKCS1-v1_5", hash: "SHA-512" }, true, ["sign"]), DATA)));
  await t("RSASSA-PKCS1-v1_5 sign 512-bit key SHA-512", async () => hex(await S.sign({ name: "RSASSA-PKCS1-v1_5" }, await imp("pkcs8", b64d(RSA512_P8), { name: "RSASSA-PKCS1-v1_5", hash: "SHA-512" }, true, ["sign"]), DATA)));
  await t("RSA-OAEP round trip", async () => new TextDecoder().decode(await S.decrypt(OAEP, oaepPriv, await S.encrypt(OAEP, oaepPub, DATA))));
  await t("RSA-OAEP round trip with a label", async () => new TextDecoder().decode(await S.decrypt({ name: "RSA-OAEP", label: AAD }, oaepPriv, await S.encrypt({ name: "RSA-OAEP", label: AAD }, oaepPub, DATA))));
  await t("RSA-OAEP label mismatch", async () => hex(await S.decrypt({ name: "RSA-OAEP" }, oaepPriv, await S.encrypt({ name: "RSA-OAEP", label: AAD }, oaepPub, DATA))));
  await t("RSA-OAEP too long", async () => hex(await S.encrypt(OAEP, oaepPub, seq(256, 0))));
  await t("RSA-OAEP largest that fits", async () => (await S.encrypt(OAEP, oaepPub, seq(190, 0))).byteLength);
  await t("RSA-OAEP one too many", async () => hex(await S.encrypt(OAEP, oaepPub, seq(191, 0))));
  await t("RSA-OAEP decrypt garbage", async () => hex(await S.decrypt(OAEP, oaepPriv, seq(256, 0))));
  await t("RSA-OAEP encrypt with the private key", async () => hex(await S.encrypt(OAEP, oaepPriv, DATA)));
  await t("RSA-PSS round trip", async () => { const p = { name: "RSA-PSS", saltLength: 32 }; const priv = await imp("pkcs8", b64d(RSA2048_P8), PSS, true, ["sign"]); const pub = await imp("spki", b64d(RSA2048_SPKI), PSS, true, ["verify"]); return await S.verify(p, pub, await S.sign(p, priv, DATA), DATA); });
  await t("RSA-PSS saltLength 0", async () => { const p = { name: "RSA-PSS", saltLength: 0 }; const priv = await imp("pkcs8", b64d(RSA2048_P8), PSS, true, ["sign"]); return hex(await S.sign(p, priv, DATA)); });
  await t("RSA-PSS saltLength too large", async () => { const p = { name: "RSA-PSS", saltLength: 256 }; const priv = await imp("pkcs8", b64d(RSA2048_P8), PSS, true, ["sign"]); return hex(await S.sign(p, priv, DATA)); });
  await t("RSA-PSS saltLength missing", async () => { const priv = await imp("pkcs8", b64d(RSA2048_P8), PSS, true, ["sign"]); return hex(await S.sign({ name: "RSA-PSS" }, priv, DATA)); });
  await t("ECDSA round trip", async () => { const p = { name: "ECDSA", hash: "SHA-256" }; return await S.verify(p, ecdsaPub, await S.sign(p, ecdsaPriv, DATA), DATA); });
  await t("ECDSA signature length", async () => (await S.sign({ name: "ECDSA", hash: "SHA-256" }, ecdsaPriv, DATA)).byteLength);
  await t("ECDSA verify a DER signature", async () => await S.verify({ name: "ECDSA", hash: "SHA-256" }, ecdsaPub, new Uint8Array(70), DATA));
  await t("ECDSA hash missing", async () => hex(await S.sign({ name: "ECDSA" }, ecdsaPriv, DATA)));
  await t("ECDSA P-521 round trip", async () => { const p = { name: "ECDSA", hash: "SHA-512" }; const priv = await imp("pkcs8", b64d(P521_P8), { name: "ECDSA", namedCurve: "P-521" }, true, ["sign"]); const pub = await imp("spki", b64d(P521_SPKI), { name: "ECDSA", namedCurve: "P-521" }, true, ["verify"]); return (await S.sign(p, priv, DATA)).byteLength + " " + (await S.verify(p, pub, await S.sign(p, priv, DATA), DATA)); });
  await t("ECDH deriveBits 256", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPeer }, ecdhPriv, 256)));
  await t("ECDH deriveBits null length", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPeer }, ecdhPriv, null)));
  await t("ECDH deriveBits 128", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPeer }, ecdhPriv, 128)));
  await t("ECDH deriveBits 257", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPeer }, ecdhPriv, 257)));
  await t("ECDH deriveBits 4", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPeer }, ecdhPriv, 4)));
  await t("ECDH public is a private key", async () => hex(await S.deriveBits({ name: "ECDH", public: ecdhPriv }, ecdhPriv, 256)));
  await t("ECDH public missing", async () => hex(await S.deriveBits({ name: "ECDH" }, ecdhPriv, 256)));
  await t("ECDH deriveKey to AES-GCM", async () => { const k = await S.deriveKey({ name: "ECDH", public: ecdhPeer }, ecdhPriv, { name: "AES-GCM", length: 256 }, true, BOTH); return hex(await S.exportKey("raw", k)); });

  // 12i. generateKey, and the parameters it polices.
  await t("generateKey RSA-OAEP 2048", async () => { const p = await S.generateKey({ name: "RSA-OAEP", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["encrypt", "decrypt"]); return (await S.exportKey("spki", p.publicKey)).byteLength + " " + p.publicKey.type + " " + p.privateKey.type + " " + JSON.stringify(p.publicKey.usages) + " " + JSON.stringify(p.privateKey.usages); });
  await t("generateKey RSA-OAEP 1024", async () => { const p = await S.generateKey({ name: "RSA-OAEP", modulusLength: 1024, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["encrypt", "decrypt"]); return p.publicKey.algorithm.modulusLength; });
  await t("generateKey RSA-OAEP 512", async () => { const p = await S.generateKey({ name: "RSA-OAEP", modulusLength: 512, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["encrypt", "decrypt"]); return p.publicKey.algorithm.modulusLength; });
  await t("generateKey RSA-OAEP exponent 3", async () => { const p = await S.generateKey({ name: "RSA-OAEP", modulusLength: 2048, publicExponent: new Uint8Array([3]), hash: "SHA-256" }, true, ["encrypt", "decrypt"]); return JSON.stringify(p.publicKey.algorithm.publicExponent); });
  await t("generateKey RSA-OAEP exponent 4", async () => { const p = await S.generateKey({ name: "RSA-OAEP", modulusLength: 2048, publicExponent: new Uint8Array([4]), hash: "SHA-256" }, true, ["encrypt", "decrypt"]); return "ok"; });
  await t("generateKey RSA-OAEP no hash", async () => { await S.generateKey({ name: "RSA-OAEP", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]) }, true, ["encrypt", "decrypt"]); return "ok"; });
  await t("generateKey ECDSA P-256", async () => { const p = await S.generateKey(ECDSA256, true, ["sign", "verify"]); return JSON.stringify(p.publicKey.algorithm) + " " + (await S.exportKey("raw", p.publicKey)).byteLength; });
  await t("generateKey ECDSA P-521", async () => { const p = await S.generateKey({ name: "ECDSA", namedCurve: "P-521" }, true, ["sign", "verify"]); return (await S.exportKey("raw", p.publicKey)).byteLength; });
  await t("generateKey ECDSA bogus curve", async () => { await S.generateKey({ name: "ECDSA", namedCurve: "P-257" }, true, ["sign", "verify"]); return "ok"; });
  await t("generateKey ECDH P-256 usages", async () => { const p = await S.generateKey(ECDH256, true, ["deriveBits", "deriveKey"]); return JSON.stringify(p.publicKey.usages) + " " + JSON.stringify(p.privateKey.usages); });
  await t("generateKey ECDSA with encrypt", async () => { await S.generateKey(ECDSA256, true, ["encrypt"]); return "ok"; });

  // 12j. wrapKey / unwrapKey with an RSA wrapping key -- the 25 counted WPT failures live here.
  await t("wrapKey raw under RSA-OAEP", async () => { const w = await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["wrapKey"]); const u = await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["unwrapKey"]); const wrapped = await S.wrapKey("raw", await raw(K16, "AES-CBC", BOTH), w, OAEP); const k = await S.unwrapKey("raw", wrapped, u, OAEP, "AES-CBC", true, BOTH); return wrapped.byteLength + " " + hex(await S.exportKey("raw", k)); });
  await t("wrapKey jwk under RSA-OAEP", async () => { const w = await imp("spki", b64d(RSA2048_SPKI), OAEP, true, ["wrapKey"]); const u = await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["unwrapKey"]); const wrapped = await S.wrapKey("jwk", await raw(K16, "AES-GCM", BOTH), w, OAEP); const k = await S.unwrapKey("jwk", wrapped, u, OAEP, "AES-GCM", true, BOTH); return hex(await S.exportKey("raw", k)); });
  await t("wrapKey spki of an RSA public key under AES-KW", async () => { const wrapped = await S.wrapKey("spki", oaepPub, kek, "AES-KW"); const k = await S.unwrapKey("spki", wrapped, kek, "AES-KW", OAEP, true, ["encrypt"]); return wrapped.byteLength + " " + desc(k); });
  await t("wrapKey pkcs8 of an RSA private key under AES-KW", async () => { const wrapped = await S.wrapKey("pkcs8", oaepPriv, kek, "AES-KW"); const k = await S.unwrapKey("pkcs8", wrapped, kek, "AES-KW", OAEP, true, ["decrypt"]); return wrapped.byteLength + " " + k.type; });
  await t("wrapKey raw of an EC public key under AES-KW", async () => { const wrapped = await S.wrapKey("raw", ecdsaPub, kek, "AES-KW"); return wrapped.byteLength; });
  await t("wrapKey too big for the RSA key", async () => { const w = await imp("spki", b64d(RSA512_SPKI), OAEP, true, ["wrapKey"]); return (await S.wrapKey("jwk", await raw(K32, "AES-GCM", BOTH), w, OAEP)).byteLength; });
  await t("unwrapKey RSA-OAEP garbage", async () => { const u = await imp("pkcs8", b64d(RSA2048_P8), OAEP, true, ["unwrapKey"]); return desc(await S.unwrapKey("raw", seq(256, 0), u, OAEP, "AES-CBC", true, BOTH)); });

  return R.join("\n");
})
