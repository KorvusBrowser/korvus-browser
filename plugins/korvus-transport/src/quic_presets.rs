//! The HTTP/3 (QUIC) presets a persona bundle can name (`[transport] quic = "..."`, H3 2026-10-11).
//!
//! A preset is one browser build's QUIC + HTTP/3 wire, MEASURED -- the way `wreq-util`'s TLS presets are one
//! browser's ClientHello -- so the engine never asks which browser it is wearing: the bundle names the preset, and
//! this table is the measurement. Every value below was read off stock Chrome 150.0.7871.46 by the loopback recorder
//! (three runs alike, 2026-10-11) and located in Chromium 150's source; the line
//! references are `net/third_party/quiche/src/quiche/...` unless they start with `net/`.

/// The persona's HTTP/3 options for preset `name`, or `None` for a name no preset carries (the persona then speaks
/// no HTTP/3, and `check_wire` reports it).
pub fn options_for(name: &str) -> Option<wreq::http3::Http3Options> {
    match name {
        "chrome_150" => Some(chrome_150()),
        _ => None,
    }
}

fn chrome_150() -> wreq::http3::Http3Options {
    wreq::http3::Http3Options {
        // Measured: every client Initial datagram 1250 bytes (`kDefaultMaxPacketSize`, quic/core/quic_constants.h:33).
        max_packet_size: 1250,
        // Measured transport parameters (quic/core/quic_config.cc:1057-1058, net/quic/quic_context.cc:21-22,
        // quic/core/quic_session.cc:217-221, net/quic/quic_context.h:69-75).
        max_udp_payload_size: 1472,
        dcid_len: 8,
        scid_len: 0,
        max_idle_timeout_ms: 30_000,
        initial_max_data: 15_728_640,
        initial_max_stream_data: 6_291_456,
        initial_max_streams_bidi: 100,
        initial_max_streams_uni: 103,
        max_datagram_frame_size: Some(65_536),
        connection_options: vec![*b"ORIG"],
        version_information: true,
        grease_transport_parameter: true,
        shuffle_transport_parameters: true,
        // Measured: 0x3127 present on a second connection to a server, absent on the first.
        send_initial_rtt: true,
        // Measured: CRYPTO split and shuffled among PINGs and PADDING, the ClientHello's head and tail in the first
        // packet (quic/core/quic_chaos_protector.cc).
        chaos_protection: true,
        first_packet_number: 1,
        default_initial_rtt_ms: 100,
        handshake_timeout_ms: 10_000,
        // Measured ClientHello inside QUIC: TLS 1.3 only, no GREASE, no status_request / SCT, permuted extensions,
        // BoringSSL's default signature algorithms, ALPN and ALPS `h3` (quic/core/tls_client_handshaker.cc).
        tls_grease: false,
        tls_ocsp_stapling: false,
        tls_signed_cert_timestamps: false,
        tls_permute_extensions: true,
        tls_sigalgs: Some(
            "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:\
             rsa_pss_rsae_sha384:rsa_pkcs1_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha512:rsa_pkcs1_sha1"
                .to_string(),
        ),
        alps: true,
        // Measured control stream: SETTINGS 0x1=65536 0x6=262144 0x7=100 0x33=1 + GREASE, then a GREASE frame
        // (quic/core/http/quic_spdy_session.cc:618-658, quic_send_control_stream.cc:47-88).
        settings: vec![(0x01, 65_536), (0x06, 262_144), (0x07, 100), (0x33, 1)],
        grease_setting: true,
        grease_frame: true,
        qpack_encoder_max_capacity: 65_536,
        huffman: true,
        cookie_crumbling: true,
        // Measured: a PRIORITY_UPDATE per request with a non-default priority, before its HEADERS.
        priority_update: true,
        pseudo_header_order: [":method", ":authority", ":scheme", ":path"].map(String::from).to_vec(),
        // Measured live (NetLog): the dns_alpn_h3 job carries a challenge host's first request; the TCP job
        // waits 300 ms with no srtt cached (net/quic/quic_session_pool.cc:1548-1586).
        use_dns_https_alpn: true,
        main_job_default_delay_ms: 300,
        main_job_srtt_multiplier: 1.5,
        main_job_max_delay_ms: 3_000,
        force_quic_origins: Vec::new(),
        only_forced_origins: false,
    }
}
