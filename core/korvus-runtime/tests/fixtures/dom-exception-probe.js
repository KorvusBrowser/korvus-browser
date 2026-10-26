// W4.c surface + behaviour probe for `DOMException`, in whichever realm it is evaluated.
// Returns a JSON string. Runs UNMODIFIED in the window, inside a real Worker isolate, and in
// `google-chrome --headless=new`, so all three dumps are directly diffable -- which is how the
// expectations in `tests/dom_exception_live.rs` were obtained rather than reasoned about.
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
      try { r.v = d.value === undefined ? "undefined" : JSON.stringify(d.value); } catch (e) { r.v = "?"; }
    }
    return r;
  };
  const C = globalThis.DOMException;
  if (typeof C !== "function") return JSON.stringify({ absent: typeof C });
  const P = C.prototype;
  const out = {
    ctor: { name: C.name, length: C.length, nat: nat(C), tells: tells(C) },
    // WebIDL gives DOMException %Error.prototype% for its PROTOTYPE object and leaves the interface
    // object on %Function.prototype%. `Object.getPrototypeOf(DOMException).name` is "" in Chrome
    // (Function.prototype), which `class DOMException extends Error` could not reproduce.
    ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
    protoOrder: Object.getOwnPropertyNames(P),
    ctorOrder: Object.getOwnPropertyNames(C),
    protoChain: (() => { const a = []; let c = P; for (let i = 0; i < 8; i++) { c = Object.getPrototypeOf(c); if (!c) break; const k = c.constructor; a.push(k && k.name ? k.name : "?"); if (c === Object.prototype) break; } return a; })(),
    tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
    rows: {}, consts: {}, statics: {}, fm: {}, vals: {},
  };
  for (const n of ["code", "name", "message"]) out.rows[n] = desc(P, n);
  for (const n of ["INDEX_SIZE_ERR", "ABORT_ERR", "DATA_CLONE_ERR"]) {
    out.consts[n] = desc(P, n);
    out.statics[n] = desc(C, n);
  }

  // FAILURE MODE 1: the prototype must not be an instance of itself.
  out.fm.protoIsInstance = (() => { try { return P instanceof C; } catch (e) { return "ERR"; } })();
  // FAILURE MODE 2: a foreign receiver. Chrome throws `TypeError: Illegal invocation` for all three
  // rows -- INCLUDING when the receiver is DOMException.prototype itself.
  out.fm.foreign = {};
  for (const n of ["code", "name", "message"]) {
    const d = Object.getOwnPropertyDescriptor(P, n);
    const call = (recv) => { try { const r = d.get.call(recv); return "OK:" + String(r); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 40); } };
    out.fm.foreign[n] = call({});
    out.fm.foreign[n + "@proto"] = call(P);
    out.fm.foreign[n + "@error"] = call(new Error("x"));
  }
  out.fm.jsonStringifyProto = (() => { try { return JSON.stringify(P); } catch (e) { return "THROW:" + (e && e.name); } })();
  // FAILURE MODE 3: the constructor's own guards.
  const T = (label, f) => { try { out.fm[label] = "OK:" + String(f()); } catch (e) { out.fm[label] = (e && e.name) + ": " + String(e && e.message).slice(0, 120); } };
  T("callNoNew", () => DOMException("a"));

  // The INSTANCE. A real DOMException owns nothing at all -- no `stack`, no `message`, no `name`.
  const e = new C("msg", "AbortError");
  out.vals.ownNames = Object.getOwnPropertyNames(e);
  out.vals.isError = e instanceof Error;
  out.vals.isDomException = e instanceof C;
  out.vals.stackType = typeof e.stack;
  out.vals.toString = String(e);
  out.vals.tag = Object.prototype.toString.call(e);
  out.vals.json = (() => { try { return JSON.stringify(e); } catch (x) { return "THROW"; } })();
  const triple = (...a) => { const d = new C(...a); return [d.name, d.message, d.code]; };
  out.vals.noArg = (() => { const d = new C(); return [d.name, d.message, d.code]; })();
  out.vals.named = triple("x", "NotFoundError");
  out.vals.unknownName = triple("x", "Bogus");
  out.vals.abort = triple("m", "AbortError");
  out.vals.undefArgs = triple(undefined, undefined);
  out.vals.constOnProto = P.ABORT_ERR;
  out.vals.constOnCtor = C.ABORT_ERR;
  // A thrown DOMException must still be catchable as an Error and answer `.name` through the
  // prototype accessor -- the round trip a page's error handling actually makes.
  out.vals.thrown = (() => { try { throw new C("boom", "InvalidStateError"); } catch (x) { return [x instanceof Error, x.name, x.code, x.message]; } })();
  // THE deno_core ERROR BUILDERS, read back from the only place a page can see them: an OP that
  // fails with a DOM exception NAME. `atob("x")` is a forgiving-base64 failure, which the infra spec
  // makes an `InvalidCharacterError`, and `op_atob` returns it as `JsErrorBox::new("InvalidCharacterError", …)`.
  // Without a registered builder deno_core's `buildCustomError` answers `undefined` for the class and
  // the page catches something that is not a DOMException at all. Both realms, because the
  // registration is per-realm.
  out.vals.fromOp = (() => {
    try { globalThis.atob("x"); return "NO-THROW"; }
    catch (x) { return [x instanceof C, x instanceof Error, x && x.name, x && x.code]; }
  })();
  return JSON.stringify(out);
})()
