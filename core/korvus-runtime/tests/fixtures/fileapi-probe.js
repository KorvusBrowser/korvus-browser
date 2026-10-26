// W4.c surface + behaviour probe for `Blob` and `File`, in whichever realm it is evaluated.
// Returns a JSON string. Runs UNMODIFIED in the window, inside a real Worker isolate, and in
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
      r.k = "a"; r.g = !!d.get; r.s = !!d.set; r.nat = nat(d.get);
      if (d.get) { r.gn = d.get.name; r.gl = d.get.length; r.gt = tells(d.get); }
    } else {
      r.k = "d"; r.w = d.writable;
      if (typeof d.value === "function") { r.nat = nat(d.value); r.fn = d.value.name; r.fl = d.value.length; r.ft = tells(d.value); }
      else { try { r.v = JSON.stringify(d.value); } catch (e) { r.v = "?"; } }
    }
    return r;
  };
  const T = (f) => { try { const r = f(); return "OK:" + (r === undefined ? "undefined" : String(r)); } catch (e) { return (e && e.name) + ": " + String(e && e.message).slice(0, 130); } };
  const out = { iface: {}, rows: {}, foreign: {}, vals: {} };

  for (const n of ["Blob", "File"]) {
    const C = globalThis[n];
    if (typeof C !== "function") { out.iface[n] = { absent: typeof C }; continue; }
    const P = C.prototype;
    out.iface[n] = {
      name: C.name, length: C.length, nat: nat(C),
      protoOrder: Object.getOwnPropertyNames(P),
      protoChain: (() => { const a = []; let c = P; for (let i = 0; i < 6; i++) { c = Object.getPrototypeOf(c); if (!c) break; const k = c.constructor; a.push(k && k.name ? k.name : "?"); if (c === Object.prototype) break; } return a; })(),
      ctorProtoName: (() => { const p = Object.getPrototypeOf(C); return p && typeof p.name === "string" ? p.name : "?"; })(),
      tag: (() => { const d = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag); return d ? { v: d.value, e: d.enumerable, c: d.configurable, w: d.writable } : null; })(),
    };
    out.rows[n] = {};
    for (const m of Object.getOwnPropertyNames(P)) out.rows[n][m] = desc(P, m);
  }

  // FAILURE MODE 2: a foreign receiver on every Blob/File prototype row, on `{}` and on the
  // prototype object itself.
  //
  // A PROMISE-RETURNING operation never throws synchronously in WebIDL: the receiver failure becomes
  // a REJECTION. Measured on Chrome 150 -- `Blob.prototype.text.call({})` hands back a Promise that
  // rejects with "Failed to execute 'text' on 'Blob': Illegal invocation", while the value-returning
  // `slice`/`stream` throw `Illegal invocation` on the spot. So the three promise rows are recorded
  // as their REJECTION REASON (the probe returns a promise the caller awaits) and the rest as a throw.
  const PROMISE_ROWS = new Set(["arrayBuffer", "text", "bytes"]);
  const settle = [];
  for (const n of ["Blob", "File"]) {
    const C = globalThis[n];
    if (typeof C !== "function") continue;
    const P = C.prototype;
    out.foreign[n] = {};
    for (const m of Object.getOwnPropertyNames(P)) {
      if (m === "constructor") continue;
      const d = Object.getOwnPropertyDescriptor(P, m);
      for (const [label, recv] of [[m, {}], [m + "@proto", P]]) {
        if (PROMISE_ROWS.has(m)) {
          const key = label;
          out.foreign[n][key] = "NEVER-SETTLED";
          try {
            const p = d.value.call(recv);
            settle.push(Promise.resolve(p).then(
              (v) => { out.foreign[n][key] = "RESOLVED:" + String(v); },
              (e) => { out.foreign[n][key] = (e && e.name) + ": " + String(e && e.message).slice(0, 130); },
            ));
          } catch (e) { out.foreign[n][key] = "SYNC-THROW:" + (e && e.name) + ": " + String(e && e.message).slice(0, 130); }
        } else {
          out.foreign[n][label] = T(() => (d.get ? d.get.call(recv) : d.value.call(recv)));
        }
      }
    }
  }

  const B = globalThis.Blob, F = globalThis.File;
  const v = out.vals;
  // FAILURE MODE 1 + 3: a real instance owns nothing, and every row reads back off it.
  const b = new B(["ab", "cd"], { type: "TEXT/Plain" });
  v.ownNames = Object.getOwnPropertyNames(b);
  v.size = b.size;
  v.type = b.type;
  v.tag = Object.prototype.toString.call(b);
  v.isBlob = b instanceof B;
  v.empty = (() => { const x = new B(); return [x.size, x.type, Object.getOwnPropertyNames(x).length]; })();
  v.noArgOwn = Object.getOwnPropertyNames(new B());
  // The type normalisation is FileAPI's, not mimesniff's: outside U+0020..U+007E -> "", otherwise
  // the whole string ASCII-lowercased, nothing parsed or reformatted.
  v.types = ["text/plain; charset=UTF-8", "bogus", "  text/plain  ", 'text/plain;charset="UTF-8"', "aéb"].map((t) => new B([], { type: t }).type);
  // Byte-accurate parts: strings UTF-8, Blob, ArrayBuffer, ArrayBufferView.
  v.parts = (() => {
    const u = new Uint8Array([1, 2, 3]);
    const ab = new ArrayBuffer(4);
    return [new B(["é"]).size, new B([u]).size, new B([ab]).size, new B([u.subarray(1)]).size, new B([b]).size, new B([1, null, undefined]).size];
  })();
  v.endings = (() => [new B(["a\r\nb\rc"], { endings: "native" }).size, new B(["a\r\nb\rc"]).size])();
  v.slice = (() => { const s = b.slice(1, 3, "x/y"); return [s.size, s.type, s instanceof B]; })();
  v.sliceNeg = (() => { const s = b.slice(-2); return [s.size, s.type]; })();
  v.sliceNone = (() => { const s = b.slice(); return [s.size, s.type]; })();
  v.ctorErrs = {
    noIterable: T(() => new B(1)),
    nullParts: T(() => new B(null).size),
    stringParts: T(() => new B("abc").size),
    callNoNew: T(() => B([])),
  };
  if (typeof F === "function") {
    const f = new F(["ab"], "n.txt", { lastModified: 1700000000000, type: "text/plain" });
    v.fileOwnNames = Object.getOwnPropertyNames(f);
    v.file = [f.name, f.lastModified, f.webkitRelativePath, f.size, f.type, f instanceof B, f instanceof F];
    v.fileTag = Object.prototype.toString.call(f);
    v.fileLmDate = (() => { const d = f.lastModifiedDate; return d === null ? "null" : (d instanceof Date ? d.getTime() : typeof d); })();
    v.fileLmZero = (() => { const d = new F([], "z", { lastModified: 0 }).lastModifiedDate; return d === null ? "null" : (d instanceof Date ? d.getTime() : typeof d); })();
    v.fileCoerce = (() => { const g = new F([], 5, { lastModified: "12.7" }); return [g.name, g.lastModified]; })();
    v.fileLmNaN = (() => new F([], "z", { lastModified: NaN }).lastModified)();
    v.fileErrs = {
      noArgs: T(() => new F()),
      oneArg: T(() => new F(["a"])),
      callNoNew: T(() => F(["a"], "b")),
    };
    v.fileDefaultLmIsNow = (() => { const g = new F([], "n"); return Math.abs(g.lastModified - Date.now()) < 5000; })();
  }
  // The three promise rows above are settled before the dump is serialised, so the caller gets one
  // JSON string either way -- window, worker or a `--dump-dom` page.
  return Promise.all(settle).then(() => JSON.stringify(out));
})()
