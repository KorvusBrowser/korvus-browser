//! `korvus-analysis` -- record every page call into native code, so a malfunctioning Web API can be
//! traced back from the value that came out wrong (docs: the Layer plan in this crate's README section
//! below).
//!
//! # Layer 1 (this crate today): the recorder's writer
//!
//! An analysis build (`cargo build --release -p korvus --features analysis`) compiles korvus-webapi's
//! capture in. With `KORVUS_ANALYSIS_OUT=<dir>` set, [`AnalysisPlugin`] opens the queue and starts ONE
//! writer thread for the process; every call a page makes into a template function, a getter or setter,
//! a stub or an op2 member arrives as a [`spec::analysis::Call`] and is written to
//! `<dir>/trace-<pid>.ndjson`, one JSON object per line:
//!
//! ```text
//! {"korvus_analysis":1,"pid":…,"stack_depth":8}                         header
//! {"seq":…,"parent":…,"t":…,"thread":…,"realm":…,"kind":"call","label":"Document.get compatMode",
//!  "this":{"t":"obj","class":"HTMLDocument","id":…},"args":[…],"result":{"t":"str",…},"stack":[…]}
//! {"end":true,"events":…,"dropped":…}                                    footer, at shutdown
//! ```
//!
//! The JS thread never waits on this writer: its batches arrive through a bounded channel it only
//! `try_send`s to, and a batch the channel cannot take is dropped and counted in the footer's
//! `dropped` (`core/korvus-kernel/src/analysis.rs`). A trace whose footer says `dropped > 0` is
//! incomplete, and says so. The channel is drained by a thread that never formats (a second one does),
//! so it is full only when the box cannot copy as fast as the page records.
//!
//! `KORVUS_ANALYSIS_STACK=<n>` sets the JS stack depth each call captures (default 8, 0 for none).
//!
//! # Not yet (Layers 2 and 3)
//!
//! The call graph and value links built offline from the trace, and the two-engine comparison against
//! Chrome's `apitrace.js` trace. Neither touches the engine.
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use spec::kernel::{Context, Plugin, PluginConfig, Result};

/// Starts the writer when `KORVUS_ANALYSIS_OUT` is set; a no-op otherwise. Applying it again (another
/// engine, a worker's loader) changes nothing: one writer per process.
pub struct AnalysisPlugin;

impl Plugin for AnalysisPlugin {
    fn name(&self) -> &str {
        "analysis"
    }
    fn apply(&self, _ctx: &Context, _config: &PluginConfig) -> Result<()> {
        start_from_env();
        Ok(())
    }
}

static WRITER: OnceLock<Mutex<Option<std::thread::JoinHandle<()>>>> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Open the recorder if `KORVUS_ANALYSIS_OUT` names a directory. Returns the trace path when this call
/// started it.
pub fn start_from_env() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("KORVUS_ANALYSIS_OUT")?;
    // A number: the depth of each SITE's stack (0: no sites, no stacks); "full": a stack on every call.
    let v = std::env::var("KORVUS_ANALYSIS_STACK").unwrap_or_default();
    let (depth, full) = if v == "full" { (8, true) } else { (v.parse().unwrap_or(8usize), false) };
    start(std::path::Path::new(&dir), depth, full)
}

/// Open the recorder writing into `dir`.
pub fn start(dir: &std::path::Path, stack_depth: usize, full_stacks: bool) -> Option<std::path::PathBuf> {
    if WRITER.get().is_some() {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("trace-{}.ndjson", std::process::id()));
    let file = std::fs::File::create(&path).ok()?;
    let rx = spec::analysis::open(stack_depth, full_stacks)?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    let _ = writeln!(out, "{}", serde_json::json!({ "korvus_analysis": 2, "pid": std::process::id(), "stack_depth": stack_depth, "full_stacks": full_stacks }));
    // Two threads. The DRAIN thread only copies batches out of the channel (and hands their buffers
    // straight back to the JS threads, warm: spec::analysis::recycle), so the channel always has room and
    // nothing is dropped while a burst is formatted; the FORMAT thread decodes and serialises (~5000
    // instructions a record) and writes. A writer that did both in one loop stopped draining for the
    // seconds a 256 MB batch took to format, the channel filled behind it, and a live an interactive-widget challenge visit lost
    // 914 983 of 3.8 M records -- the challenge's whole main worker. Copied records
    // still wait for a pause (no batch for IDLE) or a full CHUNK before they are handed on: a second core
    // busy beside the JS thread lowers the JS core's clock (measured 2026-10-06: a spinning core
    // made an UNrecorded setter 20-50 ns slower), so formatting overlaps the page only in a long burst.
    let (ftx, frx) = std::sync::mpsc::channel::<Vec<u8>>();
    let format = std::thread::Builder::new()
        .name("korvus-analysis-format".into())
        .spawn(move || {
            for chunk in frx {
                let mut at = 0;
                while at + 4 <= chunk.len() {
                    let n = u32::from_le_bytes(chunk[at..at + 4].try_into().unwrap_or_default()) as usize;
                    let recs = spec::analysis::decode(&chunk[at + 4..at + 4 + n]);
                    for r in &recs {
                        if serde_json::to_writer(&mut out, r).is_ok() {
                            let _ = out.write_all(b"\n");
                        }
                    }
                    WRITTEN.fetch_add(recs.len() as u64, Ordering::Relaxed);
                    at += 4 + n;
                }
                let _ = out.flush();
            }
            let _ = writeln!(
                out,
                "{}",
                serde_json::json!({ "end": true, "events": WRITTEN.load(Ordering::Relaxed), "dropped": spec::analysis::dropped() })
            );
            let _ = out.flush();
        })
        .ok()?;
    let handle = std::thread::Builder::new()
        .name("korvus-analysis".into())
        .spawn(move || {
            const IDLE: Duration = Duration::from_millis(50);
            const CHUNK: usize = 64 << 20;
            let mut pending: Vec<u8> = Vec::new();
            let take = |(bytes, _n): (Vec<u8>, u32), pending: &mut Vec<u8>| {
                pending.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                pending.extend_from_slice(&bytes);
                spec::analysis::recycle(bytes);
            };
            loop {
                match rx.recv_timeout(IDLE) {
                    Ok(batch) => {
                        take(batch, &mut pending);
                        if pending.len() >= CHUNK {
                            let _ = ftx.send(std::mem::take(&mut pending));
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if !pending.is_empty() {
                            let _ = ftx.send(std::mem::take(&mut pending));
                        }
                        if STOP.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            while let Ok(batch) = rx.try_recv() {
                take(batch, &mut pending);
            }
            if !pending.is_empty() {
                let _ = ftx.send(pending);
            }
            // the format thread ends when this sender is gone and its queue is written
            drop(ftx);
            let _ = format.join();
        })
        .ok()?;
    let _ = WRITER.set(Mutex::new(Some(handle)));
    Some(path)
}

/// End the trace: hand the CALLING thread's partial batch over, let the writer drain the queue, write
/// the footer and close the file. Batches still held by other threads' locals arrive only if those
/// threads have ended. Safe to call when no recorder was started.
pub fn shutdown() {
    let Some(w) = WRITER.get() else { return };
    spec::analysis::flush();
    STOP.store(true, Ordering::Release);
    if let Some(h) = w.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = h.join();
    }
}
