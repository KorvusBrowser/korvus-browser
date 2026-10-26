// W4.c surface + behaviour probe for `SharedWorker`, in whichever realm it is evaluated.
// Returns a Promise<JSON string>. Runs UNMODIFIED in the window and in
// `google-chrome --headless=new` (which reaches a real shared worker through a Blob URL), so the two
// dumps are directly diffable.
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
      if (d.set) { r.sn = d.set.name; r.sl = d.set.length; }
    } else {
      r.k = "d"; r.w = d.writable;
      if (typeof d.value === "function") { r.nat = nat(d.value); r.fn = d.value.name; r.fl = d.value.length; r.ft = tells(d.value); }
      else { try { r.v = JSON.stringify(d.value); } catch (e) { r.v = "?"; } }
    }
    return r;
  };
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 140); } };
  const out = { iface: {}, rows: {}, foreign: {}, vals: {} };
  const C = globalThis.SharedWorker;
  if (typeof C !== "function") return Promise.resolve(JSON.stringify({ absent: typeof C }));
  const P = C.prototype;
  out.iface = {
    name: C.name, length: C.length, nat: nat(C),
    protoOrder: Object.getOwnPropertyNames(P),
    ctorOrder: Object.getOwnPropertyNames(C),
    protoChain: (() => { const a = []; let c = P; for (let i = 0; i < 6; i++) { c = Object.getPrototypeOf(c); if (!c) break; const k = c.constructor; a.push(k && k.name ? k.name : "?"); if (c === Object.prototype) break; } return a; })(),
    ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
    tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
    noArgs: T(() => new C()),
    callNoNew: T(() => C("x")),
  };
  for (const m of Object.getOwnPropertyNames(P)) out.rows[m] = desc(P, m);
  for (const m of Object.getOwnPropertyNames(P)) {
    if (m === "constructor") continue;
    const d = Object.getOwnPropertyDescriptor(P, m);
    const call = (recv) => T(() => (d.get ? d.get.call(recv) : d.value.call(recv)));
    out.foreign[m] = call({});
    out.foreign[m + "@proto"] = call(P);
  }

  // The worker answers TWO questions on one port: `scope` reports what its own global scope IS, and
  // anything else is echoed. The scope report is what testharness.js reads before it can run at all --
  // `create_test_environment()` picks `SharedWorkerTestEnvironment` on
  // `'SharedWorkerGlobalScope' in self && self instanceof SharedWorkerGlobalScope`, and falls through
  // to the DEDICATED environment (which publishes down a plane a page holding a `SharedWorker` has no
  // end of) when that name is missing. Sent on REQUEST rather than unsolicited so the two answers are
  // ordered by the port's own queue instead of by a race with `start()`.
  const SCOPES = ["DedicatedWorkerGlobalScope", "SharedWorkerGlobalScope", "ServiceWorkerGlobalScope", "WorkerGlobalScope"];
  const src =
    "const SCOPES = " + JSON.stringify(SCOPES) + ";" +
    "self.onconnect = (e) => { const p = e.ports[0];" +
    "  p.onmessage = (m) => {" +
    "    if (m.data !== 'scope') { p.postMessage('shared:' + m.data); return; }" +
    "    p.postMessage('scope:' + JSON.stringify({" +
    "      names: SCOPES.filter((n) => n in self)," +
    "      isShared: typeof SharedWorkerGlobalScope === 'function' && self instanceof SharedWorkerGlobalScope," +
    "      protoName: (() => { const q = Object.getPrototypeOf(self); return q && q.constructor ? q.constructor.name : '?'; })()," +
    "      tag: Object.prototype.toString.call(self)" +
    "    }));" +
    "  };" +
    "  p.start(); };";
  let url = null;
  try { url = URL.createObjectURL(new Blob([src], { type: "text/javascript" })); } catch (e) { url = null; }
  if (!url) { out.vals = { skipped: "no blob URL" }; return Promise.resolve(JSON.stringify(out)); }
  let w;
  try { w = new C(url, "probe"); } catch (e) { out.vals = { skipped: (e && e.name) + ": " + (e && e.message) }; return Promise.resolve(JSON.stringify(out)); }
  out.vals.ownNames = Object.getOwnPropertyNames(w);
  out.vals.isEventTarget = w instanceof EventTarget;
  out.vals.isShared = w instanceof C;
  out.vals.tag = Object.prototype.toString.call(w);
  out.vals.portIsMessagePort = w.port instanceof MessagePort;
  out.vals.portSameObject = w.port === w.port;
  out.vals.onerrorNull = w.onerror;
  out.vals.onerrorRoundTrip = (() => { const f = () => {}; w.onerror = f; const same = w.onerror === f; w.onerror = 5; const cleared = w.onerror; w.onerror = null; return [same, cleared === null || cleared === undefined ? "null-ish" : String(cleared)]; })();
  const seen = [];
  out.scope = null;
  return new Promise((res) => {
    let done = false;
    const finish = () => { if (done) return; done = true; out.vals.seen = seen; res(JSON.stringify(out)); };
    w.port.onmessage = (e) => {
      const d = String(e.data);
      // The scope report does not enter `seen`: it is a property of the worker's realm, not of the
      // echo path, and the echo assertion is a list of exactly what came back down the message plane.
      if (d.indexOf("scope:") === 0) { try { out.scope = JSON.parse(d.slice(6)); } catch (err) { out.scope = { err: String(err) }; } return; }
      seen.push("port:" + d);
      finish();
    };
    try { w.port.start(); } catch (e) {}
    w.port.postMessage("scope");
    w.port.postMessage("ping");
    setTimeout(finish, 2500);
  });
})()
