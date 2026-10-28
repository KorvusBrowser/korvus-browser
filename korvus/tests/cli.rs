//! The `korvus` binary as a person runs it: `--help`, and `fetch --timeout`'s bound on the whole run.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn korvus(args: &[&str]) -> (Output, Duration) {
    let t = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_korvus")).args(args).env("KORVUS_UPDATE", "off").env("KORVUS_HOST", "ignore").output().expect("korvus runs");
    (out, t.elapsed())
}

/// `korvus serve --help` started a server on 9222 and never returned. A command's `--help` says what it takes and runs
/// nothing; so does `korvus --help`.
#[test]
fn help_says_what_a_command_takes_and_runs_nothing() {
    for args in [&["serve", "--help"][..], &["serve", "-h"], &["fetch", "--help"], &["--help"], &["help", "serve"]] {
        let (out, took) = korvus(args);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{args:?}: {:?}", out.status);
        assert!(took < Duration::from_secs(10), "{args:?} ran for {took:?}");
        assert!(text.contains("usage: korvus"), "{args:?}: {text}");
    }
    let (out, _) = korvus(&["serve", "--help"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("--port N"));
}

/// A page whose parser-blocking script never answers. `--timeout` stopped the script's run and not the load's wait for
/// it: the run ended 5 s past the deadline (`PRINT_GRACE`), exit 124, with nothing printed. Headless Chrome's
/// `--timeout` prints the page as it stands when the deadline falls due.
#[test]
fn a_load_waiting_on_the_network_ends_at_the_timeout_with_the_page_printed() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            std::thread::spawn(move || {
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let _ = r.read_line(&mut line);
                if line.contains(" /never.js ") {
                    std::thread::sleep(Duration::from_secs(120));
                    return;
                }
                let body = "<!doctype html><title>held</title><script src=/never.js></script><p>after";
                let mut w = s;
                let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let (out, took) = korvus(&["fetch", &url, "--timeout", "2", "--eval", "document.title"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(took < Duration::from_secs(5), "the run ended at {took:?}, past its 2 s timeout");
    assert_eq!(stdout.trim(), "held", "the page as it stood: {stderr}");
    // headless Chrome's `pageLoadTimedOut`: what there was is printed, and the run fails, as the deadline fell due before
    // DOMContentLoaded
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("did not reach DOMContentLoaded"), "{stderr}");
}

/// `--max-heap` is taken off the line wherever it stands, and a page runs under it; a heap of no megabytes is refused,
/// not run.
#[test]
fn a_run_under_a_heap_cap_is_a_run() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut String::new());
            let body = "<!doctype html><title>capped</title>";
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let page = &format!("http://127.0.0.1:{port}/");
    let (out, _) = korvus(&["--max-heap", "256", "fetch", page, "--eval", "document.title"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "capped", "{}", String::from_utf8_lossy(&out.stderr));
    let (out, _) = korvus(&["fetch", page, "--max-heap", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--max-heap"), "{}", String::from_utf8_lossy(&out.stderr));
}
