// W4.c surface + behaviour probe for `btoa`, `atob`, `crypto` and `console`, in whichever realm it is
// evaluated. Returns a JSON string. Runs UNMODIFIED in the window, inside a real Worker isolate, and
// in `google-chrome --headless=new` (window and, over a loopback page, a real dedicated worker).
(() => {
  const FTS = Function.prototype.toString;
  const nat = (f) => { try { return FTS.call(f).indexOf("[native code]") >= 0; } catch (e) { return "ERR"; } };
  const tells = (f) => { try { return ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k)); } catch (e) { return "ERR"; } };
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 150); } };
  // THE MEMBER'S DESCRIPTOR, WHEREVER IT LIVES, PLUS WHO HOLDS IT.
  //
  // This used to read `getOwnPropertyDescriptor(globalThis, n)` only, and that is a WINDOW assumption:
  // `btoa` is an own property of `window`, but in a worker it is a `WorkerGlobalScope` operation and
  // lives on `WorkerGlobalScope.prototype`, which the global INHERITS. Asked of real Chrome
  // (`--headless=new`, a loopback page spawning a real dedicated worker, 2026-09-12):
  // `Object.getOwnPropertyDescriptor(self, "btoa")` is `null` there and
  // `Object.getOwnPropertyNames(self)` contains only `console` of these names. So the probe walks the
  // chain and REPORTS the holder, and the test asserts the holder per realm -- which pins one more of
  // Chrome's facts than the own-only read could see at all.
  const holderOf = (n) => {
    let o = globalThis;
    while (o) {
      if (Object.prototype.hasOwnProperty.call(o, n)) {
        return o === globalThis ? "globalThis" : ((o.constructor && o.constructor.name) || "?") + ".prototype";
      }
      o = Object.getPrototypeOf(o);
    }
    return null;
  };
  const descIn = (n) => {
    let o = globalThis;
    while (o) {
      const d = Object.getOwnPropertyDescriptor(o, n);
      if (d) return d;
      o = Object.getPrototypeOf(o);
    }
    return null;
  };
  const fnDesc = (n) => {
    const d = descIn(n);
    if (!d) return null;
    const f = d.value;
    return { e: d.enumerable, c: d.configurable, w: d.writable, holder: holderOf(n), name: f && f.name, length: f && f.length, nat: nat(f), tells: tells(f) };
  };
  const out = { b64: {}, crypto: {}, console: {} };
  out.b64.btoa = fnDesc("btoa");
  out.b64.atob = fnDesc("atob");
  out.b64.round = T(() => atob(btoa("hiÿ")));
  out.b64.btoaNoArgs = T(() => btoa());
  out.b64.atobNoArgs = T(() => atob());
  out.b64.btoaUnicode = T(() => btoa("Ā"));
  out.b64.atobBad = T(() => atob("!!!"));
  out.b64.btoaNumber = T(() => btoa(123));
  out.b64.atobWhitespace = T(() => atob(" aGk= "));
  out.b64.btoaForeign = T(() => btoa.call({}, "hi"));

  const c = globalThis.crypto;
  out.crypto.tag = Object.prototype.toString.call(c);
  out.crypto.own = Object.getOwnPropertyNames(c);
  out.crypto.ctorName = c && c.constructor && c.constructor.name;
  out.crypto.isCrypto = typeof globalThis.Crypto === "function" ? c instanceof globalThis.Crypto : "no-Crypto";
  out.crypto.uuidShape = T(() => /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(c.randomUUID()));
  out.crypto.fills = T(() => { const a = new Uint8Array(16); const r = c.getRandomValues(a); return (r === a) + ":" + (a.some((x) => x !== 0)); });
  // `crypto.desc` stays an OWN-only read on purpose: `the_crypto_singleton_...` asserts that a worker's
  // global has NO own `crypto` (Chrome answers an inherited accessor on WorkerGlobalScope.prototype).
  // The chain answer is `crypto.holder` beside it.
  out.crypto.desc = (() => { const d = Object.getOwnPropertyDescriptor(globalThis, "crypto"); return d ? { e: d.enumerable, c: d.configurable, w: d.writable, acc: !!(d.get || d.set) } : null; })();
  out.crypto.holder = holderOf("crypto");
  out.navigator = { holder: holderOf("navigator"), acc: (() => { const d = descIn("navigator"); return d ? !!(d.get || d.set) : null; })() };
  out.fetchHolder = holderOf("fetch");
  out.structuredCloneHolder = holderOf("structuredClone");

  const k = globalThis.console;
  out.console.tag = Object.prototype.toString.call(k);
  out.console.names = Object.getOwnPropertyNames(k);
  out.console.symbols = Object.getOwnPropertySymbols(k).map(String);
  out.console.tagDesc = (() => { const d = Object.getOwnPropertyDescriptor(k, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })();
  out.console.proto = (() => { const p = Object.getPrototypeOf(k); return p === Object.prototype ? "Object.prototype" : (p && p.constructor && p.constructor.name) || String(p); })();
  out.console.logNative = nat(k.log);
  out.console.logName = k.log && k.log.name;
  out.console.logLength = k.log && k.log.length;
  out.console.desc = (() => { const d = Object.getOwnPropertyDescriptor(globalThis, "console"); return d ? { e: d.enumerable, c: d.configurable, w: d.writable } : null; })();
  out.console.memberDesc = (() => { const d = Object.getOwnPropertyDescriptor(k, "log"); return d ? { e: d.enumerable, c: d.configurable, w: d.writable } : null; })();
  return JSON.stringify(out);
})()
