// W4.c surface + behaviour probe for `Worker`, in whichever realm it is evaluated.
// Returns a Promise<JSON string>. Runs UNMODIFIED in the window and in
// `google-chrome --headless=new` (which reaches a real worker through a Blob URL), so the two dumps
// are directly diffable. In a realm that cannot spawn (a korvus worker isolate, which registers no
// spawn op) the behavioural half is skipped and only the SHAPE is reported.
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
  const C = globalThis.Worker;
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
    const call = (recv) => T(() => (d.get ? d.get.call(recv) : d.value.call(recv, "x")));
    out.foreign[m] = call({});
    out.foreign[m + "@proto"] = call(P);
  }

  // The BEHAVIOURAL half. A realm with no way to make a worker source reports `skipped` and the
  // assertions skip with it.
  const src = "self.onmessage = (e) => { if (e.data === 'boom') { throw new Error('worker blew up'); } self.postMessage('echo:' + e.data); };";
  let url = null;
  try { url = URL.createObjectURL(new Blob([src], { type: "text/javascript" })); } catch (e) { url = null; }
  if (!url) { out.vals = { skipped: "no blob URL" }; return Promise.resolve(JSON.stringify(out)); }
  let w;
  try { w = new C(url); } catch (e) { out.vals = { skipped: (e && e.name) + ": " + (e && e.message) }; return Promise.resolve(JSON.stringify(out)); }
  out.vals.ownNames = Object.getOwnPropertyNames(w);
  out.vals.isEventTarget = w instanceof EventTarget;
  out.vals.isWorker = w instanceof C;
  out.vals.tag = Object.prototype.toString.call(w);
  out.vals.handlersNull = [w.onmessage, w.onerror];
  const seen = [];
  return new Promise((res) => {
    let done = false;
    const finish = () => { if (done) return; done = true; out.vals.seen = seen; try { w.terminate(); } catch (e) {} res(JSON.stringify(out)); };
    w.onmessage = (e) => { seen.push("onmessage:" + e.data + ":" + (e.target === w) + ":" + (e instanceof MessageEvent)); };
    w.addEventListener("message", (e) => {
      seen.push("listener:" + e.data);
      if (e.data === "echo:two") { setTimeout(finish, 120); }
    });
    w.onerror = (e) => { seen.push("onerror:" + (e instanceof Event)); };
    w.postMessage("one");
    w.postMessage("boom");
    w.postMessage("two");
    setTimeout(finish, 2500);
  });
})()
