// W4.b surface probe: the nineteen event-family interfaces, in whichever realm it is evaluated.
// Returns a JSON string. Deliberately shape-only, so window and worker dumps are directly diffable
// against each other's BEFORE/AFTER.
(() => {
  const NAMES = ["Event","CustomEvent","UIEvent","MouseEvent","PointerEvent","WheelEvent","Touch",
    "TouchEvent","KeyboardEvent","InputEvent","FocusEvent","BeforeUnloadEvent","ErrorEvent",
    "ProgressEvent","MessageEvent","CookieChangeEvent","MessageChannel","Notification","EventTarget"];
  const FTS = Function.prototype.toString;
  const nat = (f) => { try { return FTS.call(f).indexOf("[native code]") >= 0; } catch (e) { return "ERR"; } };
  const tells = (f) => { try { return ["prototype","arguments","caller"].some((k) => Object.prototype.hasOwnProperty.call(f, k)); } catch (e) { return "ERR"; } };
  const desc = (o, n) => {
    let d; try { d = Object.getOwnPropertyDescriptor(o, n); } catch (e) { return { err: String(e) }; }
    if (!d) return null;
    const r = { e: d.enumerable, c: d.configurable };
    if (d.get || d.set) {
      r.k = "a"; r.g = !!d.get; r.s = !!d.set; r.nat = (!d.get || nat(d.get)) && (!d.set || nat(d.set));
      if (d.get) { r.gn = d.get.name; r.gl = d.get.length; r.gt = tells(d.get); }
      if (d.set) { r.sn = d.set.name; r.sl = d.set.length; r.st = tells(d.set); }
    } else {
      r.k = "d"; r.w = d.writable;
      if (typeof d.value === "function") { r.nat = nat(d.value); r.fn = d.value.name; r.fl = d.value.length; r.ft = tells(d.value); }
      else { try { r.v = d.value === undefined ? "undefined" : JSON.stringify(d.value); } catch (e) { r.v = "?"; } }
    }
    return r;
  };
  const chain = (p) => { const out = []; let c = p; for (let i = 0; i < 12 && c; i++) { c = Object.getPrototypeOf(c); if (!c) break; const ct = c.constructor; out.push(ct && ct.name ? ct.name : "?"); if (c === Object.prototype) break; } return out; };
  // A representative instance per interface: what `new X(...)` produces, when it is constructible.
  const MK = {
    Event: 'new Event("t")', CustomEvent: 'new CustomEvent("t",{detail:7})', UIEvent: 'new UIEvent("t")',
    MouseEvent: 'new MouseEvent("click",{clientX:5,clientY:6,button:2})', PointerEvent: 'new PointerEvent("p",{pointerId:3})',
    WheelEvent: 'new WheelEvent("w",{deltaY:9})', Touch: 'new Touch({identifier:1, target: globalThis.document ? document.body : new EventTarget()})', TouchEvent: 'new TouchEvent("t")',
    KeyboardEvent: 'new KeyboardEvent("k",{key:"a",keyCode:65})', InputEvent: 'new InputEvent("i",{data:"x"})',
    FocusEvent: 'new FocusEvent("f")', BeforeUnloadEvent: 'new BeforeUnloadEvent()', ErrorEvent: 'new ErrorEvent("e",{message:"m",lineno:3})',
    ProgressEvent: 'new ProgressEvent("p",{loaded:1.5,total:-5.5,lengthComputable:"hah"})',
    MessageEvent: 'new MessageEvent("m",{data:{a:1},origin:"o"})', CookieChangeEvent: 'new CookieChangeEvent("c")',
    MessageChannel: 'new MessageChannel()', Notification: 'new Notification("hi",{body:"b"})', EventTarget: 'new EventTarget()',
  };
  const out = {};
  for (const n of NAMES) {
    const C = globalThis[n];
    if (typeof C !== "function") { out[n] = { absent: typeof C }; continue; }
    const P = C.prototype;
    const rec = {
      ctor: { name: C.name, length: C.length, nat: nat(C), tells: tells(C), chain: chain(C) },
      ctorOwn: {}, protoOwn: {}, protoOrder: Object.getOwnPropertyNames(P),
      protoSyms: Object.getOwnPropertySymbols(P).map(String),
      ctorOrder: Object.getOwnPropertyNames(C),
      chain: chain(P),
    };
    for (const k of rec.protoOrder) rec.protoOwn[k] = desc(P, k);
    for (const k of rec.ctorOrder) rec.ctorOwn[k] = desc(C, k);
    const tag = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag);
    rec.tag = tag ? { v: tag.value, e: tag.enumerable, c: tag.configurable, w: tag.writable, acc: !!tag.get } : null;
    // instance shape
    try {
      const inst = eval(MK[n]);
      rec.inst = { own: Object.getOwnPropertyNames(inst), keys: Object.keys(inst), str: Object.prototype.toString.call(inst) };
      const it = Object.getOwnPropertyDescriptor(inst, "isTrusted");
      rec.inst.isTrusted = it ? { e: it.enumerable, c: it.configurable, acc: !!it.get, gn: it.get ? it.get.name : null, gl: it.get ? it.get.length : null, nat: it.get ? nat(it.get) : null, v: inst.isTrusted } : null;
      rec.inst.protoHasIsTrusted = Object.prototype.hasOwnProperty.call(P, "isTrusted");
      // value read-back of every prototype accessor, OWN and INHERITED: `which` lives on
      // UIEvent.prototype but is interface-specific (button + 1 on a mouse event, keyCode on a key
      // event), so a per-interface own-keys walk cannot see the value that differs.
      rec.vals = {};
      const keys = rec.protoOrder.slice();
      for (let p = Object.getPrototypeOf(P); p && p !== Object.prototype; p = Object.getPrototypeOf(p)) {
        for (const k of Object.getOwnPropertyNames(p)) if (keys.indexOf(k) < 0) keys.push(k);
      }
      for (const k of keys) {
        if (k === "constructor") continue;
        try { const v = inst[k]; rec.vals[k] = typeof v === "function" ? "fn" : (v === undefined ? "undefined" : (v === null ? "null" : (typeof v === "object" ? Object.prototype.toString.call(v) : JSON.stringify(v)))); }
        catch (e) { rec.vals[k] = "THROW:" + (e && e.name); }
      }
    } catch (e) { rec.inst = { err: String(e && e.message || e) }; }
    // the four failure modes
    rec.fm = {};
    // 1. an instance on the prototype
    rec.fm.protoOwnIsTrusted = Object.prototype.hasOwnProperty.call(P, "isTrusted");
    rec.fm.protoIsInstance = (() => { try { return P instanceof C; } catch (e) { return "ERR"; } })();
    // 2. a foreign receiver
    rec.fm.foreign = {};
    for (const k of rec.protoOrder) {
      if (k === "constructor") continue;
      const d = Object.getOwnPropertyDescriptor(P, k);
      try {
        if (d.get) { const v = d.get.call({}); rec.fm.foreign[k] = "OK:" + (v === undefined ? "undefined" : (v === null ? "null" : typeof v)); }
        else if (typeof d.value === "function") { const v = d.value.call({}); rec.fm.foreign[k] = "OK:" + (v === undefined ? "undefined" : typeof v); }
      } catch (e) { rec.fm.foreign[k] = (e && e.name) + ":" + (e && e.message ? String(e.message).slice(0, 60) : ""); }
    }
    // 3. constructor argument handling
    rec.fm.ctorErrs = {};
    const T = (label, expr) => { try { const r = eval(expr); rec.fm.ctorErrs[label] = "OK:" + (r === undefined ? "undefined" : typeof r); } catch (e) { rec.fm.ctorErrs[label] = (e && e.name) + ":" + String(e && e.message).slice(0, 80); } };
    T("noargs", "new " + n + "()");
    T("callnonew", n + "('t')");
    T("baddict", "new " + n + "('t', 5)");
    T("symboltype", "new " + n + "(Symbol('s'))");
    out[n] = rec;
  }
  // The LEGACY positional initialisers. Each writes the same slots its constructor writes, so a
  // page that builds an event with document.createEvent and fills it in this way reads back what it
  // set -- the check that catches a writer and a reader drifting onto two different private slots.
  const L = (k, f) => { try { out.init = out.init || {}; out.init[k] = String(f()); } catch (e) { out.init = out.init || {}; out.init[k] = "THROW:" + (e && e.name); } };
  L("initEvent", () => { const e = new Event("a"); e.initEvent("b", true, true); return [e.type, e.bubbles, e.cancelable].join(","); });
  L("initCustomEvent", () => { const e = new CustomEvent("a"); e.initCustomEvent("b", true, true, 42); return [e.type, e.bubbles, e.detail].join(","); });
  L("initCustomEventNoDetail", () => { const e = new CustomEvent("a", { detail: 9 }); e.initCustomEvent("b"); return String(e.detail); });
  if (globalThis.UIEvent) L("initUIEvent", () => { const e = new UIEvent("a"); e.initUIEvent("b", true, true, null, 5); return [e.type, e.detail].join(","); });
  if (globalThis.MouseEvent) L("initMouseEvent", () => { const e = new MouseEvent("a"); e.initMouseEvent("b", true, true, null, 1, 2, 3, 4, 5); return [e.type, e.detail, e.clientX].join(","); });
  if (globalThis.KeyboardEvent) L("initKeyboardEvent", () => { const e = new KeyboardEvent("a"); e.initKeyboardEvent("b", true, true, null, "q", 2); return [e.type, e.key, e.location].join(","); });
  L("initMessageEvent", () => { const e = new MessageEvent("a"); e.initMessageEvent("b", true, true, { z: 1 }, "org", "lid"); return [e.type, e.bubbles, e.data.z, e.origin, e.lastEventId].join(","); });
  return JSON.stringify(out);
})()
