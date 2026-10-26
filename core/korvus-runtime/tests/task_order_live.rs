//! LIVE gate: the event loop runs tasks in the order they became READY, whatever their source -- Chromium's sequence
//! manager.
//!
//! The probe is a React-scheduler-shaped `MessageChannel` loop of 5 ms tasks with two timers and three network reads
//! started beside it; a server on its own thread stamps the moment it sends each response (epoch ms). Every callback
//! reports how many of the loop's tasks STARTED after its timer's deadline, or after its response was sent, and before
//! it ran -- an order, not a duration, so a loaded box moves it little. Chrome 152.0.7977.82 headless, this machine,
//! 2026-10-23, three runs alike (`scratchpad/r3/p1/www/starve3.html`):
//!
//! | callback                                   | Chrome 152 | this engine before |
//! |--------------------------------------------|------------|--------------------|
//! | `setTimeout(50)`, `setTimeout(200)`        | 1, 1       | 4-12, 6-9          |
//! | `fetch()` headers (100 ms, 30 ms)          | 0-1        | 4-6                |
//! | XMLHttpRequest `load` (150 ms)             | 0-1        | 7-11               |
//!
//! A due timer is moved to the work queue when the next task is selected, behind the message the running task posted
//! (one more task); a response is posted to the main thread when it arrives, ahead of the message the running task
//! posts at its end (no more task). This engine ran a turn of up to three generations of spawned tasks before it
//! resolved any op that completed, saw an op complete only once tokio had a turn between two of its own, and reached a
//! timer's callback through two more hops (`deno_core`'s `dispatch_task_spawner`, KORVUS PATCH 2026-10-23).
//!
//! The gates after it pin the rest of that order, each with its own Chrome 152 measurement.
use engine::Engine;
use std::io::{Read, Write};
use std::time::Duration;

/// `/slow?ms=N[&...]`: answers after N ms (a fraction allowed) with `{"sent": <epoch ms when sent>}`; `/px.gif`: a 1x1
/// GIF; `/bar?ms=N&n=K&k=KEY`: after N ms, waits for K requests of the same KEY and all answer at once, as `/slow`;
/// `/split?ms=N&body=B`: the head after N ms, the 64-byte body B ms later, `{"h": <head sent>, "sent": <body sent>}`;
/// `/img`, `/css`, `/js?name=X` after `ms`: a GIF, an empty rule, `__js('X', <sent>)`; anything else: the page.
fn serve(listener: std::net::TcpListener, page: String) {
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x01D\x00;";
    static BARRIERS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Barrier>>>> = std::sync::OnceLock::new();
    let epoch_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64() * 1000.0;
    for s in listener.incoming() {
        let Ok(mut s) = s else { return };
        let page = page.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf) else { return };
            let _ = s.set_nodelay(true);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
            let arg = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).map(str::to_string);
            let ms = |k: &str, d: f64| arg(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
            let (body, ty): (Vec<u8>, &str) = match path {
                "/slow" | "/bar" => {
                    std::thread::sleep(Duration::from_secs_f64(ms("ms", 100.0) / 1000.0));
                    if path == "/bar" {
                        let b = BARRIERS.get_or_init(Default::default).lock().unwrap().entry(arg("k").unwrap_or_default()).or_insert_with(|| std::sync::Arc::new(std::sync::Barrier::new(ms("n", 2.0) as usize))).clone();
                        b.wait();
                    }
                    (format!("{{\"sent\":{:.3}}}", epoch_ms()).into_bytes(), "application/json")
                }
                "/split" => {
                    std::thread::sleep(Duration::from_secs_f64(ms("ms", 20.0) / 1000.0));
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: 64\r\n\r\n";
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.flush();
                    let h = epoch_ms();
                    std::thread::sleep(Duration::from_secs_f64(ms("body", 20.0) / 1000.0));
                    let body = format!("{{\"h\":{h:.3},\"sent\":{:.3}}}", epoch_ms());
                    let _ = s.write_all(format!("{body:<64}").as_bytes());
                    let _ = s.flush();
                    return;
                }
                "/img" | "/css" | "/js" => {
                    std::thread::sleep(Duration::from_secs_f64(ms("ms", 0.0) / 1000.0));
                    match path {
                        "/img" => (GIF.to_vec(), "image/gif"),
                        "/css" => (b"body{}".to_vec(), "text/css"),
                        _ => (format!("window.__js && __js('{}', {:.3});", arg("name").unwrap_or_default(), epoch_ms()).into_bytes(), "text/javascript"),
                    }
                }
                "/px.gif" => (GIF.to_vec(), "image/gif"),
                _ => (page.into_bytes(), "text/html"),
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
            let _ = s.flush();
        });
    }
}

const PAGE: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((done) => {
  const T0 = performance.now(), starts = [], ev = [];
  const now = () => performance.now() - T0;
  const page = (epoch) => epoch - performance.timeOrigin - T0;
  const ch = new MessageChannel();
  const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
  const after = (ready, at) => starts.filter((s) => s >= ready && s < at).length;
  const deb = /deb=1/.test(location.search); let dt = 0;
  ch.port1.onmessage = () => {
    // (?deb=1: every task re-arms a debounce timer, as React's scheduler and lodash.debounce do)
    if (deb) { clearTimeout(dt); dt = setTimeout(() => {}, 1000); }
    starts.push(now()); spin(5);
    if (now() < 700) { ch.port2.postMessage(0); return; }
    clearTimeout(dt);
    setTimeout(() => { const r = {}; for (const [n, ready, at] of ev) r[n] = after(ready, at); done(JSON.stringify(r)); }, 100);
  };
  let fh;
  fetch('/slow?ms=100').then((r) => { fh = now(); return r.json(); }).then((j) => ev.push(['fetch100', page(j.sent), fh]));
  const x = new XMLHttpRequest(); x.open('GET', '/slow?ms=150');
  x.onload = () => ev.push(['xhr150', page(JSON.parse(x.responseText).sent), now()]); x.send();
  setTimeout(() => fetch('/slow?ms=30').then((r) => { const h = now(); return r.json().then((j) => ev.push(['fetch30', page(j.sent), h])); }), 300);
  const tt = now();
  setTimeout(() => ev.push(['t50', tt + 50, now()]), 50);
  setTimeout(() => ev.push(['t200', tt + 200, now()]), 200);
  ch.port2.postMessage(0);
});
</script></body></html>"#;

/// Load `page` from a server of its own (as `/page.html` + `query`) and answer what its `window.R` settles with.
async fn probe_page(page: &str, query: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let served = page.to_string();
    std::thread::spawn(move || serve(listener, served));
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(page, &format!("{origin}/page.html{query}"), &[]).await.expect("load_page");
    let got = e.eval_value_settled("order", "window.R".to_string(), Duration::from_secs(20)).await.expect("eval");
    got.as_str().unwrap_or("null").to_string()
}

async fn probe(query: &str) -> serde_json::Value {
    serde_json::from_str(&probe_page(PAGE, query).await).expect("json")
}

/// Up to three runs, the first that keeps every bound: a response the transport's thread is descheduled on comes in a
/// task late through no fault of the loop's. The old loop fails all three -- its lateness is its own: 4 tasks and more
/// for every one of them.
#[tokio::test]
async fn timers_and_responses_run_in_the_order_they_became_ready() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = probe("").await;
        let n = |k: &str| got[k].as_i64().unwrap_or(-1);
        let ok = n("t50") == 1 && n("t200") == 1 && (0..=1).contains(&n("fetch100")) && (0..=1).contains(&n("fetch30")) && (0..=1).contains(&n("xhr150"));
        runs.push(got);
        if ok {
            return;
        }
    }
    let got = runs.last().cloned().unwrap_or_default();
    let n = |k: &str| got[k].as_i64().unwrap_or(-1);
    assert!(n("t50") == 1 && n("t200") == 1, "a due timer runs after the task running at its deadline and ONE more (Chrome 152): {runs:?}");
    assert!((0..=1).contains(&n("fetch100")) && (0..=1).contains(&n("fetch30")), "a fetch() response runs right after the task it came in during (Chrome 152): {runs:?}");
    assert!((0..=1).contains(&n("xhr150")), "an XMLHttpRequest's load runs right after the task its response came in during (Chrome 152): {runs:?}");
}

/// The same, with every task of the loop re-arming a debounce timer (`clearTimeout` + `setTimeout(f, 1000)`, as React's
/// scheduler and lodash.debounce do). The arm wakes the parked timer pump from INSIDE the task, and that wake is no
/// arrival: Chrome 152 still runs `setTimeout(50)` and `(200)` after ONE more task (2026-10-23, three runs, P10). The loop
/// took the arm's wake for the timer's, and ran them after NONE -- ahead of the message the task had posted.
#[tokio::test]
async fn a_debounce_timer_rearmed_by_every_task_moves_no_other_timer() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = probe("?deb=1").await;
        let ok = got["t50"].as_i64() == Some(1) && got["t200"].as_i64() == Some(1);
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("a due timer runs after the task running at its deadline and ONE more, whatever the task armed (Chrome 152): {runs:?}");
}

/// A wake the RUNNING TASK causes is no arrival. A task arms a timer (waking the timer pump, parked on a far one) and
/// posts a message `m`, then runs 60 ms while a `fetch()` response comes in: Chrome 152 runs `m` before the response
/// `f`. And a task that arms a 1 ms timer, runs 5 ms and posts `n` runs `n` before the timer `t`. 2026-10-23, Chrome
/// 152.0.7977.82, this machine, every trial of three runs (P1c, P1): `m0 f0 ... n0 t0 ...`. The loop marked an arrival
/// at the first wake of ANY op since its last look, and the arm was one: `f0 m0 ... t0 n0 ...`.
const SAME_THREAD: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const log = [];
const ch = new MessageChannel(); ch.port1.onmessage = (e) => log.push(e.data);
const far = setTimeout(() => {}, 5000);
let trials = 0;
// a response that comes in during a task, after the task armed a timer (waking the parked timer pump) and posted m
function trial() {
  const i = trials, ch2 = new MessageChannel();
  ch2.port1.onmessage = () => { setTimeout(() => {}, 1000); ch.port2.postMessage('m' + i); spin(60); };
  fetch('/slow?ms=20&r=' + Math.random()).then(() => { log.push('f' + i); if (++trials < 4) setTimeout(trial, 30); else setTimeout(part2, 30); });
  ch2.port2.postMessage(0);
}
// a 1 ms timer armed inside a task that then runs 5 ms and posts a message at its end
let k = 0;
const ch3 = new MessageChannel();
ch3.port1.onmessage = () => { const i = k; setTimeout(() => log.push('t' + i), 1); spin(5); ch.port2.postMessage('n' + i); };
function part2() {
  if (k >= 4) { clearTimeout(far); setTimeout(() => finish(log.join(' ')), 30); return; }
  fetch('/slow?ms=5&r=' + Math.random()).then(() => { ch3.port2.postMessage(0); setTimeout(() => { k++; part2(); }, 30); });
}
setTimeout(trial, 20);
</script></body></html>"#;
#[tokio::test]
async fn a_wake_the_running_task_causes_is_no_arrival() {
    let want = "m0 f0 m1 f1 m2 f2 m3 f3 n0 t0 n1 t1 n2 t2 n3 t3";
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = probe_page(SAME_THREAD, "").await;
        if got == want {
            return;
        }
        runs.push(got);
    }
    panic!("a response and a timer run where they came in, not where the task's own arm woke the loop (Chrome 152: {want}): {runs:?}");
}

/// EACH op where IT came in, each with its own microtask checkpoint, and a due timer at the next selection. A loop of
/// 30 ms `MessageChannel` tasks; in one of them a response A comes in 5 ms in, the task posts a `setTimeout(0)` T and a
/// `window.postMessage` W 15 ms in, a timer D comes due 20 ms in, a response B comes in 25 ms in, and the task posts
/// the next one, N, at its end. Chrome 152 (2026-10-23, 12 of 12 rounds, two runs): `A Amt T W B Bmt N D`. The loop
/// queued every op that completed during a task at the first wake of any of them and resolved them in one call:
/// `A B Amt Bmt D T W N`. Rounds whose responses the server sent too near a boundary to say are not counted.
const ARRIVALS: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const R = [], now = () => performance.now(), ep = (e) => e - performance.timeOrigin;
const ch = new MessageChannel(); let slice = 0, round = null, rounds = 0;
addEventListener('message', (e) => { if (e.data === 'W' && round) round.seq.push('W'); });
ch.port1.onmessage = () => {
  slice++; const s0 = now();
  if (round && slice === round.k + 2) round.seq.push('N');
  if (slice % 5 === 1 && rounds < 6) {
    rounds++; const r = round = { k: slice, seq: [], sent: {} };
    const go = (n, ms) => fetch('/slow?ms=' + ms + '&r=' + Math.random()).then((x) => { r.seq.push(n); queueMicrotask(() => r.seq.push(n + 'mt')); return x.json(); }).then((j) => { r.sent[n] = ep(j.sent); });
    go('A', 35); go('B', 55);
    r.dueD = s0 + 50; setTimeout(() => r.seq.push('D'), r.dueD - now());
  }
  if (round && slice === round.k + 1) { round.s1 = s0; spin(15); round.post = now(); setTimeout(() => round.seq.push('T'), 0); postMessage('W', '*'); spin(15); round.end = now(); }
  else spin(30);
  if (round && slice === round.k + 3) R.push({ seq: round.seq.join(' '), A: +(round.sent.A - round.s1).toFixed(1), B: +(round.sent.B - round.s1).toFixed(1), post: +(round.post - round.s1).toFixed(1), D: +(round.dueD - round.s1).toFixed(1), end: +(round.end - round.s1).toFixed(1) });
  if (slice < 33) ch.port2.postMessage(0); else setTimeout(() => finish(JSON.stringify(R)), 100);
};
ch.port2.postMessage(0);
</script></body></html>"#;
#[tokio::test]
async fn each_response_and_timer_runs_where_it_came_in() {
    let want = "A Amt T W B Bmt N D";
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(ARRIVALS, "").await).expect("json");
        let rounds: Vec<&serde_json::Value> = got.as_array().map(|a| a.iter().collect()).unwrap_or_default();
        let f = |r: &serde_json::Value, k: &str| r[k].as_f64().unwrap_or(f64::NAN);
        // A well before the posts, B well between them and the task's end, D between the posts and B
        let clear: Vec<&&serde_json::Value> = rounds.iter().filter(|r| f(r, "A") < f(r, "post") - 3.0 && f(r, "B") > f(r, "D") + 1.0 && f(r, "B") < f(r, "end") - 2.0).collect();
        let ok = clear.len() >= 3 && clear.iter().all(|r| r["seq"].as_str() == Some(want));
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("each arrival runs where it came in, with its own checkpoint (Chrome 152: {want}): {runs:?}");
}

/// AN IDLE PERIOD BEGINS ONLY WHEN NO TASK IS READY. `requestIdleCallback`, then a `setTimeout(0)` that posts `m2` and a
/// message `m1` whose handler posts `m3`: Chrome 152 runs `t0 m1 m2 m3 ric`. Beside a loop of 5 ms `MessageChannel` tasks
/// for 300 ms, an idle callback runs after the loop's LAST task, and one with `{timeout: 150}` at its timeout with
/// `didTimeout` true. 2026-10-23, three runs alike (P2, P2b, ric2). The pump began idle periods whenever the timer
/// queue had nothing due: `t0 m1 ric m2 m3`, the idle callback after 1 task of the loop, the timeout one at 11 ms.
const IDLE: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const r = {}, log = [];
const ch = new MessageChannel(); ch.port1.onmessage = (e) => { log.push(e.data); if (e.data === 'm1') ch.port2.postMessage('m3'); };
setTimeout(() => {
  requestIdleCallback(() => log.push('ric'));
  setTimeout(() => { log.push('t0'); ch.port2.postMessage('m2'); }, 0);
  ch.port2.postMessage('m1');
}, 20);
setTimeout(() => {
  r.order = log.join(' ');
  const T0 = performance.now(); let n = 0; const c = new MessageChannel();
  c.port1.onmessage = () => { n++; spin(5); if (performance.now() - T0 < 300) c.port2.postMessage(0); else r.loop = n; };
  requestIdleCallback(() => { r.ric = n; });
  requestIdleCallback((d) => { r.to = Math.round(performance.now() - T0); r.toBusy = r.loop === undefined; r.didTimeout = d.didTimeout; }, { timeout: 150 });
  c.port2.postMessage(0);
  setTimeout(() => finish(JSON.stringify(r)), 500);
}, 200);
</script></body></html>"#;
#[tokio::test]
async fn an_idle_period_begins_only_when_no_task_is_ready() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(IDLE, "").await).expect("json");
        let ok = got["order"].as_str() == Some("t0 m1 m2 m3 ric")
            && got["ric"].as_i64().is_some()
            && got["ric"] == got["loop"]
            && got["didTimeout"].as_bool() == Some(true)
            && got["toBusy"].as_bool() == Some(true)
            && got["to"].as_i64().is_some_and(|t| t >= 150);
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("an idle callback runs when nothing else is ready, or at its timeout with didTimeout (Chrome 152): {runs:?}");
}

/// TIMERS DUE TOGETHER RUN BACK TO BACK. Three `setTimeout(10)`s, the first posting a port message, a window message and
/// a `setTimeout(0)`: Chrome 152 runs `A amt B C M W T0` (5 of 5). Four `setTimeout(50)`s and four timers 1 ms apart
/// beside a loop of 5 ms tasks run after 1 more task each (1-2 for the last; 2026-10-23, four runs). Chromium's sequence
/// manager moves every due delayed task to its work queue at one selection; the pump moved one per selection, so each
/// further timer waited behind what the one before it posted: `A amt M W T0 B C`, and 1, 2, 3, 4.
const TOGETHER: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const r = {}, L = [];
const ch = new MessageChannel(); ch.port1.onmessage = (e) => L.push(e.data);
addEventListener('message', (e) => { if (e.data === 'W') L.push('W'); });
setTimeout(() => { L.push('A'); ch.port2.postMessage('M'); postMessage('W', '*'); setTimeout(() => L.push('T0'), 0); queueMicrotask(() => L.push('amt')); }, 10);
setTimeout(() => L.push('B'), 10); setTimeout(() => L.push('C'), 10);
setTimeout(() => {
  r.same = L.join(' ');
  const T0 = performance.now(), starts = []; const c = new MessageChannel();
  c.port1.onmessage = () => { starts.push(performance.now()); spin(5); if (performance.now() - T0 < 250) c.port2.postMessage(0); };
  const a = performance.now();
  for (const k of ['a', 'b', 'c', 'd']) setTimeout(() => { r['eq50' + k] = starts.filter((s) => s >= a + 50 && s < performance.now()).length; }, 50);
  [150, 151, 152, 153].forEach((ms) => setTimeout(() => { r['near' + ms] = starts.filter((s) => s >= a + ms && s < performance.now()).length; }, ms));
  c.port2.postMessage(0);
  setTimeout(() => finish(JSON.stringify(r)), 400);
}, 100);
</script></body></html>"#;
#[tokio::test]
async fn timers_due_together_run_back_to_back() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(TOGETHER, "").await).expect("json");
        let after = ["eq50a", "eq50b", "eq50c", "eq50d", "near150", "near151", "near152", "near153"];
        let ok = got["same"].as_str() == Some("A amt B C M W T0") && after.iter().all(|k| got[*k].as_i64().is_some_and(|n| (1..=2).contains(&n)));
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("timers due at one selection run back to back, ahead of what the first posts (Chrome 152): {runs:?}");
}

/// A STEP CHAINED ON A SETTLED PROMISE IS A NEW TASK, not the continuation of the arrival being resolved. A `fetch()`
/// response comes in during a 60 ms task that posts `m` at its end, and its reaction starts an `<img>` of an image already
/// loaded: Chrome 152 runs `f m img` (5 of 5 trials, three runs, 2026-10-23, P5). The kept image's load step was queued
/// at the front, as the response's own: `f img m`.
const SETTLED: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const log = [];
const ch = new MessageChannel(); ch.port1.onmessage = (e) => log.push(e.data);
let trials = 0;
function trial() {
  const i = trials, ch2 = new MessageChannel();
  ch2.port1.onmessage = () => { spin(60); ch.port2.postMessage('m' + i); };
  fetch('/slow?ms=20&r=' + Math.random()).then(() => {
    log.push('f' + i);
    const im = new Image(); im.onload = () => { log.push('img' + i); if (++trials < 4) setTimeout(trial, 30); else finish(log.join(' ')); }; im.src = '/px.gif';
  });
  ch2.port2.postMessage(0);
}
const im0 = new Image(); im0.onload = () => setTimeout(trial, 20); im0.src = '/px.gif';
</script></body></html>"#;
#[tokio::test]
async fn a_step_chained_on_a_settled_promise_is_a_new_task() {
    let want = "f0 m0 img0 f1 m1 img1 f2 m2 img2 f3 m3 img3";
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got = probe_page(SETTLED, "").await;
        if got == want {
            return;
        }
        runs.push(got);
    }
    panic!("a kept image's load is a task of its own, behind what was queued before it (Chrome 152: {want}): {runs:?}");
}

/// A WORKER'S TIMERS, as the page's: beside a ping-pong of 0.3 ms `MessageChannel` tasks in a worker, eight timers
/// (40, 80 ... 320 ms) each run after the task running at its deadline and one more, now and then two -- Chrome 152:
/// 2 1 1 1 2 1 1 1, 1 2 2 1 2 2 1 1, 1 1 1 1 1 1 2 1 (2026-10-23, mean 1.1-1.5, never 3). The worker's loop had no
/// task-source hook, so a timer that came due inside a 1 ms turn of short tasks waited for the turn's end: mean 2.0-2.8,
/// 3 or 4 in every run (four runs).
const WORKER: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const body = `
  const spin = (ms) => { const e = performance.now() + ms; while (performance.now() < e); };
  const T0 = performance.now(), starts = [], r = {};
  const ch = new MessageChannel();
  ch.port1.onmessage = () => { starts.push(performance.now()); spin(0.3); if (performance.now() - T0 < 380) ch.port2.postMessage(0); else { r.n = starts.length; postMessage(r); } };
  for (let d = 40; d <= 320; d += 40) { const at = performance.now() + d; setTimeout(() => { r['t' + d] = starts.filter((s) => s >= at && s < performance.now()).length; }, d); }
  ch.port2.postMessage(0);`;
const w = new Worker(URL.createObjectURL(new Blob([body], { type: 'text/javascript' })));
// (the page's own loop waits for the worker on a timer of its own)
const keep = setTimeout(() => {}, 15000);
w.onmessage = (e) => { clearTimeout(keep); finish(JSON.stringify(e.data)); };
</script></body></html>"#;
#[tokio::test]
async fn a_worker_timer_runs_after_the_task_at_its_deadline_and_one_more() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(WORKER, "").await).expect("json");
        let after: Vec<i64> = (1..=8).map(|i| got[format!("t{}", 40 * i)].as_i64().unwrap_or(-1)).collect();
        let ok = after.iter().all(|n| (1..=2).contains(n)) && after.iter().sum::<i64>() <= 13;
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("a worker's due timer runs after the task at its deadline and one more (Chrome 152): {runs:?}");
}

/// RESPONSES KEEP THEIR ORDER. Three `fetch()`es with the page idle: `a` sent at 30 ms, `b` 0.3 ms after it, `c` at 32
/// ms; the first reaction posts a `MessageChannel` message and spins 4 ms. Chrome 152 runs the three responses in the
/// order the server sent them in every round (`aMbc`, 2026-10-23, 20 of 20); an arrival held while the loop was idle and
/// released one at a time let `c`, which came in during the spin, run ahead of `b` -- `aMcb` in 18 of 20.
const ARRIVAL_ORDER: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const now = () => performance.now(); const spin = (ms) => { const e = now() + ms; while (now() < e); };
const R = []; const ch = new MessageChannel(); let L; ch.port1.onmessage = () => L.push('M');
function round(done) {
  L = []; let first = true; const sent = {};
  const go = (c, ms) => fetch('/slow?ms=' + ms + '&r=' + Math.random()).then((res) => { L.push(c);
    if (first) { first = false; ch.port2.postMessage(0); spin(4); } return res.json(); }).then((j) => { sent[c] = j.sent; });
  Promise.all([go('a', 30), go('b', 30.3), go('c', 32)]).then(() => setTimeout(() => { R.push({ ran: L.join(''), sent }); done(); }, 20));
}
function next(i) { if (i >= 10) { finish(JSON.stringify(R)); return; } round(() => setTimeout(() => next(i + 1), 15)); }
setTimeout(() => next(0), 50);
</script></body></html>"#;
#[tokio::test]
async fn responses_run_in_the_order_they_came_in() {
    // Up to three runs, as this file's other gates take them: under the gate's load (load average 17-20 beside the
    // whole suite, 2026-10-23) the server's own threads sent 0.3 ms-apart responses 4-8 ms apart, and the transport
    // handed some over late -- swaps a quiet run never shows (30 rounds of 30 in order). The loop this guards against
    // held responses back and swapped 3 to 8 rounds of 10 in every run.
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(ARRIVAL_ORDER, "").await).expect("json");
        let rounds = got.as_array().expect("rounds");
        assert_eq!(rounds.len(), 10, "{got}");
        // Two responses the server sent at least 0.2 ms apart run in that order (the server's own threads may swap two
        // sends under load: the order checked is the one they were SENT in, not the one they were asked for).
        let swapped = rounds
            .iter()
            .filter(|r| {
                let ran: Vec<char> = r["ran"].as_str().unwrap_or("").chars().filter(|c| *c != 'M').collect();
                let sent = |c: char| r["sent"][c.to_string()].as_f64().unwrap_or(f64::NAN);
                ran.windows(2).any(|w| sent(w[0]) > sent(w[1]) + 0.2)
            })
            .count();
        runs.push(got);
        if swapped <= 1 {
            return;
        }
    }
    panic!("a response does not run ahead of one sent before it (Chrome 152: aMbc): {runs:?}");
}

/// A BODY'S END IS THE BODY'S ARRIVAL. The head, then the 64-byte body 20 ms later, beside a loop of 5 ms `MessageChannel`
/// tasks; the tasks STARTED after the body was sent and before each step: Chrome 152 runs an XMLHttpRequest's
/// `readystatechange` 3 and 4 and its `load` after none (`0,0,0`, 14 of 15; `1,1,1` once), a body reader's first chunk
/// and its `done` after none (15 of 15), and resolves `json()` after ONE (15 of 15: a body consumer resolves from a task
/// of its own) -- 2026-10-23, three runs. Each further read of the body -- the end after its last chunk, then the body's
/// record -- had its answer the first time it was polled and was queued behind the loop's next message: `0,2,2`, `0,2`,
/// and `json()` after 2-3.
const BODY_END: &str = r#"<!doctype html><html><body><script>
window.R = new Promise((d) => { window.__done = d; });
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const now = () => performance.now(); const spin = (ms) => { const e = now() + ms; while (now() < e); };
const ep = (s) => s - performance.timeOrigin; const starts = []; const R = { xhr: [], json: [], reader: [] };
const cnt = (a, b) => b === undefined ? 'x' : starts.filter((s) => s >= a && s < b).length;
const U = () => '/split?ms=20&body=20&r=' + Math.random();
function sx(done) {
  const x = new XMLHttpRequest(); const t = {}; x.open('GET', U());
  x.onreadystatechange = () => { if (!t['rs' + x.readyState]) t['rs' + x.readyState] = now(); };
  x.onload = () => { t.load = now(); setTimeout(() => { const j = JSON.parse(x.responseText); const s = ep(j.sent);
    R.xhr.push([t.rs3, t.rs4, t.load].map((v) => cnt(s, v)).join(',')); done(); }, 0); };
  x.send();
}
function sj(done) {
  fetch(U()).then((r) => r.json()).then((j) => { const b = now(); setTimeout(() => { R.json.push(String(cnt(ep(j.sent), b))); done(); }, 0); });
}
function sr(done) {
  const t = {}; let txt = '';
  fetch(U()).then(async (r) => { const rd = r.body.getReader(); const dec = new TextDecoder();
    for (;;) { const { value, done: d } = await rd.read(); if (d) { t.done = now(); break; } if (!t.c1) t.c1 = now(); txt += dec.decode(value); } })
  .then(() => setTimeout(() => { const j = JSON.parse(txt); R.reader.push(cnt(ep(j.sent), t.c1) + ',' + cnt(ep(j.sent), t.done)); done(); }, 0));
}
let busy = true; const ch = new MessageChannel();
ch.port1.onmessage = () => { starts.push(now()); spin(5); if (busy) ch.port2.postMessage(0); };
function seq(list, done) { if (!list.length) return done(); list.shift()(() => seq(list, done)); }
setTimeout(() => { ch.port2.postMessage(0);
  const plan = []; for (let k = 0; k < 5; k++) plan.push((d) => sx(d), (d) => sj(d), (d) => sr(d));
  seq(plan, () => { busy = false; finish(JSON.stringify(R)); });
}, 50);
</script></body></html>"#;
#[tokio::test]
async fn a_body_s_end_runs_where_the_body_came_in() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(BODY_END, "").await).expect("json");
        let hits = |k: &str, want: &str| got[k].as_array().map_or(0, |a| a.iter().filter(|s| s.as_str() == Some(want)).count());
        let ok = hits("xhr", "0,0,0") >= 4 && hits("reader", "0,0") >= 4 && hits("json", "1") >= 4;
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("an XMLHttpRequest's load and a reader's done run with the body's arrival, json() a task later (Chrome 152: 0,0,0 / 0,0 / 1): {runs:?}");
}

/// AN ELEMENT'S LOAD IS A TASK POSTED AFTER THE RESPONSE. Beside a loop of 5 ms `MessageChannel` tasks, the tasks STARTED
/// after the response's end and before: an inserted async script ran and fired `load`, a `<link rel=stylesheet>` fired
/// `load`, an `Image` fired `load`. Chrome 152: 1,1 / 1 / 1 (15 of 15 each, three runs, 2026-10-23) -- Blink posts an
/// image's and a link's load event once the response is processed, and queues the script's execution. The image's and
/// the stylesheet's step ran as the response's own continuation: 0 and 0, an order Chrome never produced.
const SUBRESOURCE_LOAD: &str = r#"<!doctype html><html><head></head><body><script>
window.R = new Promise((d) => { window.__done = d; });
const finish = (v) => { const o = document.getElementById('out'); if (o) o.textContent = v; if (window.__done) window.__done(v); };
const now = () => performance.now(); const spin = (ms) => { const e = now() + ms; while (now() < e); };
const ep = (s) => s - performance.timeOrigin; const starts = []; const R = { script: [], css: [], img: [] };
const cnt = (a, b) => b === undefined ? 'x' : starts.filter((s) => s >= a && s < b).length;
let cur = null; window.__js = (name, sent) => { if (cur) { cur.run = now(); cur.sent = ep(sent); } };
function ss(done) {
  cur = {}; const c = cur; const s = document.createElement('script'); const u = location.origin + '/js?ms=30&name=s&r=' + Math.random();
  s.onload = () => { c.load = now(); setTimeout(() => { R.script.push(cnt(c.sent, c.run) + ',' + cnt(c.sent, c.load)); done(); }, 0); };
  s.src = u; document.head.append(s);
}
function sc(done) {
  const l = document.createElement('link'); l.rel = 'stylesheet'; const u = location.origin + '/css?ms=30&r=' + Math.random();
  l.onload = () => { const t = now(); setTimeout(() => { const e = performance.getEntriesByName(u)[0]; R.css.push(String(cnt(e ? e.responseEnd : NaN, t))); done(); }, 0); };
  l.href = u; document.head.append(l);
}
function si(done) {
  const i = new Image(); const u = location.origin + '/img?ms=30&r=' + Math.random();
  i.onload = () => { const t = now(); setTimeout(() => { const e = performance.getEntriesByName(u)[0]; R.img.push(String(cnt(e ? e.responseEnd : NaN, t))); done(); }, 0); };
  i.src = u;
}
let busy = true; const ch = new MessageChannel();
ch.port1.onmessage = () => { starts.push(now()); spin(5); if (busy) ch.port2.postMessage(0); };
function seq(list, done) { if (!list.length) return done(); list.shift()(() => seq(list, done)); }
setTimeout(() => { ch.port2.postMessage(0);
  const plan = []; for (let k = 0; k < 5; k++) plan.push((d) => ss(d), (d) => sc(d), (d) => si(d));
  seq(plan, () => { busy = false; finish(JSON.stringify(R)); });
}, 50);
</script></body></html>"#;
#[tokio::test]
async fn an_image_s_and_a_stylesheet_s_load_is_a_task_after_the_response() {
    let mut runs = Vec::new();
    for _ in 0..3 {
        let got: serde_json::Value = serde_json::from_str(&probe_page(SUBRESOURCE_LOAD, "").await).expect("json");
        let hits = |k: &str, want: &str| got[k].as_array().map_or(0, |a| a.iter().filter(|s| s.as_str() == Some(want)).count());
        // A DEBUG build turns a response around several times slower than a release one, and a 5 ms task of the loop
        // more starts meanwhile (the gate's debug run: script '2,2' in 2-3 of 5, img '2' in 0-2 of 5, three runs alike,
        // 2026-10-23; release '1,1' and '1' every time). There the order is checked, not Chrome's count: no load is
        // held behind more than one more task -- a batch of them would read 3 and up.
        let ok = if cfg!(debug_assertions) {
            ["img", "css", "script"].iter().all(|k| {
                got[*k].as_array().is_some_and(|a| a.len() == 5 && a.iter().all(|s| s.as_str().unwrap_or("x").split(',').all(|n| n.parse::<u32>().is_ok_and(|n| n <= 2))))
            })
        } else {
            hits("img", "1") >= 4 && hits("css", "1") >= 4 && hits("script", "1,1") >= 4
        };
        runs.push(got);
        if ok {
            return;
        }
    }
    panic!("an image's and a stylesheet's load run one task after the response, a script's as before (Chrome 152: 1 / 1 / 1,1): {runs:?}");
}
