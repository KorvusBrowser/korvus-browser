// SSOT W6.2 surface probe for THE GLOBAL SCOPE ITSELF, in whichever realm it is evaluated.
//
// Returns a JSON string. Runs UNMODIFIED in the window and in a real worker isolate -- in
// `google-chrome --headless=new` (which reaches a real worker through a Blob URL, so the page must be
// served over http, never `file://`) and in this engine. The two dumps are directly diffable, and the
// DIFF is the instrument: `tests/worker_surface.rs` compares MEMBERS of interfaces and structurally
// cannot see an interface OBJECT that reports JS source in one realm and `[native code]` in the other
// (which is how W6.1 found `Function.prototype.toString.call(EventTarget)` handing back a class body
// inside a worker).
//
// What it reports, per realm:
//   globals   -- `Object.getOwnPropertyNames(globalThis)` IN ORDER, plus a descriptor for each
//   chain     -- the global's prototype chain, by constructor name, and each link's own member names
//   scope     -- the scope interface's prototype members with full descriptors
//   ifaceNat  -- `Function.prototype.toString.call(X)` nativeness for every interface object on the
//                global (the asymmetry `worker_surface` cannot see)
//   singles   -- constructor name + own/proto member names of the four singletons
//   behaviour -- the handful of live answers a shape dump cannot give (timers, importScripts, ...)
(() => {
  const FTS = Function.prototype.toString;
  const nat = (f) => { try { return FTS.call(f).indexOf("[native code]") >= 0; } catch (e) { return "ERR"; } };
  const tells = (f) => { try { return ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k)); } catch (e) { return "ERR"; } };
  const desc = (o, n) => {
    let d; try { d = Object.getOwnPropertyDescriptor(o, n); } catch (e) { return { err: String(e) }; }
    if (!d) return null;
    const r = { e: d.enumerable, c: d.configurable };
    if (d.get || d.set) {
      r.k = "a"; r.g = !!d.get; r.s = !!d.set;
      r.nat = (!d.get || nat(d.get)) && (!d.set || nat(d.set));
      if (d.get) { r.gn = d.get.name; r.gl = d.get.length; r.gt = tells(d.get); }
      if (d.set) { r.sn = d.set.name; r.sl = d.set.length; }
    } else {
      r.k = "d"; r.w = d.writable;
      if (typeof d.value === "function") { r.nat = nat(d.value); r.fn = d.value.name; r.fl = d.value.length; r.ft = tells(d.value); }
      else { r.t = typeof d.value; }
    }
    return r;
  };
  const ctorName = (o) => { try { const c = o && o.constructor; return c && c.name ? c.name : String(o); } catch (e) { return "ERR"; } };
  const out = { realm: "", globals: [], gdesc: {}, chain: [], chainMembers: {}, scope: {}, ifaceNat: {}, singles: {}, behaviour: {} };

  out.realm = (typeof WorkerGlobalScope !== "undefined" && globalThis instanceof WorkerGlobalScope) ? "worker" : "window";

  const G = globalThis;
  out.globals = Object.getOwnPropertyNames(G);
  for (const n of out.globals) out.gdesc[n] = desc(G, n);

  // The chain, by constructor name, with each link's own member NAMES (order included).
  {
    let c = Object.getPrototypeOf(G);
    for (let i = 0; i < 8 && c; i++) {
      const nm = ctorName(c);
      out.chain.push(nm);
      out.chainMembers[nm] = Object.getOwnPropertyNames(c);
      if (c === Object.prototype) break;
      c = Object.getPrototypeOf(c);
    }
  }

  // The SCOPE interface prototype, with descriptors: WorkerGlobalScope in a worker, Window in a page.
  {
    const S = out.realm === "worker" ? G.WorkerGlobalScope : G.Window;
    if (typeof S === "function" && S.prototype) {
      const P = S.prototype;
      out.scope.iface = S.name;
      out.scope.natCtor = nat(S);
      out.scope.order = Object.getOwnPropertyNames(P);
      out.scope.rows = {};
      for (const m of out.scope.order) out.scope.rows[m] = desc(P, m);
      out.scope.tag = (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })();
    }
  }

  // EVERY interface object on the global: is it `[native code]`, and does it own the three shim tells?
  // This is the column `worker_surface.rs` does not have.
  for (const n of out.globals) {
    let v; try { v = G[n]; } catch (e) { continue; }
    if (typeof v !== "function") continue;
    if (!/^[A-Z]/.test(n)) continue;
    out.ifaceNat[n] = { nat: nat(v), tells: tells(v), len: v.length, name: v.name };
  }

  // The four singletons a fingerprinter reads by name.
  for (const n of ["navigator", "location", "crypto", "performance", "console"]) {
    let v; try { v = G[n]; } catch (e) { continue; }
    if (v == null) continue;
    const proto = Object.getPrototypeOf(v);
    out.singles[n] = {
      ctor: ctorName(v),
      own: Object.getOwnPropertyNames(v),
      proto: proto ? Object.getOwnPropertyNames(proto) : null,
    };
  }

  // Live answers. Shape cannot show whether a timer id is real, whether clearTimeout cancels, or
  // whether importScripts exists as anything but a name.
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 120); } };
  out.behaviour.setTimeoutId = T(() => { const id = G.setTimeout(() => {}, 5); G.clearTimeout(id); return typeof id + ":" + (Number(id) > 0); });
  out.behaviour.setIntervalId = T(() => { const id = G.setInterval(() => {}, 5); G.clearInterval(id); return typeof id + ":" + (Number(id) > 0); });
  out.behaviour.setTimeoutNoArgs = T(() => G.setTimeout());
  out.behaviour.queueMicrotaskNoArgs = T(() => G.queueMicrotask());
  out.behaviour.selfIsGlobal = T(() => G.self === G);
  out.behaviour.originType = T(() => typeof G.origin);
  out.behaviour.isSecureContext = T(() => String(G.isSecureContext));
  out.behaviour.importScripts = T(() => typeof G.importScripts);
  out.behaviour.atob = T(() => G.atob("aGk="));
  out.behaviour.structuredClone = T(() => JSON.stringify(G.structuredClone({ a: 1 })));
  out.behaviour.queueMicrotask = T(() => typeof G.queueMicrotask);
  out.behaviour.selfOwn = T(() => Object.prototype.hasOwnProperty.call(G, "self"));

  // NOT part of the asymmetry set (the two realms are ALLOWED to answer differently here): what each
  // scope member actually ANSWERS, so a row that is a D2 stub can be told from one that is not.
  out.scopeValues = {};
  for (const n of ["self", "location", "navigator", "origin", "performance", "trustedTypes", "crypto",
                   "indexedDB", "fonts", "isSecureContext", "crossOriginIsolated", "scheduler", "caches",
                   "createImageBitmap", "fetch", "importScripts", "atob", "btoa", "queueMicrotask",
                   "reportError", "structuredClone", "setTimeout", "clearTimeout", "setInterval",
                   "clearInterval", "name", "close", "postMessage", "requestAnimationFrame"]) {
    out.scopeValues[n] = T(() => { const v = G[n]; return typeof v + (v === null ? ":null" : (typeof v === "object" || typeof v === "function" ? "" : ":" + String(v))); });
  }
  out.scopeValues["name="] = T(() => { const was = G.name; G.name = "korvus-probe"; const now = String(G.name); try { G.name = was; } catch (e) {} return now; });
  out.scopeValues["origin="] = T(() => { const was = G.origin; G.origin = "korvus-probe"; const now = String(G.origin); try { G.origin = was; } catch (e) {} return now; });
  out.scopeValues["performance="] = T(() => { const was = G.performance; G.performance = 1; const now = String(G.performance); try { G.performance = was; } catch (e) {} return now; });
  out.behaviour.protoSelf = T(() => { const p = Object.getPrototypeOf(G); let c = p; while (c && !Object.prototype.hasOwnProperty.call(c, "self")) c = Object.getPrototypeOf(c); return c ? ctorName(c) : "none"; });

  return JSON.stringify(out);
})()
