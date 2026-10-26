//! **THE PARTS OF HTTP/3 THAT NEED NO SOCKET** (H3, 2026-10-11): the Alt-Svc parser against QUICHE's grammar, the
//! QPACK encoder byte for byte against what stock Chrome 152 wrote (as 150 did), and the transport parameters of the `chrome_150`
//! preset against the recorded ones. `h3_wire.rs` is the end-to-end half.

const CHROME_ROWS: &str = include_str!("fixtures/h3/chrome-152.json");

/// `SpdyAltSvcWireFormat::ParseHeaderFieldValue` (`http2/core/spdy_alt_svc_wire_format.cc`).
#[test]
fn alt_svc_is_read_as_quiche_reads_it() {
    use wreq::http3::testing::alt_svc;
    assert_eq!(alt_svc("clear"), Some(vec![]));
    assert_eq!(alt_svc(""), None);
    let v = alt_svc(r#"h3=":443"; ma=86400, h3-29=":443"; ma=3600, h2="alt.example:8443""#).expect("three entries");
    assert_eq!(v.len(), 3);
    assert_eq!(v[0], ("h3".into(), "".into(), 443, 86400));
    assert_eq!(v[1].3, 3600);
    assert_eq!((v[2].1.as_str(), v[2].2), ("alt.example", 8443));
    // `ma` defaults to a day; an unknown parameter (`persist`) is skipped.
    assert_eq!(alt_svc(r#"h3=":443"; persist=1"#).expect("one entry")[0].3, 86400);
    // The port is mandatory and the authority quoted; either missing rejects the whole value.
    assert_eq!(alt_svc(r#"h3="host""#), None);
    assert_eq!(alt_svc("h3=:443"), None);
    // `v` is a quoted list for non-`hq` entries, and its commas do not end the entry.
    let v = alt_svc(r#"quic=":443"; v="46,43", h3=":443""#).expect("two entries");
    assert_eq!(v.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(), ["quic", "h3"]);
    // Percent-encoded protocol ids are decoded.
    assert_eq!(alt_svc(r#"h%33=":443""#).expect("one entry")[0].0, "h3");
}

/// Chrome 150's first request on its first QUIC connection to the recorder (aioquic: QPACK_MAX_TABLE_CAPACITY 4096),
/// as `qpack_vec.py` read it off `force-1.json` (2026-10-11): the header list, the field section on stream 0 and the
/// start of the encoder stream. QUICHE's encoder inserted all thirteen fields the static table does not hold whole,
/// and referred to them relative to a Base equal to the Required Insert Count.
#[test]
fn the_qpack_encoder_writes_chromes_first_request_byte_for_byte() {
    let fields: &[(&str, &str)] = &[
        (":method", "GET"),
        (":authority", "127.0.0.1:54167"),
        (":scheme", "https"),
        (":path", "/"),
        ("sec-ch-ua", "\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"150\", \"Google Chrome\";v=\"150\""),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"Linux\""),
        ("upgrade-insecure-requests", "1"),
        ("user-agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36"),
        (
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
        ),
        ("sec-fetch-site", "none"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-user", "?1"),
        ("sec-fetch-dest", "document"),
        ("accept-encoding", "gzip, deflate, br, zstd"),
        ("accept-language", "ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7"),
        ("priority", "u=0, i"),
    ];
    // Chrome's encoder stream for the whole connection, its type byte dropped; this request's instructions are its
    // start.
    const CHROME_ENCODER_STREAM: &str = "3fe11fc08b089d5c0b8170dc6da0b8ef674148b1275ad1ffb7fe749d3f70c176c1d527f3f7de0fe5efe7e94fe6f4f61e935b4ff3f7de0fe42d83f9fa53f9c473cd4154bd3d87a4bfcfdf783f90b60fe76b4148b1275ad1ad49e33505023f306d4148b1275ad1ad5d034ca7b29f07224c696e757822ff20ced07f66a281b0dae053fafc087ed4ce6aadf2a7979c89c6bfb521aeba0bc8b1e632586d975765c53facd8f7e8cff4a506ea5531149d4ffda97a7b0f49580b605c0b817029b8728ec330db2eaecb9fdde5497ca589d34d1f43aeba0c41a4c7a98f33a69a3fdf9a68fa1d75d0620d263d4c79a68fbed00177fe8d48e62b03ee697e8d48e62b1e0b1d7f46a4731581d754df5f2c7cfdf6800bbdf43aeba0c41a4c7a9841a6a8b22c5f249c754c5fbef046cfdf6800bbbf6a4148b4a549275906497f83a8f5176a4148b4a549275a93c85f86a87dcd30d25f6a4148b4a549275ad416cf023f316a4148b4a549275a42a13f8690e4b692d49fdf929bd9abfa5242cb40d25fa523b3e94f684c9fff099cea75b36dfaea7fbed00177fe8b52dc377df6800bbdf45abefb4005dd66aec31ec327d785b6007d286fc184610b9108dd8e497ca582211f5f2c7cfdf6800b87888840e92ac7b0d31aaf8885a8eb10f6238784427d505fcd929d29ad1718602275702e05c371b682e3ac7f8603753d30c184606bf4478384412c35698203753d31c18560cbd754dfddb1352398ac0fb9a5fa352398ac782c75fd1a91cc56075d537d1a91cc5611de6ff7e69a3e8d48e62b1f3f5f2c7cfdf6800bbd8384352398bf8385b6017d286fc1022f668b8321ec4783842d35a7d78385b600fd286fc1896251f7310f52e621ff";
    const CHROME_BLOCK: &str = "0e00d18cd7c18b8a89ff1f888786858483828180";
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let (decoded, stream, block) = wreq::http3::testing::qpack_roundtrip(fields, 4096);
    assert_eq!(hex(&block), CHROME_BLOCK, "the field section on the request stream");
    let stream = hex(&stream);
    assert!(
        !stream.is_empty() && CHROME_ENCODER_STREAM.starts_with(&stream),
        "the encoder stream must be the start of Chrome's:\nkorvus {stream}\nchrome {}",
        &CHROME_ENCODER_STREAM[..stream.len().min(CHROME_ENCODER_STREAM.len())]
    );
    let back: Vec<(&str, &str)> = decoded.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    assert_eq!(back, fields, "and it decodes back to the list");
}

/// `ValueSplittingHeaderList`: a cookie is crumbled at `;` (eating one space), any other value split at NUL.
#[test]
fn cookies_are_crumbled_as_quiche_crumbles_them() {
    let (decoded, _, _) = wreq::http3::testing::qpack_roundtrip(&[("cookie", "a=1; b=2;c=3"), ("x", "p\0q")], 0);
    let got: Vec<(&str, &str)> = decoded.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    assert_eq!(got, [("cookie", "a=1"), ("cookie", "b=2"), ("cookie", "c=3"), ("x", "p"), ("x", "q")]);
}

/// The `chrome_150` preset's transport parameters, read the way the fixture reads Chrome's.
#[test]
fn the_chrome_150_preset_sends_chromes_transport_parameters() {
    let chrome: Vec<String> = serde_json::from_str(CHROME_ROWS).expect("the Chrome rows");
    let opts = transport::quic_options_for("chrome_150").expect("the preset");
    for _ in 0..8 {
        // Several encodings: the GREASE parameter and the order are fresh each time.
        let mut korvus = wreq::http3::testing::transport_parameter_rows(&opts);
        korvus.sort();
        korvus.dedup();
        let mut want: Vec<String> = chrome.iter().filter(|r| r.starts_with("tp.")).cloned().collect();
        want.sort();
        assert_eq!(korvus, want);
    }
}
