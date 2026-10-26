// W4.c surface + behaviour probe for `ImageData`. Returns a JSON string. Runs UNMODIFIED in the
// window and in `google-chrome --headless=new`, so the two dumps are directly diffable.
(() => {
  const FTS = Function.prototype.toString;
  const nat = (f) => { try { return FTS.call(f).indexOf("[native code]") >= 0; } catch (e) { return "ERR"; } };
  const tells = (f) => { try { return ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k)); } catch (e) { return "ERR"; } };
  const desc = (o, n) => {
    let d; try { d = Object.getOwnPropertyDescriptor(o, n); } catch (e) { return { err: String(e) }; }
    if (!d) return null;
    const r = { e: d.enumerable, c: d.configurable };
    if (d.get || d.set) {
      r.k = "a"; r.g = !!d.get; r.s = !!d.set; r.nat = nat(d.get);
      if (d.get) { r.gn = d.get.name; r.gl = d.get.length; r.gt = tells(d.get); }
    } else {
      r.k = "d"; r.w = d.writable;
      try { r.v = typeof d.value === "object" && d.value ? d.value.constructor.name : JSON.stringify(d.value); } catch (e) { r.v = "?"; }
    }
    return r;
  };
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 150); } };
  const C = globalThis.ImageData;
  if (typeof C !== "function") return JSON.stringify({ absent: typeof C });
  const P = C.prototype;
  const out = {
    iface: {
      name: C.name, length: C.length, nat: nat(C),
      protoOrder: Object.getOwnPropertyNames(P),
      ctorOrder: Object.getOwnPropertyNames(C),
      ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
      tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
    },
    rows: {}, foreign: {}, vals: {}, errs: {},
  };
  for (const m of Object.getOwnPropertyNames(P)) out.rows[m] = desc(P, m);
  for (const m of Object.getOwnPropertyNames(P)) {
    if (m === "constructor") continue;
    const d = Object.getOwnPropertyDescriptor(P, m);
    if (!d || !d.get) continue;
    out.foreign[m] = T(() => d.get.call({}));
    out.foreign[m + "@proto"] = T(() => d.get.call(P));
  }
  const v = out.vals;
  const a = new C(3, 2);
  v.own = Object.getOwnPropertyNames(a);
  v.tag = Object.prototype.toString.call(a);
  v.dims = [a.width, a.height, a.colorSpace, a.data.length, a.data.constructor.name, a.data[0]];
  v.isImageData = a instanceof C;
  v.pixelFormat = typeof a.pixelFormat === "string" ? a.pixelFormat : "absent:" + typeof a.pixelFormat;
  v.fromArray = (() => { const u = new Uint8ClampedArray(4 * 6); const b = new C(u, 3); return [b.width, b.height, b.data === u]; })();
  v.fromArrayWithHeight = (() => { const u = new Uint8ClampedArray(4 * 6); const b = new C(u, 3, 2); return [b.width, b.height]; })();
  v.settings = (() => { const b = new C(1, 1, { colorSpace: "display-p3" }); return b.colorSpace; })();
  const E = out.errs;
  E.noArgs = T(() => new C());
  E.oneArg = T(() => new C(2));
  E.zeroWidth = T(() => new C(0, 1));
  E.zeroHeight = T(() => new C(1, 0));
  E.negative = T(() => new C(-1, 1));
  E.callNoNew = T(() => C(1, 1));
  E.badArrayLength = T(() => new C(new Uint8ClampedArray(5), 1));
  E.mismatchHeight = T(() => new C(new Uint8ClampedArray(8), 1, 3));
  E.huge = T(() => new C(100000, 100000));
  E.fractional = T(() => { const b = new C(2.9, 1.9); return [b.width, b.height].join(","); });
  return JSON.stringify(out);
})()
