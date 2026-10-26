//! `core/webrtc/sdp.rs` -- an offer's text: the session header and one m-section per transceiver, then the
//! application section of the data channels (a bot challenge asks
//! `createOffer({offerToReceiveAudio: 1, offerToReceiveVideo: 1})` beside a data channel, which gives Chrome
//! three m-sections -- and three transports to gather on -- where korvus wrote one).
//!
//! An audio or video section is the WORN DEVICE's, as it wrote one for a receive-only transceiver
//! (`Persona::webrtc_offer`, the bundle's `webrtc-offer.json`): which codecs, payload types, header extensions
//! and feedback lines it lists is the build's -- Chromium 151 adds RTCP XR, Opera's Linux build has no H.264.
//! What is the connection's own -- ICE credentials, the certificate's fingerprint, the mid, the direction -- is
//! written into it. A sending transceiver's section also carries its track's `msid` and SSRCs, which this
//! engine does not invent; such an offer stays refused by the caller, and so does a media section when the
//! persona recorded no offer.

/// What a connection fills into every section of its offer.
pub(crate) struct Identity<'a> {
    pub session_id: &'a str,
    pub ufrag: &'a str,
    pub pwd: &'a str,
    pub fingerprint: &'a str,
}

/// What a SENDING transceiver's section carries beyond a receive-only one: its track's `msid`
/// (stream id and track id) and its synchronization source. A receive-only section has none of this.
pub(crate) struct SendMedia<'a> {
    pub stream: &'a str,
    pub track: &'a str,
    pub ssrc: u32,
    pub cname: &'a str,
}

/// One m-section, in offer order.
pub(crate) enum Section<'a> {
    /// A transceiver's: `audio` or `video`, its direction (`recvonly`, `inactive`, `sendonly`), and --
    /// only when it is SENDING a fed media track -- the `msid`/SSRC its section must carry.
    Media { kind: &'a str, direction: &'a str, send: Option<SendMedia<'a>> },
    /// The data channels'.
    Data,
}

/// The whole offer -- `None` when it has a media section and the device recorded no offer. Mids are the
/// sections' indices, and every section is in the BUNDLE group (Chrome's `a=group:BUNDLE 0 1 2`, whatever the
/// bundle policy).
pub(crate) fn offer(id: &Identity, device: Option<&spec::WebrtcOffer>, sections: &[Section]) -> Option<String> {
    let mids: Vec<String> = (0..sections.len()).map(|i| i.to_string()).collect();
    let mut out = format!("v=0\r\no=- {} 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n", id.session_id);
    if !sections.is_empty() {
        out.push_str(&format!("a=group:BUNDLE {}\r\n", mids.join(" ")));
    }
    out.push_str("a=extmap-allow-mixed\r\na=msid-semantic: WMS\r\n");
    for (i, s) in sections.iter().enumerate() {
        let mid = i.to_string();
        match s {
            Section::Media { kind, direction, send } => {
                let d = device?;
                let t = if *kind == "audio" { &d.audio } else { &d.video };
                out.push_str(&own_section(t, id, &mid, direction, send.as_ref()));
                out.push_str("\r\n");
            }
            Section::Data => out.push_str(&format!(
                "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:{}\r\na=ice-pwd:{}\r\n\
a=ice-options:trickle\r\na=fingerprint:sha-256 {}\r\na=setup:actpass\r\na=mid:{mid}\r\na=sctp-port:5000\r\n\
a=max-message-size:262144\r\n",
                id.ufrag, id.pwd, id.fingerprint
            )),
        }
    }
    Some(out)
}

/// The device's section with this connection's own values written into it -- its ICE credentials, its
/// certificate's fingerprint, the mid and the direction -- and every other line as the device wrote it.
fn own_section(device: &str, id: &Identity, mid: &str, direction: &str, send: Option<&SendMedia>) -> String {
    let own = |l: &str| -> String {
        if l.starts_with("a=ice-ufrag:") {
            format!("a=ice-ufrag:{}", id.ufrag)
        } else if l.starts_with("a=ice-pwd:") {
            format!("a=ice-pwd:{}", id.pwd)
        } else if l.starts_with("a=fingerprint:") {
            format!("a=fingerprint:sha-256 {}", id.fingerprint)
        } else if l.starts_with("a=mid:") {
            format!("a=mid:{mid}")
        } else if matches!(l, "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive") {
            format!("a={direction}")
        } else {
            l.to_string()
        }
    };
    let mut s = device.split("\r\n").map(own).collect::<Vec<_>>().join("\r\n");
    // A sending section names its stream/track (`a=msid`) and its SSRC (Chrome's `cname` + `msid`
    // ssrc-attribute lines). A receive-only section has neither, so its text is unchanged.
    if let Some(m) = send {
        s.push_str(&format!(
            "\r\na=msid:{stream} {track}\r\na=ssrc:{ssrc} cname:{cname}\r\na=ssrc:{ssrc} msid:{stream} {track}",
            stream = m.stream,
            track = m.track,
            ssrc = m.ssrc,
            cname = m.cname,
        ));
    }
    s
}

/// The DTLS transport an answer describes, for the send path's handshake: `(setup, fingerprint, address,
/// port)` from the answer's `a=setup`, `a=fingerprint` and either a host `a=candidate` or the `c=`/`m=`
/// default. `None` when any of the three is missing (so the handshake is not attempted).
pub(crate) fn parse_answer_transport(sdp: &str) -> Option<(String, String, String, u16)> {
    let setup = sdp.lines().find_map(|l| l.strip_prefix("a=setup:")).map(|v| v.trim().to_string())?;
    let fingerprint = sdp.lines().find_map(|l| l.strip_prefix("a=fingerprint:")).map(|v| v.trim().to_string())?;
    let (addr, port) = answer_address(sdp)?;
    Some((setup, fingerprint, addr, port))
}

/// The address the send path aims its DTLS at: a host `a=candidate` if the answer trickled one, else the
/// section's `c=` address with the `m=` port.
fn answer_address(sdp: &str) -> Option<(String, u16)> {
    for l in sdp.lines().filter_map(|l| l.strip_prefix("a=candidate:")) {
        let t: Vec<&str> = l.split(' ').collect();
        if t.len() >= 6 && t[1] == "1" && t[2].eq_ignore_ascii_case("udp") {
            let typ = t.iter().position(|x| *x == "typ").and_then(|i| t.get(i + 1)).copied().unwrap_or("");
            if typ == "host" {
                if let Ok(port) = t[5].parse::<u16>() {
                    return Some((t[4].to_string(), port));
                }
            }
        }
    }
    let addr = sdp.lines().find_map(|l| l.strip_prefix("c=IN IP4 ").or_else(|| l.strip_prefix("c=IN IP6 ")))?.trim().to_string();
    let port = sdp.lines().find_map(|l| l.strip_prefix("m=")).and_then(|m| m.split(' ').nth(1)).and_then(|p| p.parse::<u16>().ok())?;
    Some((addr, port))
}

/// The m-section a candidate line belongs in, by mid; `None` past the last.
fn section_bounds(sdp: &str, mid: usize) -> Option<(usize, usize)> {
    let starts: Vec<usize> = sdp.match_indices("\r\nm=").map(|(i, _)| i + 2).collect();
    let start = *starts.get(mid)?;
    let end = starts.get(mid + 1).copied().unwrap_or(sdp.len());
    Some((start, end))
}

/// A gathered candidate written into the local description, as Chrome does it: the `a=candidate` line (the SDP
/// form carries no `ufrag`) after the section's other candidates -- the first right after `a=rtcp:`, or `c=` in
/// the data section -- and then the section's DEFAULT address recomputed from all its candidates.
pub(crate) fn add_candidate(sdp: &str, mid: usize, sdp_line: &str) -> String {
    let Some((start, end)) = section_bounds(sdp, mid) else { return sdp.to_string() };
    let section = &sdp[start..end];
    let mut lines: Vec<String> = section.split("\r\n").map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let at = lines
        .iter()
        .rposition(|l| l.starts_with("a=candidate:"))
        .or_else(|| lines.iter().position(|l| l.starts_with("a=rtcp:")))
        .or_else(|| lines.iter().position(|l| l.starts_with("c=")))
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines.insert(at, format!("a={sdp_line}"));
    let (addr, port) = default_address(&lines);
    if let Some(m) = lines.first_mut() {
        let mut parts: Vec<&str> = m.splitn(3, ' ').collect();
        if parts.len() == 3 {
            let p = port.to_string();
            parts[1] = &p;
            *m = parts.join(" ");
        }
    }
    if let Some(c) = lines.iter_mut().find(|l| l.starts_with("c=")) {
        let fam = if addr.contains(':') { "IP6" } else { "IP4" };
        *c = format!("c=IN {fam} {addr}");
    }
    let mut new_section = lines.join("\r\n");
    new_section.push_str("\r\n");
    format!("{}{}{}", &sdp[..start], new_section, &sdp[end..])
}

/// libwebrtc's `UpdateConnectionAddress` (`pc/jsep_session_description.cc`): over the section's RTP-component
/// UDP candidates IN ORDER, a candidate replaces the current one unless it is no more preferred (host 4,
/// srflx and prflx 2, relay 1) within the same address family, or it is IPv6 while the current is IPv4. A
/// hostname (an mDNS host candidate) as the result is the dummy `0.0.0.0` port 9 -- so a srflx candidate that
/// arrived BEFORE its section's host leaves the default at the dummy, and one that arrived after it becomes
/// the default (Chrome 150, both orders measured).
fn default_address(lines: &[String]) -> (String, u16) {
    let (mut pref, mut family, mut ip, mut port, mut hostname) = (0, 0u8, String::from("0.0.0.0"), 9u16, false);
    for l in lines.iter().filter_map(|l| l.strip_prefix("a=candidate:")) {
        let t: Vec<&str> = l.split(' ').collect();
        if t.len() < 8 || t[1] != "1" || !t[2].eq_ignore_ascii_case("udp") {
            continue;
        }
        let ty = t.iter().position(|x| *x == "typ").and_then(|i| t.get(i + 1)).copied().unwrap_or("");
        let p = match ty {
            "host" => 4,
            "srflx" | "prflx" => 2,
            "relay" => 1,
            _ => 0,
        };
        let fam = match t[4].parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(_)) => 4,
            Ok(std::net::IpAddr::V6(_)) => 6,
            Err(_) => 0,
        };
        if (p <= pref && family == fam) || (family == 4 && fam == 6) {
            continue;
        }
        pref = p;
        family = fam;
        hostname = fam == 0;
        ip = t[4].to_string();
        port = t[5].parse().unwrap_or(9);
    }
    if hostname { ("0.0.0.0".to_string(), 9) } else { (ip, port) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device's two sections, cut down: the lines the connection owns and two it does not.
    fn device() -> spec::WebrtcOffer {
        let s = |m: &str, codec: &str| {
            format!(
                "{m}\r\nc=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=ice-ufrag:dev1\r\na=ice-pwd:devpwd\r\n\
a=ice-options:trickle\r\na=fingerprint:sha-256 DE:VI:CE\r\na=setup:actpass\r\na=mid:7\r\na=recvonly\r\na=rtcp-mux\r\n{codec}"
            )
        };
        spec::WebrtcOffer {
            audio: s("m=audio 9 UDP/TLS/RTP/SAVPF 111", "a=rtpmap:111 opus/48000/2"),
            video: s("m=video 9 UDP/TLS/RTP/SAVPF 96", "a=rtpmap:96 VP8/90000"),
        }
    }

    #[test]
    fn three_sections_bundle_as_chromes_do() {
        let id = Identity { session_id: "1", ufrag: "Tu3T", pwd: "p", fingerprint: "AA" };
        let d = device();
        let o = offer(&id, Some(&d), &[Section::Media { kind: "audio", direction: "recvonly", send: None }, Section::Media { kind: "video", direction: "inactive", send: None }, Section::Data]).unwrap();
        assert!(o.contains("a=group:BUNDLE 0 1 2\r\n"));
        let m: Vec<&str> = o.lines().filter(|l| l.starts_with("m=")).collect();
        assert_eq!(m, ["m=audio 9 UDP/TLS/RTP/SAVPF 111", "m=video 9 UDP/TLS/RTP/SAVPF 96", "m=application 9 UDP/DTLS/SCTP webrtc-datachannel"]);
        // the device's lines, the connection's values
        let video = &o[o.find("m=video").unwrap()..o.find("m=application").unwrap()];
        assert_eq!(
            video,
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=ice-ufrag:Tu3T\r\na=ice-pwd:p\r\n\
a=ice-options:trickle\r\na=fingerprint:sha-256 AA\r\na=setup:actpass\r\na=mid:1\r\na=inactive\r\na=rtcp-mux\r\na=rtpmap:96 VP8/90000\r\n"
        );
        assert!(o.contains("a=mid:0\r\na=recvonly\r\n") && !o.contains("dev") && !o.contains("DE:VI"));
    }

    #[test]
    fn a_media_section_needs_the_devices_offer() {
        let id = Identity { session_id: "1", ufrag: "U", pwd: "p", fingerprint: "AA" };
        assert_eq!(offer(&id, None, &[Section::Media { kind: "audio", direction: "recvonly", send: None }, Section::Data]), None);
        assert!(offer(&id, None, &[Section::Data]).is_some_and(|o| o.contains("m=application ")));
        assert!(offer(&id, None, &[]).is_some_and(|o| !o.contains("m=") && !o.contains("BUNDLE")));
    }

    #[test]
    fn a_sending_section_carries_its_msid_and_ssrc() {
        let id = Identity { session_id: "1", ufrag: "U", pwd: "p", fingerprint: "AA" };
        let d = device();
        let send = SendMedia { stream: "s0", track: "t0", ssrc: 12345, cname: "cn" };
        let o = offer(&id, Some(&d), &[Section::Media { kind: "video", direction: "sendonly", send: Some(send) }]).unwrap();
        // the sent direction, then the stream/track and SSRC lines a receive-only section never has
        assert!(o.contains("a=sendonly\r\n") && !o.contains("a=recvonly"));
        assert!(o.contains("a=msid:s0 t0\r\n"));
        assert!(o.contains("a=ssrc:12345 cname:cn\r\n"));
        assert!(o.contains("a=ssrc:12345 msid:s0 t0\r\n"));
        // and it is still the device's own codec line, with the connection's own fingerprint
        assert!(o.contains("a=rtpmap:96 VP8/90000") && o.contains("a=fingerprint:sha-256 AA"));
    }

    #[test]
    fn an_answers_transport_is_parsed_for_the_handshake() {
        let answer = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 50000 UDP/TLS/RTP/SAVPF 96\r\n\
c=IN IP4 127.0.0.1\r\na=setup:passive\r\na=fingerprint:sha-256 BE:EF\r\n\
a=candidate:1 1 udp 2113937151 127.0.0.1 50000 typ host\r\na=mid:0\r\n";
        assert_eq!(
            parse_answer_transport(answer),
            Some(("passive".to_string(), "sha-256 BE:EF".to_string(), "127.0.0.1".to_string(), 50000))
        );
        // no fingerprint -> no handshake
        assert_eq!(parse_answer_transport("v=0\r\nm=video 9 x\r\nc=IN IP4 1.2.3.4\r\na=setup:active\r\n"), None);
    }

    #[test]
    fn the_default_address_is_the_srflx_only_after_the_host() {
        let id = Identity { session_id: "1", ufrag: "U", pwd: "p", fingerprint: "AA" };
        let host = "candidate:1 1 udp 2113937151 x.local 4000 typ host generation 0 network-cost 999";
        let srflx = "candidate:2 1 udp 1677729535 203.0.113.9 4000 typ srflx raddr 0.0.0.0 rport 0 generation 0 network-cost 999";
        let o = offer(&id, None, &[Section::Data]).unwrap();
        let after = add_candidate(&add_candidate(&o, 0, host), 0, srflx);
        assert!(after.contains("m=application 4000 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 203.0.113.9\r\na=candidate:1 "));
        assert!(after.find("a=candidate:1").unwrap() < after.find("a=candidate:2").unwrap());
        let before = add_candidate(&add_candidate(&o, 0, srflx), 0, host);
        assert!(before.contains("m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=candidate:2 "));
    }
}
