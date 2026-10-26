// W4.c surface + behaviour probe for `AbortController` and `AbortSignal`, in whichever realm it is
// evaluated. Returns a Promise<JSON string> (AbortSignal.timeout needs a turn of the event loop to
// fire). Runs UNMODIFIED in the window, inside a real Worker isolate, and in
// `google-chrome --headless=new`, so all three dumps are directly diffable.
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
  const out = { iface: {}, rows: {}, statics: {}, foreign: {}, vals: {} };

  for (const n of ["AbortController", "AbortSignal"]) {
    const C = globalThis[n];
    if (typeof C !== "function") { out.iface[n] = { absent: typeof C }; continue; }
    const P = C.prototype;
    out.iface[n] = {
      name: C.name, length: C.length, nat: nat(C),
      newable: T(() => new C()),
      protoOrder: Object.getOwnPropertyNames(P),
      ctorOrder: Object.getOwnPropertyNames(C),
      protoChain: (() => { const a = []; let c = P; for (let i = 0; i < 6; i++) { c = Object.getPrototypeOf(c); if (!c) break; const k = c.constructor; a.push(k && k.name ? k.name : "?"); if (c === Object.prototype) break; } return a; })(),
      ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
      tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
    };
    out.rows[n] = {};
    for (const m of Object.getOwnPropertyNames(P)) out.rows[n][m] = desc(P, m);
    out.statics[n] = {};
    for (const m of Object.getOwnPropertyNames(C)) if (!["length", "name", "prototype"].includes(m)) out.statics[n][m] = desc(C, m);
  }

  // FAILURE MODE 2: a foreign receiver on every row of both prototypes.
  for (const n of ["AbortController", "AbortSignal"]) {
    const C = globalThis[n];
    if (typeof C !== "function") continue;
    const P = C.prototype;
    out.foreign[n] = {};
    for (const m of Object.getOwnPropertyNames(P)) {
      if (m === "constructor") continue;
      const d = Object.getOwnPropertyDescriptor(P, m);
      const call = (recv) => T(() => (d.get ? d.get.call(recv) : d.value.call(recv)));
      out.foreign[n][m] = call({});
      out.foreign[n][m + "@proto"] = call(P);
    }
  }

  const v = out.vals;
  const AC = globalThis.AbortController, AS = globalThis.AbortSignal;
  const c = new AC();
  v.ctlOwn = Object.getOwnPropertyNames(c);
  v.sigOwn = Object.getOwnPropertyNames(c.signal);
  v.sigTag = Object.prototype.toString.call(c.signal);
  v.sameObject = c.signal === c.signal;
  v.isEventTarget = c.signal instanceof EventTarget;
  v.isSignal = c.signal instanceof AS;
  v.before = [c.signal.aborted, c.signal.reason, c.signal.onabort];
  v.throwIfNotAborted = T(() => c.signal.throwIfAborted());
  // abort(): the flag, the default DOMException reason, one `abort` event, and idempotence.
  const seen = [];
  c.signal.addEventListener("abort", (e) => seen.push("listener:" + e.type + ":" + (e.target === c.signal)));
  c.signal.onabort = (e) => seen.push("onabort:" + e.type);
  c.abort();
  c.abort("again");
  v.events = seen;
  v.after = [c.signal.aborted, c.signal.reason instanceof DOMException, c.signal.reason && c.signal.reason.name, c.signal.reason && c.signal.reason.message];
  v.throwIfAborted = T(() => c.signal.throwIfAborted());
  v.abortWithReason = (() => { const x = new AC(); x.abort("boom"); return [x.signal.aborted, x.signal.reason]; })();
  v.abortUndefined = (() => { const x = new AC(); x.abort(undefined); return [x.signal.reason instanceof DOMException, x.signal.reason.name]; })();
  // The three statics.
  v.staticAbort = (() => { const s = AS.abort(); return [s.aborted, s.reason instanceof DOMException, s.reason.name, s instanceof AS]; })();
  v.staticAbortReason = (() => { const s = AS.abort(42); return [s.aborted, s.reason]; })();
  v.anyErrs = { noArg: T(() => AS.any()), notIterable: T(() => AS.any(1)), badElement: T(() => AS.any([{}])) };
  v.timeoutErrs = { noArg: T(() => AS.timeout()), negative: T(() => AS.timeout(-1)), fractional: T(() => AS.timeout(1.5)) };
  v.anyAlreadyAborted = (() => { const a = AS.abort("x"); const s = AS.any([a, new AC().signal]); return [s.aborted, s.reason]; })();
  const ctlLive = new AC();
  const anySig = AS.any([ctlLive.signal]);
  const anySeen = [];
  anySig.addEventListener("abort", () => anySeen.push("any:" + anySig.reason));
  const to = AS.timeout(1);
  const toSeen = [];
  to.addEventListener("abort", () => toSeen.push("timeout:" + (to.reason && to.reason.name)));
  ctlLive.abort("propagated");
  v.anyPropagates = [anySig.aborted, anySig.reason, anySeen];
  return new Promise((res) => {
    setTimeout(() => {
      v.timeout = [to.aborted, to.reason && to.reason.name, to.reason instanceof DOMException, toSeen];
      res(JSON.stringify(out));
    }, 60);
  });
})()
