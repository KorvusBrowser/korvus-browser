//! A frame's document is decoded in ITS encoding and reports it, as Chrome 150's is.
//!
//! The probe (`~/bench-a reference engine/obj/site/krf.html`): a UTF-8 page whose iframe is an EUC-KR `<meta charset>`
//! document. Chrome 150 reads the frame's text as `가` and `characterSet` as `EUC-KR`; korvus read `��|UTF-8`
//! (2026-10-05 and 2026-10-12), because the frame's bytes reached it as the transport's UTF-8 view and
//! `characterSet` was the page's. WPT's `encoding/legacy-mb-*` reads every character through such a frame.
//!
//! Rows: the frame declares its encoding by `<meta charset>`, by `<meta http-equiv>`, and by the `Content-Type`
//! header, in EUC-KR, Shift_JIS, Big5 and GBK. Each frame's body is its row's text, encoded. Chrome 150 read every row
//! as asserted here (2026-10-12, stock, a Python twin of this server): `가나|EUC-KR`, `日本|Shift_JIS`, `中文|Big5`,
//! `中文|GBK`, the page `UTF-8`.
use std::io::{BufRead, BufReader, Write};

/// (path, declared label in the response, WHATWG name Chrome reports, how it is declared, the frame's text)
const ROWS: &[(&str, &str, &str, &str, &str)] = &[
    ("/euckr-meta", "euc-kr", "EUC-KR", "meta", "가나"),
    ("/euckr-equiv", "euc-kr", "EUC-KR", "equiv", "가나"),
    ("/euckr-header", "euc-kr", "EUC-KR", "header", "가나"),
    ("/sjis-meta", "shift_jis", "Shift_JIS", "meta", "日本"),
    ("/big5-header", "big5", "Big5", "header", "中文"),
    ("/gbk-meta", "gbk", "GBK", "meta", "中文"),
];

fn encoded(label: &str, text: &str) -> Vec<u8> {
    let enc = encoding_rs::Encoding::for_label(label.as_bytes()).expect("a known label");
    enc.encode(text).0.into_owned()
}

/// The top page (UTF-8) frames `path`; each framed document is `TEXT` in its row's encoding.
fn serve() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (ctype, body): (String, Vec<u8>) = match ROWS.iter().find(|(p, ..)| path == *p) {
                Some((_, label, _, how, text)) => {
                    let mut doc = Vec::new();
                    match *how {
                        "meta" => doc.extend(format!("<!doctype html><meta charset=\"{label}\"><p id=s>").into_bytes()),
                        "equiv" => doc.extend(format!("<!doctype html><meta http-equiv=\"content-type\" content=\"text/html; charset={label}\"><p id=s>").into_bytes()),
                        _ => doc.extend(b"<!doctype html><p id=s>".to_vec()),
                    }
                    doc.extend(encoded(label, text));
                    let ct = if *how == "header" { format!("text/html; charset={label}") } else { "text/html".to_string() };
                    (ct, doc)
                }
                None => {
                    let frame = path.trim_start_matches("/top");
                    let html = format!("<!doctype html><meta charset=utf-8><iframe src=\"{frame}\"></iframe>");
                    ("text/html; charset=utf-8".to_string(), html.into_bytes())
                }
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = w.write_all(head.as_bytes());
            let _ = w.write_all(&body);
        }
    });
    port
}

#[tokio::test]
async fn a_frame_reads_and_reports_its_own_encoding() {
    let port = serve();
    for (path, _, name, how, text) in ROWS {
        let url = format!("http://127.0.0.1:{port}/top{path}");
        let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
        let mut e = engine::Engine::new(Box::new(p));
        e.set_url(&url);
        let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
        e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
        let _ = e.pump(std::time::Duration::from_millis(1500)).await;
        let v = e
            .eval_value("frame", "(() => { const d = document.querySelector('iframe').contentDocument; return d.getElementById('s').textContent + '|' + d.characterSet + '|' + document.characterSet; })()".to_string())
            .expect("eval");
        assert_eq!(v.as_str().unwrap_or_default(), format!("{text}|{name}|UTF-8"), "{path} ({how})");
    }
}
