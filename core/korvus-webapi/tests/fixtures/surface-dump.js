// Web API surface introspection dumper. Runs UNMODIFIED in both a real browser (paste into the
// DevTools console of the target Brave build) and in Korvus (the surface_parity test evals it), so
// the two dumps are directly diffable. Captures the *shape* fingerprinters read: which interfaces
// exist, their prototype chain, and per-member descriptors + [native code] + enumeration ORDER
// (order is itself a signal, so members are NOT sorted). Returns a JSON string.
//
// Usage in the target browser:
//   1. open the exact target build, DevTools console
//   2. copy(  (function(){ /* paste this file's body */ })()  )   // copies the JSON to clipboard
//   3. save as brave-<chromium-version>.surface.json next to this file
(() => {
  const FP_TS = Function.prototype.toString;
  const isNative = (fn) => { try { return FP_TS.call(fn).includes("[native code]"); } catch (e) { return false; } };
  const ownNames = (o) => { try { return Object.getOwnPropertyNames(o); } catch (e) { return []; } };
  // A native method/getter must NOT own `prototype`/`arguments`/`caller` (a JS-function shim tell).
  const ownTells = (fn) => { try { return ["prototype", "arguments", "caller"].some((k) => Object.prototype.hasOwnProperty.call(fn, k)); } catch (e) { return false; } };

  // Classify one prototype member from its descriptor.
  const member = (proto, name) => {
    let d; try { d = Object.getOwnPropertyDescriptor(proto, name); } catch (e) { return null; }
    if (!d) return null;
    const out = { name, enumerable: d.enumerable, configurable: d.configurable };
    if (d.get || d.set) {
      out.kind = "accessor";
      out.get = !!d.get; out.set = !!d.set;
      out.native = (!d.get || isNative(d.get)) && (!d.set || isNative(d.set));
      // Function identity beyond [native code]: native accessors are named "get x"/"set x" with
      // getter length 0 / setter length 1. A raw v8::Function that skipped set_name/length shows ""/0.
      if (d.get) { out.getName = d.get.name; out.getLength = d.get.length; }
      if (d.set) { out.setName = d.set.name; out.setLength = d.set.length; }
      out.ownTell = (!!d.get && ownTells(d.get)) || (!!d.set && ownTells(d.set));
    } else if (typeof d.value === "function") {
      out.kind = "method";
      out.native = isNative(d.value);
      out.length = d.value.length;
      out.fnName = d.value.name;
      out.ownTell = ownTells(d.value);
    } else {
      out.kind = "value";
      out.type = typeof d.value;
      // Constants (DOM Node.ELEMENT_NODE etc.) are part of the shape; capture primitives only.
      if (d.value === null || typeof d.value !== "object") out.value = d.value;
    }
    return out;
  };

  // --- LIVE VALUES (axis ④ "value from profile") -----------------------------------------------
  // The descriptor tells the SHAPE of a member; axis ④ needs its VALUE as actually read off a live
  // instance (a getter's result, a constant). We only record values that are JSON-safe scalars or
  // shallow primitive arrays -- functions and objects (NodeLists, style decls, sub-objects) are the
  // "large objects" the task says to skip, so they carry no `liveValue` (their shape meta still
  // stands). Every read is guarded: a throwing getter simply yields no value.
  const MAX_STR = 4096; // cap: a value longer than this is treated as a large object and skipped
  const MAX_ARR = 64;   // cap: an array longer than this is skipped
  // Classify a raw JS value -> { has:true, value } if JSON-safe, else { has:false }.
  const jsonSafe = (val) => {
    try {
      if (val === null) return { has: true, value: null };
      const t = typeof val;
      if (t === "boolean" || t === "number") return { has: true, value: val };
      if (t === "string") return val.length <= MAX_STR ? { has: true, value: val } : { has: false };
      if (Array.isArray(val)) {
        if (val.length > MAX_ARR) return { has: false };
        const out = [];
        for (const el of val) {
          const et = typeof el;
          if (el === null || et === "boolean" || et === "number") { out.push(el); }
          else if (et === "string") { if (el.length > MAX_STR) return { has: false }; out.push(el); }
          else return { has: false }; // a non-primitive element => skip the whole array
        }
        return { has: true, value: out };
      }
      return { has: false }; // object / function / symbol / bigint / undefined
    } catch (e) { return { has: false }; }
  };
  // Read `inst[name]` (invoking any getter) and JSON-classify it; functions are skipped.
  const readLive = (inst, name) => {
    let val; try { val = inst[name]; } catch (e) { return { has: false }; }
    if (typeof val === "function") return { has: false };
    return jsonSafe(val);
  };
  // Decorate a member record with `liveValue` read off `inst`, when JSON-safe. `rec` may be null.
  const withLive = (rec, inst) => {
    if (!rec) return rec;
    const r = readLive(inst, rec.name);
    if (r.has) rec.liveValue = r.value;
    return rec;
  };
  // Walk an instance's whole prototype chain (most-derived first, deduped, Object.prototype excluded)
  // into per-member records carrying `liveValue`. Chain-walking (vs a single proto level) is what makes
  // a DOM element instance useful: HTMLDivElement.prototype is nearly empty -- nodeName/tagName/
  // isConnected/namespaceURI live up on Element/Node. Dedup keeps the most-derived member, so its
  // descriptor and the value `inst[name]` resolves to stay consistent.
  const walkInstance = (inst) => {
    const out = [];
    const seen = new Set();
    let proto = Object.getPrototypeOf(inst);
    let depth = 0;
    while (proto && proto !== Object.prototype && depth < 12) {
      for (const n of ownNames(proto)) {
        if (n === "constructor" || seen.has(n)) continue;
        seen.add(n);
        out.push(withLive(member(proto, n), inst));
      }
      proto = Object.getPrototypeOf(proto);
      depth++;
    }
    return out.filter(Boolean);
  };

  // Walk an interface constructor into a comparable record.
  const iface = (name, ctor) => {
    const rec = { typeof: typeof ctor, ctorNative: isNative(ctor) };
    if (typeof ctor === "function") {
      rec.length = ctor.length;
      rec.ctorName = ctor.name;
      // Constructability: does `new X()` work, or throw "is not a constructor"/"Illegal constructor"?
      // Argless — a constructor that REQUIRES args throws too (the aggregator only faults the argless
      // cases WebIDL says must construct; arg-requiring ones are the agent's per-case job).
      try { new ctor(); rec.newable = true; } catch (e) { rec.newable = false; }
      // Whether the interface's prototype is iterable (Symbol.iterator present + native).
      try {
        const it = ctor.prototype && ctor.prototype[Symbol.iterator];
        rec.iterable = typeof it === "function";
        if (rec.iterable) rec.iteratorNative = isNative(it);
      } catch (e) {}
      // static members (skip the universal function own-props)
      const skip = new Set(["length", "name", "prototype", "arguments", "caller"]);
      rec.statics = ownNames(ctor).filter((n) => !skip.has(n)).map((n) => member(ctor, n)).filter(Boolean);
      const proto = ctor.prototype;
      if (proto) {
        // prototype chain by constructor name (Element -> Node -> EventTarget -> Object)
        const chain = [];
        let p = Object.getPrototypeOf(proto);
        while (p && p !== Object.prototype && chain.length < 12) {
          const cn = p.constructor && p.constructor.name;
          chain.push(cn || "?");
          p = Object.getPrototypeOf(p);
        }
        rec.proto = chain;
        // WHERE `constructor` SITS, which is a fact about the interface and not about `constructor`.
        //
        // Its descriptor is the same everywhere (w=1 e=0 c=1, the interface object) so it was
        // filtered out of `members` from the start -- and that filter hid a whole-surface ordering
        // defect for as long as this rig has existed. Measured 2026-09-19, korvus put `constructor`
        // FIRST on 635 of 930 shared interfaces; real Chrome 150 does on 115 of 955. No gate could
        // see it, because this file never wrote the position down.
        //
        // The browser's rule, read off Chrome 150: `constructor` is the LAST member installed at
        // interface-definition time, so members added by a PARTIAL interface land after it.
        // Element 151/152, Node 47/48, Blob 7/8, URL 14/15, DOMRect 4/5, Event 21/22 -- all last;
        // StorageManager 2/5 (`getDirectory`, `persist` are partials), Navigator 31/80 and
        // Performance 13/19, which everything bolts onto.
        //
        // Recorded as an INDEX rather than by un-filtering the member list, so every existing
        // consumer of `members` keeps reading exactly what it read before.
        const protoNames = ownNames(proto);
        rec.ctorIndex = protoNames.indexOf("constructor");
        rec.members = protoNames.filter((n) => n !== "constructor").map((n) => member(proto, n)).filter(Boolean);
        try { rec.toStringTag = proto[Symbol.toStringTag]; } catch (e) {}
      }
    }
    return rec;
  };

  const globals = ownNames(globalThis);
  const interfaces = {};
  for (const name of globals) {
    // Interface objects: capitalized name whose value is a function with a .prototype.
    if (!/^[A-Z]/.test(name)) continue;
    let v; try { v = globalThis[name]; } catch (e) { continue; }
    if (typeof v !== "function" || !v.prototype) continue;
    interfaces[name] = iface(name, v);
  }

  // A few non-constructor globals that carry fingerprint surface. Each member additionally carries its
  // live value read off the singleton instance `v` (axis ④) -- e.g. navigator.hardwareConcurrency,
  // screen.width, location.href, document.title. Walk depth is unchanged (the immediate prototype).
  const singletons = {};
  // `window` is included so the Window interface's live dims (innerWidth/innerHeight/devicePixelRatio/
  // screenX/…) get a liveValue for axis ④ (window_dims). It is keyed by its ctor name "Window"; in a
  // worker realm `window` is undefined and simply skipped by the null guard below.
  for (const name of ["navigator", "screen", "location", "crypto", "performance", "document", "window"]) {
    let v; try { v = globalThis[name]; } catch (e) { continue; }
    if (v == null) continue;
    const proto = Object.getPrototypeOf(v);
    singletons[name] = {
      ctor: v.constructor && v.constructor.name,
      members: proto ? ownNames(proto).filter((n) => n !== "constructor").map((n) => withLive(member(proto, n), v)).filter(Boolean) : [],
    };
  }

  // Curated live instances: constructing real objects lets us capture per-member liveValues that only
  // exist on an instance (URL getters, a DOM element's Node/Element chain, an Event, a DOMMatrix). Each
  // constructor is guarded so a missing/throwing interface just omits that instance (additive). Keyed by
  // interface name; members are the full prototype-chain walk with `liveValue`.
  const instances = {};
  const instanceSpecs = [
    ["URL", () => new URL("https://a.b/c?x=1#h")],
    ["URLSearchParams", () => new URLSearchParams("a=1")],
    ["HTMLDivElement", () => document.createElement("div")],
    ["Event", () => new Event("x")],
    ["DOMMatrix", () => new DOMMatrix()],
    // NavigatorUAData: navigator.userAgentData is the live instance carrying the sync client-hints
    // scalars (mobile/platform) for axis ④. Not a constructor call — just the singleton object.
    ["NavigatorUAData", () => navigator.userAgentData],
  ];
  for (const [label, make] of instanceSpecs) {
    let inst; try { inst = make(); } catch (e) { continue; }
    if (inst == null) continue;
    let ctor; try { ctor = inst.constructor && inst.constructor.name; } catch (e) {}
    const chain = [];
    try {
      let p = Object.getPrototypeOf(inst);
      while (p && p !== Object.prototype && chain.length < 12) {
        const cn = p.constructor && p.constructor.name;
        chain.push(cn || "?");
        p = Object.getPrototypeOf(p);
      }
    } catch (e) {}
    instances[label] = { ctor, proto: chain, members: walkInstance(inst) };
  }

  // Automation-tell deepening: ChromeDriver injects cdc_ keys onto `document` too (not just window), and
  // Function.prototype.toString is a classic stealth override target — it must itself be native.
  const documentKeys = (typeof document !== "undefined") ? ownNames(document) : [];
  const fpToStringNative = isNative(FP_TS);

  return JSON.stringify({
    meta: { engine: (typeof globalThis.chrome !== "undefined") ? "chromium" : "korvus", ua: (globalThis.navigator && navigator.userAgent) || "" },
    globals,
    documentKeys,
    fpToStringNative,
    interfaces,
    singletons,
    instances,
  });
})()
