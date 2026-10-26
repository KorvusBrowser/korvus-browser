//! THE WIRE, MEASURED. A loopback TLS sink reads the ClientHello this process actually sends.
//!
//! Every other check in this repo asks the transport what it intends to emit. This one reads the
//! bytes off the socket, because the `tls-version` known_incoherence is a claim about the wire and a
//! claim about the wire is worth what the instrument behind it is worth. The sink serves no
//! certificate and the handshake dies immediately after the first flight -- which is all a
//! ClientHello fingerprint has ever been.
//!
//! LOOPBACK ONLY. Nothing here leaves the machine, so it runs in the gate like any other test.
//!
//! WHAT IT ESTABLISHED (2026-09-12, and what `known_incoherence "tls-version"` now records):
//!   * real Chrome 150.0.7871.46 on this box leads `signature_algorithms` with 0x0904/0x0905/0x0906
//!     (mldsa44/65/87) and then the classical eight -- the manifest's premise, confirmed against the
//!     installed browser rather than inherited;
//!   * `chrome_149`, which is what both bundles declare, sends only the classical eight;
//!   * `TlsOptions::sigalgs_list` DOES reach the wire (`sigalgs_list_reaches_the_wire` sets a list and
//!     reads it back off the socket) -- so the route is real;
//!   * and it could not carry the ML-DSA codepoints, because the only spelling `wreq` accepts is
//!     BoringSSL's NAME grammar, and the PUBLISHED `btls-sys 0.5.6`'s table had no ML-DSA entry.
//!     **That ceiling is gone** -- see "WHAT FP1 CHANGED" below.
//!
//! WHAT L9 ADDED (2026-09-14), AND THE ONE THING IT CORRECTED:
//!   * **the gap predicts the wall.** The same sink read the ClientHello of every shipping
//!     Chromium-family browser on this box -- Chrome 150.0.7871.46, Brave 151.1.93.138, Edge
//!     151.0.4129.93, Opera 135.0.5973.133 and a reference host-OS policy 151.0.7922.108.6, the stealth oracle that
//!     got through all 45 walled rows twice each. **All five emit the same JA4**
//!     `t13i1515h2_8daaf6152771_806a8c22fdea`, ML-DSA leading. This engine emits
//!     `..._d8a2da3f94cd`. So the wire separates korvus from the oracle exactly along the boundary the
//!     walls fall on, and it separates it from the persona's OWN source browser (Brave 151) too --
//!     the incoherence no longer has to be inferred from a browser the bundle does not claim.
//!   * **the ceiling is one layer deeper than it was recorded, and in a different crate.** The
//!     manifest used to say `SSL_CTX_set_verify_algorithm_prefs` "takes raw codepoints and does not
//!     validate them", so "the missing API is in `wreq`, NOT in BoringSSL". That was FALSE for the
//!     published pin: both routes funnel into one `set_sigalg_prefs`, which rejects any codepoint
//!     `get_signature_algorithm()` does not know -- the same `SSL_R_INVALID_SIGNATURE_ALGORITHM` the
//!     name route returns. So the ceiling was BoringSSL's `kSignatureAlgorithms` table, and both
//!     routes are still measured here rather than read.
//!   * **a second, independent wire defect, unique to the Opera bundle**: `opera_131` sends the
//!     SUPERSEDED ALPS codepoint 0x4469 where every browser measured above sends 0x44cd. Extensions
//!     are hashed by JA4, so this is a second JA4-visible difference on top of the sigalgs one --
//!     `opera_131` is further from real Opera 135 than `chrome_149` is. **FP1 corrected it** --
//!     `every_preset_sends_the_current_alps_codepoint` now asserts the correction.
//!   * eliminated as candidates, measured not assumed: ALPN (`h2`,`http/1.1` -- matches); cipher
//!     list, supported groups, key shares and supported versions (match once GREASE is excluded);
//!     extension SET (matches; ORDER is permuted per connection by both sides, which is why JA4
//!     sorts it); and header order/casing, read off a cleartext loopback socket -- korvus's order is
//!     Chrome's order, and its `sec-gpc` is correct, because real Brave 151 sends `Sec-GPC: 1` in
//!     exactly that slot. The HTTP/2 SETTINGS frame is NOT measured here: a completed handshake needs
//!     a certificate the browser trusts, and weakening the browser's launch line to accommodate the
//!     instrument is the one thing this lane must not do.
//!
//! WHAT FP1 CHANGED (2026-09-19) — THE CEILING IS GONE AND EVERY GUARD HERE WAS INVERTED, NOT DELETED:
//!   * the workspace `[patch.crates-io]` rev-pins `btls`/`btls-sys`/`tokio-btls` to a rev of upstream
//!     `main` whose BoringSSL submodule REGISTERS ML-DSA in libssl (upstream `4a3cda40b96`, merged
//!     three days after `btls-sys 0.5.6` was published). `korvus-transport::client_for` prepends
//!     `mldsa44:mldsa65:mldsa87` to the preset's own `sigalgs_list` and sets `alps_use_new_codepoint`.
//!   * **THE PIN ALONE MOVES NOTHING, measured and not a test in this file**: with the rev pin in
//!     place and the transport hunk REVERTED, all three presets still emitted the old JA4
//!     (`d8a2da3f94cd` / `02713d6af862`) and the classical eight. Upstream puts ML-DSA in
//!     `kSignSignatureAlgorithms` (signing), not in the *offer* default, so the bump alone changes
//!     nothing. The hunk is mandatory, and neither half works alone.
//!   * SIX guards in this file noticed the closing and were inverted:
//!     `the_pinned_boringssl_accepts_the_ml_dsa_sigalg_names`,
//!     `the_pinned_boringssl_accepts_the_ml_dsa_codepoints_on_the_raw_route_too`,
//!     `the_four_presets_emit_the_browser_familys_own_ja4`, `the_personas_preset_is_what_goes_on_the_wire`,
//!     `every_preset_sends_the_current_alps_codepoint` and
//!     `two_personas_in_one_process_each_get_their_own_clienthello`. A deleted guard is a regression
//!     waiting to land unseen; `the_post_quantum_key_share_is_still_offered` was ADDED because the
//!     build-log check the ML-KEM survival argument used to rest on stopped working under a git
//!     dependency.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

/// The parts of a ClientHello a fingerprint is computed from.
#[derive(Debug, Clone, Default)]
pub struct Hello {
    pub ciphers: Vec<u16>,
    pub extensions: Vec<u16>,
    pub sigalgs: Vec<u16>,
    pub groups: Vec<u16>,
    pub alpn: Vec<String>,
    pub sni: bool,
    pub versions: Vec<u16>,
    /// The `trust_anchors` extension's (0xca34) body, when the ClientHello carries one (Chrome 152).
    pub trust_anchors: Option<Vec<u8>>,
}

/// RFC 8701 GREASE values, which every fingerprint scheme excludes.
const GREASE: [u16; 16] = [
    0x0a0a, 0x1a1a, 0x2a2a, 0x3a3a, 0x4a4a, 0x5a5a, 0x6a6a, 0x7a7a, 0x8a8a, 0x9a9a, 0xaaaa, 0xbaba,
    0xcaca, 0xdada, 0xeaea, 0xfafa,
];

fn u16at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn list16(body: &[u8]) -> Vec<u16> {
    let Some(n) = u16at(body, 0) else { return Vec::new() };
    (0..n as usize).step_by(2).filter_map(|k| u16at(body, 2 + k)).collect()
}

impl Hello {
    /// Parse the handshake-message body of a ClientHello.
    fn parse(rec: &[u8]) -> Option<Hello> {
        let mut h = Hello::default();
        let mut i = 2 + 32; // legacy_version + random
        i += 1 + *rec.get(i)? as usize; // legacy_session_id
        let cs_len = u16at(rec, i)? as usize;
        i += 2;
        h.ciphers = (0..cs_len).step_by(2).filter_map(|k| u16at(rec, i + k)).collect();
        i += cs_len;
        i += 1 + *rec.get(i)? as usize; // legacy_compression_methods
        let total = u16at(rec, i)? as usize;
        i += 2;
        let end = (i + total).min(rec.len());
        while i + 4 <= end {
            let et = u16at(rec, i)?;
            let el = u16at(rec, i + 2)? as usize;
            i += 4;
            let body = rec.get(i..(i + el).min(rec.len()))?;
            i += el;
            h.extensions.push(et);
            match et {
                0x000d => h.sigalgs = list16(body),
                0x000a => h.groups = list16(body),
                0x0000 => h.sni = true,
                0xca34 => h.trust_anchors = Some(body.to_vec()),
                0x0010 => {
                    let n = u16at(body, 0).unwrap_or(0) as usize;
                    let mut j = 2usize;
                    while j < 2 + n && j < body.len() {
                        let ln = body[j] as usize;
                        if let Some(s) = body.get(j + 1..j + 1 + ln) {
                            h.alpn.push(String::from_utf8_lossy(s).into_owned());
                        }
                        j += 1 + ln;
                    }
                }
                0x002b => {
                    let n = *body.first()? as usize;
                    h.versions = (0..n).step_by(2).filter_map(|k| u16at(body, 1 + k)).collect();
                }
                _ => {}
            }
        }
        Some(h)
    }

    fn ungreased(v: &[u16]) -> Vec<u16> {
        v.iter().copied().filter(|x| !GREASE.contains(x)).collect()
    }

    /// The JA4 TLS client fingerprint, so this test speaks the same language as the manifest's
    /// recorded `t13d1516h2_8daaf6152771_d8a2da3f94cd`.
    pub fn ja4(&self) -> String {
        use sha2::{Digest, Sha256};
        let h = |t: &str| {
            let d = Sha256::digest(t.as_bytes());
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()[..12].to_string()
        };
        let ciphers = Self::ungreased(&self.ciphers);
        let exts = Self::ungreased(&self.extensions);
        let sigalgs = Self::ungreased(&self.sigalgs);
        let ver = match Self::ungreased(&self.versions).into_iter().max().unwrap_or(0x0303) {
            0x0304 => "13",
            0x0303 => "12",
            0x0302 => "11",
            0x0301 => "10",
            _ => "00",
        };
        let alpn = self.alpn.first().map_or("00".to_string(), |a| {
            let c: Vec<char> = a.chars().collect();
            format!("{}{}", c.first().unwrap_or(&'0'), c.last().unwrap_or(&'0'))
        });
        let head = format!(
            "t{ver}{}{:02}{:02}{alpn}",
            if self.sni { "d" } else { "i" },
            ciphers.len().min(99),
            exts.len().min(99)
        );
        let hex = |v: &[u16]| v.iter().map(|x| format!("{x:04x}")).collect::<Vec<_>>().join(",");
        let mut sorted_ciphers = ciphers.clone();
        sorted_ciphers.sort_unstable();
        // JA4_c hashes the extensions SORTED and with SNI (0000) and ALPN (0010) removed, then the
        // signature algorithms IN THE ORDER THEY WERE OFFERED -- which is why a reordered sigalgs
        // list changes the third component and nothing else.
        let mut sorted_exts: Vec<u16> = exts.iter().copied().filter(|e| *e != 0x0000 && *e != 0x0010).collect();
        sorted_exts.sort_unstable();
        let mut c = hex(&sorted_exts);
        if !sigalgs.is_empty() {
            c.push('_');
            c.push_str(&hex(&sigalgs));
        }
        format!("{head}_{}_{}", h(&hex(&sorted_ciphers)), h(&c))
    }
}

/// Bind a loopback TLS sink, hand its port to `drive`, and return the first ClientHello it reads.
///
/// `drive` is whatever provokes the connection -- the transport's own fetch, or a bespoke client.
/// Its result is discarded: the handshake ALWAYS fails (no certificate is served) and the failure is
/// not the measurement. The first flight is.
fn capture<F: FnOnce(u16)>(drive: F) -> Hello {
    let srv = TcpListener::bind("127.0.0.1:0").expect("bind loopback sink");
    let port = srv.local_addr().expect("sink addr").port();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        // Up to four accepts: a client that retries a failed handshake must not race the sink.
        for _ in 0..4 {
            let Ok((mut conn, _)) = srv.accept() else { break };
            if let Some(h) = read_hello(&mut conn) {
                let _ = tx.send(h);
                return;
            }
        }
    });
    drive(port);
    let hello = rx.recv_timeout(Duration::from_secs(20)).expect(
        "no ClientHello reached the loopback sink -- the client never opened a TLS connection to it",
    );
    let _ = reader.join();
    hello
}

fn read_hello(conn: &mut TcpStream) -> Option<Hello> {
    conn.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = conn.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() >= 5 {
            let need = 5 + u16at(&buf, 3)? as usize;
            if buf.len() >= need {
                break;
            }
        }
    }
    // record: type(1) version(2) length(2); handshake: type(1) length(3) body
    if buf.first() != Some(&0x16) {
        return None;
    }
    let rec = buf.get(5..5 + u16at(&buf, 3)? as usize)?;
    // Every index below is checked: a truncated or fragmented record must make this return None so
    // the sink tries the next connection, never panic inside the instrument and report that as the
    // measurement.
    if rec.first() != Some(&0x01) || rec.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([0, rec[1], rec[2], rec[3]]) as usize;
    Hello::parse(rec.get(4..4 + len.min(rec.len() - 4))?)
}

/// The eight classical signature algorithms Chrome has offered since well before 149, in its order.
/// Written as a literal because it is what the measurement is COMPARED AGAINST, not what produces it.
const CLASSICAL: [u16; 8] = [0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601];
/// draft-ietf-tls-mldsa codepoints, which real Chrome 150 leads with. Measured, not assumed --
/// see the module header.
const MLDSA: [u16; 3] = [0x0904, 0x0905, 0x0906];

fn emulation_of(profile: wreq_util::Profile) -> wreq::Emulation {
    use wreq::IntoEmulation;
    wreq_util::Emulation::builder().profile(profile).build().into_emulation()
}

fn hex(v: &[u16]) -> String {
    v.iter().map(|x| format!("{x:04x}")).collect::<Vec<_>>().join(",")
}

/// What the shipped path actually puts on the wire, read off the socket.
///
/// This is the transport's own `fetch`, not a bespoke client, so it measures the code a real run
/// uses. The preset arrives the way every request's preset arrives now -- on the request, from the
/// persona that made it ([`Wearer`]) -- rather than from a process-wide `configure`, which no longer
/// exists.
///
/// NOT `#[tokio::test]`: `capture` blocks this thread waiting for the sink, so a current-thread
/// runtime would never poll the fetch it is waiting for. The driver gets its own runtime on its own
/// thread, the same shape the two tests below use.
#[test]
fn the_personas_preset_is_what_goes_on_the_wire() {
    let hello = hello_of(&Wearer::new("chrome_149", "linux", "UA/chrome_149"));

    eprintln!("chrome_149 on the wire: ja4={} sigalgs=[{}]", hello.ja4(), hex(&hello.sigalgs));
    assert!(hello.ja4().starts_with("t13"), "Chrome's emulation must offer TLS 1.3: {}", hello.ja4());
    assert_eq!(
        hello.alpn,
        vec!["h2".to_string(), "http/1.1".to_string()],
        "Chrome offers h2 then http/1.1"
    );
    // THE GAP THAT CLOSED, MEASURED. This block used to assert the sigalgs were
    // EXACTLY the classical eight -- the incoherence, in bytes. It is inverted, not deleted: the
    // browsers lead with the three ML-DSA codepoints and now so does korvus, and the shape asserted
    // is the shape it must keep -- the three, first, IN THAT ORDER, then the preset's own tail
    // unchanged and in the preset's own order. A prepend that reordered or dropped the tail would
    // change the JA4 just as surely as no prepend at all.
    //
    // AND A GREASE VALUE AHEAD OF ALL OF THEM: Chrome 152's list leads with one (tls.peet.ws's peetprint off
    // a reference host-OS policy 152, 2026-10-22), and a device-check script answered 429 on expedia.com to korvus's list without it, 6 of 6,
    // and 200 with it, 6 of 6, the same build otherwise. JA4 drops GREASE, so only the raw list shows it.
    let mut expected = MLDSA.to_vec();
    expected.extend_from_slice(&CLASSICAL);
    assert!(
        hello.sigalgs.first().is_some_and(|g| GREASE.contains(g)),
        "chrome_149 must lead its signature_algorithms with a GREASE value, as Chrome 152 does: [{}]",
        hex(&hello.sigalgs)
    );
    assert_eq!(
        hello.sigalgs[1..], expected[..],
        "chrome_149 must offer the three ML-DSA codepoints, in order, ahead of the preset's own \
         classical eight in the preset's own order"
    );
}

/// POSITIVE CONTROL: `TlsOptions::sigalgs_list` really does reach the socket.
///
/// Without this, the rejection measured in the next test would be indistinguishable from "the field
/// is ignored" -- and a ceiling reported off an unexercised mechanism is the failure mode the unit
/// contract names by name. Here a deliberately REORDERED classical list goes in and comes back out of
/// the wire in that order, so the route is proven end to end before it is reported as blocked.
#[test]
fn sigalgs_list_reaches_the_wire() {
    let reordered: [u16; 8] = [0x0601, 0x0806, 0x0501, 0x0805, 0x0503, 0x0401, 0x0804, 0x0403];
    let names = "rsa_pkcs1_sha512:rsa_pss_rsae_sha512:rsa_pkcs1_sha384:rsa_pss_rsae_sha384:\
                 ecdsa_secp384r1_sha384:rsa_pkcs1_sha256:rsa_pss_rsae_sha256:ecdsa_secp256r1_sha256";

    let mut em = emulation_of(wreq_util::Profile::Chrome149);
    let mut tls = em.tls_options.expect("chrome_149 carries TlsOptions");
    tls.sigalgs_list = Some(names.into());
    em.tls_options = Some(tls);

    let hello = capture(|port| {
        let client = wreq::Client::builder()
            .emulation(em)
            .build()
            .expect("a client with a VALID custom sigalgs_list must build");
        let url = format!("https://127.0.0.1:{port}/");
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt")
                .block_on(async { client.get(&url).send().await.err() })
        })
        .join()
        .expect("driver thread");
    });

    eprintln!("custom sigalgs on the wire: [{}]", hex(&hello.sigalgs));
    assert_eq!(
        hello.sigalgs, reordered,
        "the sigalgs_list handed to TlsOptions must be the sigalgs on the socket, in that order"
    );
    assert_ne!(hello.sigalgs, CLASSICAL, "the control must differ from the preset's own order");
}

/// Build a client whose emulation is `chrome_149` with `sigalgs_list` replaced. `Ok` means the whole
/// list was accepted; `Err` carries what the stack said.
fn try_sigalgs(list: &str) -> Result<(), String> {
    let mut em = emulation_of(wreq_util::Profile::Chrome149);
    let mut tls = em.tls_options.expect("chrome_149 carries TlsOptions");
    tls.sigalgs_list = Some(list.to_string().into());
    em.tls_options = Some(tls);
    wreq::Client::builder().emulation(em).build().map(|_| ()).map_err(|e| e.to_string())
}

/// **THE CEILING IS GONE, AND THIS IS THE GUARD THAT SAYS IT IS STILL GONE.**
///
/// This test used to be `mldsa_sigalg_names_are_rejected_by_the_pinned_boringssl` and it asserted
/// the refusal. It was INVERTED rather than deleted, because the thing it now measures is the load-
/// bearing half of a supply-chain decision: `wreq`'s only sigalgs spelling is a string routed to
/// `SSL_CTX_set1_sigalgs_list`, so `korvus-transport::client_for` can prepend the three ML-DSA names
/// only while the linked BoringSSL registers them in `kSignatureAlgorithmNames`. The workspace
/// `[patch.crates-io]` rev pin is what puts a BoringSSL there that does. **A `[patch]` override
/// matches by name and semver range, so a future `btls-sys 0.6` -- or a hand-deleted block -- drops
/// out silently and NOTHING FAILS TO COMPILE.** `Client::build` would then fail at runtime on the
/// first fetch, or, worse, an engine that stopped prepending would go quietly back to the old wire.
/// This is the red that catches it at the gate instead.
///
/// It still records EVERY spelling, because which ones work is the finding: only the lowercase
/// `kSignatureAlgorithmNames` form is a name BoringSSL's `parse_sigalgs_list` knows. The parser has
/// no numeric or hex escape and never had one -- that half of the old finding still holds, and the
/// candidates below keep measuring it rather than asserting it from memory.
#[test]
fn the_pinned_boringssl_accepts_the_ml_dsa_sigalg_names() {
    // The classical tail Chrome 150 offers after the three ML-DSA entries. Appended to each
    // candidate so an outcome can only be about the ML-DSA spelling, never about the rest.
    const TAIL: &str = ":ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:\
                        ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:\
                        rsa_pss_rsae_sha512:rsa_pkcs1_sha512";
    // The spelling `client_for` actually passes. Written out here rather than imported so this test
    // fails if the engine's string and BoringSSL's table ever drift apart.
    const ENGINE_SPELLING: &str = "mldsa44:mldsa65:mldsa87";

    // CONTROL: the tail alone must be accepted, or every outcome below proves nothing.
    try_sigalgs(TAIL.trim_start_matches(':')).expect("the classical tail alone must be accepted");

    // THE ASSERTION THIS TEST EXISTS FOR.
    try_sigalgs(&format!("{ENGINE_SPELLING}{TAIL}")).unwrap_or_else(|e| {
        panic!(
            "the linked BoringSSL REFUSED {ENGINE_SPELLING:?}: {e}. The ML-DSA registration is not \
             in the BoringSSL this build linked, so `korvus-transport::client_for` cannot prepend \
             the three codepoints and every fetch fails at `Client::build`. Check the workspace \
             `[patch.crates-io]` rev pin: a `[patch]` override matches by name and semver range, so \
             a `btls-sys` major bump drops it out with nothing failing to compile."
        )
    });

    // EVERY OTHER SPELLING, AND WHAT EACH RETURNED. `parse_sigalgs_list` takes a TLS-1.3-style NAME
    // out of `kSignatureAlgorithmNames`, or a `PKEY+HASH` pair; it is case-sensitive and has no
    // numeric or hex escape. That is measured here, not remembered.
    let must_refuse = [
        "MLDSA44:MLDSA65:MLDSA87",       // the same names, wrong case
        "ml_dsa_44:ml_dsa_65:ml_dsa_87",
        "ML-DSA-44:ML-DSA-65:ML-DSA-87", // the OBJECT name crypto/obj carries
        "id-ml-dsa-44:id-ml-dsa-65:id-ml-dsa-87",
        "0x0904:0x0905:0x0906", // a hex escape, if the parser had one
        "2308:2309:2310",       // the decimal codepoints, if it had that one
    ];
    let mut surprises = Vec::new();
    for c in must_refuse {
        match try_sigalgs(&format!("{c}{TAIL}")) {
            Ok(()) => surprises.push(c),
            Err(e) => eprintln!("  {c:40} -> {e}"),
        }
    }
    assert!(
        surprises.is_empty(),
        "the parser gained a spelling it did not have: {surprises:?}. Not a failure of the wire -- \
         a change in the pinned BoringSSL's grammar. Re-read `parse_sigalgs_list` before relying on it."
    );

    // And the refusal above is specifically about the signature algorithm, not some unrelated build
    // failure that would have masked what is being measured.
    let err = try_sigalgs(&format!("MLDSA65{TAIL}")).expect_err("the wrong-case name must be refused");
    eprintln!("ML-DSA sigalgs_list: {ENGINE_SPELLING:?} accepted; \"MLDSA65\" -> {err}");
    let low = err.to_lowercase();
    assert!(
        low.contains("signature") || low.contains("sigalg"),
        "expected a signature-algorithm refusal, got: {err}"
    );
}

/// **THE SAME CEILING, ONE LAYER DOWN — AND IT IS GONE THERE TOO.**
///
/// This test used to be `mldsa_codepoints_are_rejected_by_the_raw_api_too` and it asserted the
/// refusal. INVERTED, not deleted, and it is not a duplicate of the one above: the two reach
/// `set_sigalg_prefs` by different routes, and a BoringSSL that registered the codepoints but not
/// the names (or the reverse) would pass one and fail the other. That is not hypothetical — the
/// hand-backport FP1 built in its phase 2a missed `kSignatureAlgorithmNames` and would have compiled,
/// passed its own tests and moved nothing on the wire.
///
/// ```text
/// SSL_CTX_set1_sigalgs_list(ctx, str)        -> parse_sigalgs_list  -> set_sigalg_prefs   (names)
/// SSL_CTX_set_verify_algorithm_prefs(ctx, …) ->                        set_sigalg_prefs   (codepoints)
///
/// static bool set_sigalg_prefs(Array<uint16_t> *out, Span<const uint16_t> prefs) {
///   …
///   if (get_signature_algorithm(pref) == nullptr) {
///     OPENSSL_PUT_ERROR(SSL, SSL_R_INVALID_SIGNATURE_ALGORITHM);
///     return false;                       // <-- what BOTH routes used to die on
/// ```
///
/// `get_signature_algorithm()` looks the codepoint up in `kSignatureAlgorithms`, and the BoringSSL
/// the rev pin brings in has the three ML-DSA rows there. `wreq` still exposes no codepoint-shaped
/// sigalgs field, so the engine does not use this route — this measures the TABLE, which is the
/// thing the engine's name route depends on.
///
/// `btls` is a DEV-dependency for this test alone. It adds no crate to the graph: wreq 0.16.1 already
/// links btls 0.5.6, it is the only version in `Cargo.lock`, and it is Apache-2.0 like the rest.
/// Naming it directly is the only way to call the raw-codepoint API at all, because `wreq` neither
/// re-exports btls nor hands out an `SslContextBuilder`.
#[test]
fn the_pinned_boringssl_accepts_the_ml_dsa_codepoints_on_the_raw_route_too() {
    use btls::ssl::{SslContextBuilder, SslMethod, SslSignatureAlgorithm};

    let prefs = |v: &[u16]| -> Vec<SslSignatureAlgorithm> {
        v.iter().copied().map(SslSignatureAlgorithm::from).collect()
    };
    let set = |v: &[u16]| -> Result<(), String> {
        let mut b = SslContextBuilder::new(SslMethod::tls()).map_err(|e| e.to_string())?;
        b.set_verify_algorithm_prefs(&prefs(v)).map_err(|e| e.to_string())
    };

    // CONTROL: the classical eight go in through THIS API, so every outcome below is about the
    // ML-DSA codepoints and not about the API, the builder, or the shape of the call.
    set(&CLASSICAL).expect(
        "the classical eight must be accepted by the raw codepoint API, or this test measures nothing",
    );

    // NEGATIVE CONTROL: a codepoint no `kSignatureAlgorithms` row can carry must STILL be refused.
    // Without it, a validator that had simply stopped validating would pass everything below.
    let bogus = {
        let mut v = vec![0x0999u16];
        v.extend_from_slice(&CLASSICAL);
        set(&v).expect_err(
            "an unassigned sigalg codepoint was ACCEPTED -- `set_sigalg_prefs` is no longer \
             validating, so this test can no longer tell a registered algorithm from an invented one",
        )
    };
    eprintln!("  0x0999 (unassigned) -> {bogus}");

    // THE MEASUREMENT: Chrome 150's real list -- the three ML-DSA codepoints then those same eight.
    let mut chrome150 = MLDSA.to_vec();
    chrome150.extend_from_slice(&CLASSICAL);
    set(&chrome150).unwrap_or_else(|e| {
        panic!(
            "the raw codepoint route REFUSED the browsers' own sigalgs list: {e}. \
             `kSignatureAlgorithms` has no ML-DSA rows, so the linked BoringSSL is not the one the \
             workspace `[patch.crates-io]` rev pin names -- check that the pin is still in effect."
        )
    });

    // And each codepoint ALONE, so the record says which are registered rather than just "the list".
    for m in MLDSA {
        let mut one = vec![m];
        one.extend_from_slice(&CLASSICAL);
        set(&one).unwrap_or_else(|e| {
            panic!("{m:#06x} alone was REFUSED by the raw codepoint route: {e}")
        });
        eprintln!("  {m:#06x} alone -> accepted");
    }
}

/// The three presets the four shipped bundles declare: `chrome_149` (Brave 151, Chrome 150),
/// `edge_148` (Edge 151), `opera_131` (Opera 135).
const DECLARED_PRESETS: [&str; 3] = ["chrome_149", "edge_148", "opera_131"];

/// **THE POST-QUANTUM KEY SHARE SURVIVED THE REV PIN — CHECKED ON THE SOCKET.**
///
/// `0x11ec` (`SSL_GROUP_X25519_MLKEM768`) exists only because `btls-sys`'s `boring-pq.patch` is
/// applied to BoringSSL. The hazard the workspace `[patch.crates-io]` rev pin introduces is gaining
/// the post-quantum SIGNATURE advertisement while silently losing the post-quantum KEY EXCHANGE --
/// half the handshake, changed by a dependency move, with nothing failing to compile.
///
/// **And the cheap check for it stopped working, which is why this test exists.** `btls-sys`'s build
/// script prints `cargo:warning=applying post quantum crypto patch to boringssl`, and cargo shows
/// build-script warnings only for PATH packages -- under a GIT dependency they are suppressed, so the
/// build log no longer contains that line at all. Reading the log would now look identical whether
/// the patch applied or not. The socket is the only instrument that still works.
#[test]
fn the_post_quantum_key_share_is_still_offered() {
    const X25519_MLKEM768: u16 = 0x11ec;
    for preset in DECLARED_PRESETS {
        let hello = hello_of(&Wearer::new(preset, "linux", &format!("UA/{preset}")));
        eprintln!("{preset:12} groups=[{}]", hex(&hello.groups));
        assert!(
            hello.groups.contains(&X25519_MLKEM768),
            "{preset} no longer offers X25519MLKEM768 ({X25519_MLKEM768:#06x}); \
             `boring-pq.patch` did not reach the BoringSSL this build linked. Do not read the build \
             log to diagnose it -- a git dependency suppresses build-script warnings. Groups on the \
             wire: [{}]",
            hex(&hello.groups)
        );
    }
}

/// What every shipping Chromium-family browser on this box put on THIS sink (L9, 2026-09-14):
/// Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93, Opera 135.0.5973.133 and
/// a reference host-OS policy 151.0.7922.108.6. A literal because it is what the measurement is COMPARED AGAINST.
///
/// **The head is `t13i1515h2` and not `t13d1516h2` because THIS sink is addressed by IP literal**,
/// which suppresses SNI; the same browsers addressed by name emit `t13d1516h2` with the identical
/// cipher and sigalgs hashes. FP1.3 settled that: `15 -> 16` is the `server_name` extension alone,
/// and a further `16 -> 17` is `pre_shared_key`, i.e. a resumed session. Only the two HASHES are
/// compared here, and they are the same either way.
const BROWSER_FAMILY_JA4: &str = "t13i1515h2_8daaf6152771_806a8c22fdea";

/// Google Chrome 152's JA4 on this sink, MEASURED 2026-10-16 off the extracted 152.0.7977.82 (a fresh profile, two
/// connections alike; the same sink by IP literal): the family's ciphers and sigalgs, plus Chrome 152's
/// `trust_anchors` extension (0xca34) -- one more extension, so another count and another extension hash.
const CHROME_152_JA4: &str = "t13i1516h2_8daaf6152771_cb7bf5808d99";

/// **THE SHIPPED PERSONA IS CHROME 152 ON THE WIRE.** The presets know nothing of `trust_anchors`: the bundle records
/// the body Chrome 152 was measured sending (`[transport] trust_anchors`), `client_for` hands it to BoringSSL through
/// vendored wreq's patch, and the ClientHello carries it byte for byte -- which is what moves the JA4 from the
/// family's to Chrome 152's. Drop the manifest field, the `client_for` step or the wreq patch and this goes red.
#[test]
fn the_shipped_persona_writes_chrome_152s_clienthello() {
    let shipped = persona::CapturedPersona::shipped();
    let wire = spec::Persona::transport_profile(&shipped);
    let want = wire.trust_anchors.clone().expect("the shipped bundle records Chrome 152's trust_anchors body");
    let hello = hello_of(&Wearer { wire, ua: "UA/shipped".into() });
    let body: String = hello.trust_anchors.as_deref().unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(body, want, "the ClientHello's trust_anchors extension is not the one Chrome 152 was measured sending");
    assert_eq!(hello.ja4(), CHROME_152_JA4, "the shipped persona's ClientHello is not Chrome 152's");
}

/// **A `wss://` HANDSHAKE WEARS THE SAME CORRECTED CLIENTHELLO AS THE PAGE'S FETCHES, BUT FOR ITS ALPN.**
///
/// The WebSocket client is a client of its own (Chrome's separate WebSocket socket pool, ALPN `http/1.1` only and
/// therefore no ALPS), and it used to build its TLS options straight from the preset -- so it went out without the
/// ML-DSA sigalgs and the trust anchors `client_for` adds, a JA4 matching neither the page's own fetches nor any
/// Chrome. Both clients now take the corrections from one function; this is the red if the WebSocket one stops.
#[test]
fn a_websocket_wears_the_page_clients_corrected_clienthello() {
    let shipped = persona::CapturedPersona::shipped();
    let wearer = Wearer { wire: spec::Persona::transport_profile(&shipped), ua: "UA/shipped".into() };
    let fetch = hello_of(&wearer);
    let ws = ws_hello_of(&wearer);
    eprintln!("fetch ja4={}  websocket ja4={}", fetch.ja4(), ws.ja4());
    assert_eq!(ws.alpn, vec!["http/1.1".to_string()], "a fresh wss:// connection offers http/1.1 only (AlpnMode::kHttp11Only)");
    assert!(
        !ws.extensions.contains(&ALPS_NEW) && !ws.extensions.contains(&ALPS_OLD),
        "no ALPS beside an http/1.1-only ALPN: Chrome configures it only for a protocol it offers"
    );
    for m in MLDSA {
        assert!(ws.sigalgs.contains(&m), "the WebSocket ClientHello does not offer ML-DSA {m:#06x}: [{}]", hex(&ws.sigalgs));
    }
    // GREASE is drawn per connection, so the lists are compared without it, as every fingerprint scheme does.
    let real = |c: &[u16]| c.iter().copied().filter(|x| !GREASE.contains(x)).collect::<Vec<_>>();
    assert!(ws.sigalgs.first().is_some_and(|g| GREASE.contains(g)), "the WebSocket's signature_algorithms lead with no GREASE");
    assert_eq!(real(&ws.sigalgs), real(&fetch.sigalgs), "the WebSocket's signature_algorithms differ from the page's fetches'");
    assert_eq!(ws.trust_anchors, fetch.trust_anchors, "the WebSocket's trust_anchors differ from the page's fetches'");
    assert_eq!(real(&ws.ciphers), real(&fetch.ciphers), "the WebSocket's cipher suites differ from the page's fetches'");
}

/// Drive one real `wss://` handshake through `transport::websocket`, worn by `p`, at the loopback sink, and return
/// the ClientHello it put on the socket. The handle is held until the sink has read the hello: dropping it is the page
/// abandoning the socket, which cancels the connect.
fn ws_hello_of(p: &Wearer) -> Hello {
    let id = transport::persona_identity(p);
    let mut held = None;
    let hello = capture(|port| {
        let req = transport::websocket::WsRequest {
            url: format!("wss://127.0.0.1:{port}/"),
            protocols: Vec::new(),
            origin: "https://127.0.0.1".into(),
            headers: Vec::new(),
            wire: None,
        }
        .worn_by(&id);
        held = Some(transport::websocket::open(req));
    });
    drop(held);
    hello
}

/// **THE GAP IS CLOSED, AND THIS IS WHAT HOLDS IT CLOSED.**
///
/// This test was `the_declared_presets_carry_the_recorded_tls_version_gap` and it asserted the gap
/// was NON-EMPTY, with failure text saying what to do the day it closed. It closed on 2026-09-19, so
/// the test is INVERTED rather than deleted and the instruction is carried out: every preset the four
/// shipped bundles declare now emits the JA4 this box measured off real Brave 151, Chrome 150, Edge
/// 151, Opera 135 and a reference host-OS policy 151, and the `known_incoherence "tls-version"` entries no longer
/// claim a wire gap.
///
/// **A guard that is deleted instead of inverted is a regression waiting to land unseen**, and the
/// regression here is specific: the three codepoints reach the wire only while the workspace
/// `[patch.crates-io]` rev pin is in effect AND `korvus-transport::client_for` prepends them. Drop
/// either and **nothing fails to compile** -- a `[patch]` override matches by name and semver range,
/// so a `btls-sys` major bump silently removes it. This is the red that catches it.
#[test]
fn the_four_presets_emit_the_browser_familys_own_ja4() {
    for preset in DECLARED_PRESETS {
        let hello = hello_of(&Wearer::new(preset, "linux", &format!("UA/{preset}")));
        let ja4 = hello.ja4();
        eprintln!("{preset:12} ja4={ja4}  sigalgs=[{}]", hex(&hello.sigalgs));

        assert_eq!(
            ja4, BROWSER_FAMILY_JA4,
            "{preset} no longer emits the browser family's JA4. Either the workspace \
             `[patch.crates-io]` rev pin dropped out (it matches by name and semver range, so a \
             `btls-sys` major bump removes it with nothing failing to compile), or `client_for` \
             stopped correcting the preset, or the pinned `wreq-util` changed the preset itself. \
             Measure which before touching this number -- it is a browser's wire, not a choice."
        );
        // The codepoints themselves, rather than only the hash that stands for them: a hash that
        // matched for some other reason would otherwise pass.
        for m in MLDSA {
            assert!(
                hello.sigalgs.contains(&m),
                "{preset} does not offer ML-DSA {m:#06x}. sigalgs on the wire: [{}]",
                hex(&hello.sigalgs)
            );
        }
        // And the current ALPS codepoint, which was `opera_131`'s own separate difference.
        assert!(
            hello.extensions.contains(&ALPS_NEW) && !hello.extensions.contains(&ALPS_OLD),
            "{preset} must send the current ALPS codepoint {ALPS_NEW:#06x} and not the superseded \
             {ALPS_OLD:#06x}"
        );
    }
}

/// ALPS (`application_settings`) moved codepoint: `0x4469` was the draft spelling, `0x44cd`
/// superseded it. Every browser measured on this sink — including real Opera 135.0.5973.133 — sends
/// **0x44cd**.
const ALPS_NEW: u16 = 0x44cd;
/// The draft ALPS codepoint. `opera_131` sent this, alone among the presets, because
/// `wreq::TlsOptions::alps_use_new_codepoint` is false in it. JA4 hashes the extension list, so it
/// was a JA4-visible difference ON TOP of the sigalgs one.
const ALPS_OLD: u16 = 0x4469;

/// **THE SECOND WIRE DEFECT, AND IT IS NOT THE SIGALGS — NOW CLOSED.**
///
/// This was `opera_131_sends_the_superseded_alps_codepoint`, which asserted the defect was PRESENT
/// and said what to do when it went away. INVERTED, not deleted. `client_for` now sets
/// `alps_use_new_codepoint` for every preset, which is a no-op for `chrome_*`/`edge_*` and is what
/// takes the Opera bundle from `c8687cd82e98` to `806a8c22fdea` once the sigalgs are corrected.
///
/// **The fact that made the old defect worth recording still stands and is why this asserts EVERY
/// preset rather than only Opera's:** real Opera 135's ClientHello is byte-for-byte Chrome's, so
/// `opera_131`'s difference bought a distinction no shipping Opera has. Anything reintroducing it —
/// a preset change, a `wreq` bump, a conditional in `client_for` — is a regression, not a nuance.
#[test]
fn every_preset_sends_the_current_alps_codepoint() {
    for preset in DECLARED_PRESETS {
        let hello = hello_of(&Wearer::new(preset, "linux", &format!("UA/{preset}")));
        eprintln!(
            "{preset:12} alps: new={} old={}",
            hello.extensions.contains(&ALPS_NEW),
            hello.extensions.contains(&ALPS_OLD)
        );
        assert!(
            hello.extensions.contains(&ALPS_NEW),
            "{preset} does not send the current ALPS codepoint {ALPS_NEW:#06x}, which every browser \
             measured on this sink sends"
        );
        assert!(
            !hello.extensions.contains(&ALPS_OLD),
            "{preset} still sends the SUPERSEDED ALPS codepoint {ALPS_OLD:#06x}. JA4 hashes the \
             extension list, so this alone moves the third component off the browsers' own."
        );
    }
}

// ── THE PROPERTY: A REQUEST WEARS THE WIRE OF THE PERSONA THAT MADE IT ───────────────────────────
//
// `transport::configure` used to guarantee this by refusing the second persona -- one process, one
// wire. That is right for a browser and wrong for a binary that boots four bundles to compare their
// JS surfaces, and L7f measured that moving the refusal to `client()` instead poisons every later
// fetch in the binary (L7f.9). The guarantee did not move: it is now structural, because the wire is
// a field of the request written from the same persona, in the same call, as the identity headers.
//
// A structural guarantee is worth nothing unasserted, so it is asserted here in BYTES -- three ways:
//
//   1. two personas fetch in ONE process and each ClientHello equals that persona's preset and not
//      the other's (`two_personas_in_one_process_each_get_their_own_clienthello`);
//   2. the UA header and the wire on a single request come from one persona, so a server reading both
//      sees one device (`the_wire_and_the_user_agent_come_from_the_same_persona`);
//   3. a request no persona claimed opens NO SOCKET AT ALL (`a_request_with_no_persona_is_refused`).

/// A minimal persona whose only interesting facts are its wire and its UA. `spec::Persona` requires
/// three methods; the rest default, and none of them reach the socket.
struct Wearer {
    wire: spec::TlsProfile,
    ua: String,
}

impl Wearer {
    fn new(id: &str, platform: &str, ua: &str) -> Self {
        Wearer {
            wire: spec::TlsProfile { id: id.into(), platform: platform.into(), ..Default::default() },
            ua: ua.to_string(),
        }
    }
}

impl spec::Persona for Wearer {
    fn navigator(&self) -> spec::NavigatorProfile {
        spec::NavigatorProfile { user_agent: self.ua.clone(), ..spec::NavigatorProfile::default() }
    }
    fn screen(&self) -> spec::ScreenProfile {
        spec::ScreenProfile::default()
    }
    fn transport_profile(&self) -> spec::TlsProfile {
        self.wire.clone()
    }
}

/// Drive one real `transport::fetch`, worn by `p`, at the loopback sink, and return the ClientHello it
/// put on the socket.
fn hello_of(p: &Wearer) -> Hello {
    let req = transport::FetchRequest::navigation("https://127.0.0.1:0/").worn_by(p);
    assert_eq!(
        req.wire.as_ref().map(|w| w.id.as_str()),
        Some(p.wire.id.as_str()),
        "worn_by must stamp the persona's own wire onto the request"
    );
    capture(|port| {
        let mut req = req.clone();
        req.url = format!("https://127.0.0.1:{port}/");
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt")
                .block_on(async move { transport::fetch(req).await.err() })
        })
        .join()
        .expect("driver thread");
    })
}

// `reference_hello` -- the ClientHello a PLAIN `wreq` client of a profile sends -- lived here.
// It is gone because it is no longer a reference for anything: `client_for` corrects two
// measured fields of every preset, so the engine's hello is deliberately not the raw library's, and
// comparing against the raw one would assert the corrections away. The reference is now a second
// persona driven through the same `transport::fetch`, which is what the property is actually about.

/// **THE ONE-WIRE PROPERTY, IN ONE PROCESS, WITH TWO PERSONAS.**
///
/// **THE DISCRIMINATING PAIR HAD TO CHANGE, AND THAT IS ITSELF THE RESULT.** This test
/// used `chrome_149` against `opera_131`, the real pair the four-bundle tree produces, and compared
/// each persona's hello against the RAW preset's. Both halves stopped working at once, for the same
/// reason and not for a bug:
///
///   * `client_for` now corrects two measured fields of every preset (ML-DSA sigalgs, ALPS
///     codepoint), so a persona's hello is deliberately NOT the raw preset's any more;
///   * and once corrected, `chrome_149` and `opera_131` emit the IDENTICAL ClientHello -- which is
///     correct, because real Opera 135's ClientHello is byte-for-byte real Chrome's. The Chromium
///     family has converged, so the tree's own pair can no longer tell two personas apart.
///
/// Asserting equality on the old pair would have made the differential vacuous -- the failure mode a
/// differential is most exposed to. So the pair moved to one that still differs on the wire,
/// `chrome_149` against `chrome_100` (`17fb6cd1bb13`), measured rather than assumed by the
/// non-vacuity guard below; and the reference is now what `client_for` PRODUCES for that preset,
/// which is what "it wore its own wire" has to mean once the engine corrects the preset at all.
/// The convergence of the shipped pair is asserted separately and on purpose, so that a bundle
/// quietly drifting off the family wire is a failure here too.
#[test]
fn two_personas_in_one_process_each_get_their_own_clienthello() {
    // The reference is the hello a persona wearing ONLY that preset produces -- same code path as
    // the engine, different persona -- so this measures "each request wore its own persona's wire"
    // rather than "the engine reproduced a library default".
    let older_ref = hello_of(&Wearer::new("chrome_100", "linux", "UA/ref-chrome_100")).ja4();
    let newer_ref = hello_of(&Wearer::new("chrome_149", "linux", "UA/ref-chrome_149")).ja4();
    // NON-VACUITY. If the two presets put the same bytes on the wire, everything below passes while
    // measuring nothing.
    assert_ne!(
        older_ref, newer_ref,
        "chrome_100 and chrome_149 must differ on the wire, or this test cannot tell them apart. \
         Every preset the four shipped bundles declare now emits ONE JA4 (the browser family has \
         converged), so a discriminating pair has to be drawn from outside them."
    );

    let older = hello_of(&Wearer::new("chrome_100", "linux", "UA/older"));
    let newer = hello_of(&Wearer::new("chrome_149", "linux", "UA/newer"));

    eprintln!("chrome_100 persona ja4={}  chrome_149 persona ja4={}", older.ja4(), newer.ja4());
    assert_eq!(
        older.ja4(),
        older_ref,
        "the chrome_100 persona's request must put chrome_100's ClientHello on the wire"
    );
    assert_eq!(
        newer.ja4(),
        newer_ref,
        "the chrome_149 persona's request must put chrome_149's ClientHello on the wire -- it went \
         out wearing another browser's handshake, which is the one thing the transport must never do"
    );
    assert_ne!(
        older.ja4(),
        newer.ja4(),
        "two personas in one process must not collapse onto one wire"
    );

    // AND THE SHIPPED PAIR, WHOSE CONVERGENCE IS THE POINT: `opera_131` is what the Opera bundle
    // declares and `chrome_149` is what the other three do, and after FP1 both put the browser
    // family's own ClientHello on the wire. Asserted here so that a bundle drifting OFF that wire
    // fails the differential as well as `the_four_presets_emit_the_browser_familys_own_ja4`.
    let opera = hello_of(&Wearer::new("opera_131", "linux", "UA/opera"));
    assert_eq!(
        opera.ja4(),
        newer.ja4(),
        "opera_131 and chrome_149 must now agree on the wire: real Opera 135's ClientHello is \
         byte-for-byte real Chrome's, and both presets are corrected to it"
    );
}

/// The other half of "one device": the request's `user-agent` and its wire are written by ONE call
/// from ONE persona, so a server that reads both cannot see two browsers.
///
/// Checked on the request rather than on the socket because the sink kills the handshake before any
/// HTTP is sent -- the wire half is measured in bytes by the test above, and this is the coupling.
#[test]
fn the_wire_and_the_user_agent_come_from_the_same_persona() {
    for (id, ua) in [("chrome_149", "UA/chrome_149"), ("opera_131", "UA/opera_131"), ("edge_148", "UA/edge_148")] {
        let p = Wearer::new(id, "linux", ua);
        let req = transport::FetchRequest::navigation("https://example-korvus.test/").worn_by(&p);
        let sent_ua = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.clone());
        assert_eq!(sent_ua.as_deref(), Some(ua), "{id}: the persona's UA must be on the request");
        assert_eq!(
            req.wire.as_ref().map(|w| w.id.as_str()),
            Some(id),
            "{id}: the same persona's wire must be on the same request"
        );
    }

    // AND A PAGE CANNOT OVERRIDE EITHER HALF. A `fetch(u, {headers: {"user-agent": ...}})` arrives
    // with the page's header already set and no wire; the persona's call strips the one and stamps
    // the other.
    let p = Wearer::new("opera_131", "linux", "UA/opera_131");
    let mut req = transport::FetchRequest::navigation("https://example-korvus.test/");
    req.headers.push(("User-Agent".into(), "UA/forged".into()));
    req.wire = Some(spec::TlsProfile { id: "chrome_149".into(), platform: "linux".into(), ..Default::default() });
    transport::apply_persona_headers(&p, &mut req);
    assert!(
        !req.headers.iter().any(|(_, v)| v == "UA/forged"),
        "the persona owns user-agent; a caller's value must not survive"
    );
    assert_eq!(
        req.wire.as_ref().map(|w| w.id.as_str()),
        Some("opera_131"),
        "the persona owns the wire too; a wire set by anything else must not survive"
    );
}

/// A request no persona claimed opens NO SOCKET. The refusal is the fail-closed half of "the wire
/// comes from the persona": without it, an un-worn request would fall back to some default wire,
/// which is precisely the guessed ClientHello this crate exists to prevent.
#[test]
fn a_request_with_no_persona_is_refused() {
    let srv = TcpListener::bind("127.0.0.1:0").expect("bind loopback sink");
    let port = srv.local_addr().expect("sink addr").port();
    srv.set_nonblocking(true).expect("nonblocking");

    let url = format!("https://127.0.0.1:{port}/");
    let err = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            // NOT `worn_by`: this is the un-worn request, which is the whole point.
            .block_on(async move { transport::fetch(transport::FetchRequest::navigation(&url)).await.err() })
    })
    .join()
    .expect("driver thread")
    .expect("a request with no persona must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("names no persona"),
        "the refusal must say why, and name the request: {msg}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        srv.accept().is_err(),
        "a request with no persona must not reach the socket at all"
    );
}
