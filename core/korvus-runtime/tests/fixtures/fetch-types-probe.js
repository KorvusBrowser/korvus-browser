// W4.c surface + behaviour probe for `Request`, `Response` and the `fetch` global, in whichever realm
// it is evaluated. Returns a JSON string. Runs UNMODIFIED in the window, inside a real Worker isolate,
// and in `google-chrome --headless=new`, so all three dumps are directly diffable.
//
// It never touches the NETWORK: `fetch` is measured as a function (shape, arity, receiver), and the
// two types are measured as constructed objects. The live network path is
// `tests/worker.rs::service_worker_fetch_interception` and the corpus.
(() => {
  const FTS = Function.prototype.toString;
  const nat = (f) => { try { return FTS.call(f).indexOf("[native code]") >= 0; } catch (e) { return "ERR"; } };
  const tells = (f) => { try { return ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k)); } catch (e) { return "ERR"; } };
  const desc = (o, n) => {
    let d; try { d = Object.getOwnPropertyDescriptor(o, n); } catch (e) { return { err: String(e) }; }
    if (!d) return null;
    const r = { e: d.enumerable, c: d.configurable };
    if (d.get || d.set) {
      r.k = "a"; r.g = !!d.get; r.s = !!d.set; r.nat = (!d.get || nat(d.get)) && (!d.set || nat(d.set));
      if (d.get) { r.gn = d.get.name; r.gl = d.get.length; r.gt = tells(d.get); }
    } else {
      r.k = "d"; r.w = d.writable;
      if (typeof d.value === "function") { r.nat = nat(d.value); r.fn = d.value.name; r.fl = d.value.length; r.ft = tells(d.value); }
      else { try { r.v = JSON.stringify(d.value); } catch (e) { r.v = "?"; } }
    }
    return r;
  };
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 140); } };
  const out = { iface: {}, rows: {}, foreign: {}, vals: {}, fetchFn: {} };
  const settle = [];

  for (const n of ["Request", "Response"]) {
    const C = globalThis[n];
    if (typeof C !== "function") { out.iface[n] = { absent: typeof C }; continue; }
    const P = C.prototype;
    out.iface[n] = {
      name: C.name, length: C.length, nat: nat(C),
      protoOrder: Object.getOwnPropertyNames(P),
      ctorOrder: Object.getOwnPropertyNames(C),
      ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
      tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
      noArgs: T(() => new C()),
      callNoNew: T(() => C("/x")),
    };
    out.rows[n] = {};
    for (const m of Object.getOwnPropertyNames(P)) out.rows[n][m] = desc(P, m);
    out.foreign[n] = {};
    for (const m of ["status", "ok", "statusText", "url", "type", "redirected", "bodyUsed", "method", "headers", "mode", "credentials"]) {
      const d = Object.getOwnPropertyDescriptor(P, m);
      if (!d || !d.get) continue;
      out.foreign[n][m] = T(() => d.get.call({}));
      out.foreign[n][m + "@proto"] = T(() => d.get.call(P));
    }
    // The METHOD rows. A promise-returning WebIDL operation never throws synchronously: the receiver
    // failure becomes a rejection. `clone` returns a value and throws. Recorded, not assumed.
    for (const m of ["arrayBuffer", "blob", "text", "json", "bytes", "clone"]) {
      const d = Object.getOwnPropertyDescriptor(P, m);
      if (!d || typeof d.value !== "function") continue;
      try {
        const r = d.value.call({});
        if (r && typeof r.then === "function") {
          out.foreign[n]["m:" + m] = "PENDING";
          settle.push(r.then(
            (x) => { out.foreign[n]["m:" + m] = "RESOLVED:" + String(x); },
            (e) => { out.foreign[n]["m:" + m] = (e && e.name) + ": " + String(e && e.message).slice(0, 130); },
          ));
        } else { out.foreign[n]["m:" + m] = "NO-THROW:" + String(r); }
      } catch (e) { out.foreign[n]["m:" + m] = "SYNC-THROW:" + (e && e.name) + ": " + String(e && e.message).slice(0, 130); }
    }
  }

  // The `fetch` GLOBAL as a function: a page reads all of this in three lines.
  const f = globalThis.fetch;
  out.fetchFn = {
    type: typeof f,
    name: typeof f === "function" ? f.name : null,
    length: typeof f === "function" ? f.length : null,
    nat: typeof f === "function" ? nat(f) : null,
    tells: typeof f === "function" ? tells(f) : null,
    desc: desc(globalThis, "fetch"),
    noArgs: "PENDING",
    newFetch: T(() => new globalThis.fetch("/x")),
  };
  // `fetch()` is a REJECTION in Chrome, not a throw, so it has to be awaited like the promise rows.
  try {
    const p = globalThis.fetch();
    if (p && typeof p.then === "function") {
      settle.push(p.then(
        (x) => { out.fetchFn.noArgs = "RESOLVED:" + String(x); },
        (e) => { out.fetchFn.noArgs = (e && e.name) + ": " + String(e && e.message).slice(0, 160); },
      ));
    } else out.fetchFn.noArgs = "NO-PROMISE:" + String(p);
  } catch (e) { out.fetchFn.noArgs = "SYNC-THROW:" + (e && e.name) + ": " + String(e && e.message).slice(0, 160); }

  const v = out.vals;
  const R = globalThis.Response;
  if (typeof R === "function") {
    const r = new R("hello", { status: 201, statusText: "Created", headers: { "x-a": "1" } });
    v.respOwn = Object.getOwnPropertyNames(r);
    v.respTag = Object.prototype.toString.call(r);
    v.resp = [r.status, r.ok, r.statusText, r.type, r.url, r.redirected, r.bodyUsed];
    v.respDefault = (() => { const d = new R(); return [d.status, d.ok, d.statusText, d.type]; })();
    v.respIs = r instanceof R;
  }
  const Q = globalThis.Request;
  if (typeof Q === "function") {
    const q = new Q("http://example.test/x", { method: "POST" });
    v.reqOwn = Object.getOwnPropertyNames(q);
    v.reqTag = Object.prototype.toString.call(q);
    v.req = [q.url, q.method, q.destination, q.referrer, q.referrerPolicy, q.mode, q.credentials, q.cache, q.redirect, q.integrity, q.keepalive, q.bodyUsed, q.isHistoryNavigation, q.isReloadNavigation];
    v.reqClone = (() => { const c = q.clone(); return [c instanceof Q, c.url, c.method]; })();
    v.reqIs = q instanceof Q;
    v.reqHeaders = (() => { try { return q.headers instanceof Headers; } catch (e) { return "ERR"; } })();
  }
  // The promise rows above are settled before the dump is serialised, so every runner gets one
  // JSON string.
  return Promise.all(settle).then(() => JSON.stringify(out));
})()
