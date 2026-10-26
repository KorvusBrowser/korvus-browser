// PARITY CS1 — `CompressionStream` / `DecompressionStream`. Evaluated UNMODIFIED by
// `tests/compression_live.rs` and by `google-chrome --headless=new` 150.0.7871.46 over CDP, so the two
// outputs diff line for line. `compression-probe.chrome-150.txt` is Chrome's output, RECORDED (two
// runs, identical), not written. A line that differs is a finding about the engine, never a reason to
// edit the file.
//
// NOTHING HERE TOUCHES THE NETWORK: the codec needs no server, so `BASE` is taken and ignored, and the
// two drivers share one shape with the other `*-probe.js` fixtures beside it.
//
// WHAT IS DELIBERATELY ABSENT, because it measures something else and would make this file lie about
// what it proves:
//   * `await writer.write(x)` before anything reads. Chrome's writable applies BACKPRESSURE, so that
//     promise never settles until the readable is drained, and this engine's WritableStream models
//     none -- a real difference, in `core/streams/writable_stream.rs`, not in the codec.
//   * compressed output over inputs above a few hundred KB. `flate2`'s `miniz_oxide` encoder agrees
//     with zlib byte for byte on everything below, and diverges above it (200 KB of zeros: 218 bytes
//     here against zlib's 219). Both are valid deflate; they are not the same encoding of it.
(async (BASE) => {
  const R = [];
  const E = (e) => "THREW " + (e && e.name) + ": " + (e && e.message);
  const TMO = (ms) => new Promise((r) => setTimeout(() => r("TIMEOUT"), ms));
  const t = async (name, f) => {
    try { R.push(name + " = " + (await Promise.race([Promise.resolve().then(f), TMO(20000)]))); } catch (e) { R.push(name + " = " + E(e)); }
  };
  const hex = (u8) => Array.from(u8).map((b) => b.toString(16).padStart(2, "0")).join("");
  const bytes = (s) => new TextEncoder().encode(s);
  const dec = (u) => new TextDecoder().decode(u);
  const src = (chunks) => new ReadableStream({ start(c) { for (const k of chunks) c.enqueue(k); c.close(); } });
  const drain = async (readable) => {
    const rd = readable.getReader(); const out = [];
    for (;;) { const r = await rd.read(); if (r.done) break; out.push(r.value); }
    return out;
  };
  const cat = (parts) => {
    let n = 0; for (const p of parts) n += p.byteLength;
    const u = new Uint8Array(n); let o = 0;
    for (const p of parts) { u.set(new Uint8Array(p.buffer || p, p.byteOffset || 0, p.byteLength), o); o += p.byteLength; }
    return u;
  };
  const through = async (chunks, ts) => cat(await drain(src(chunks).pipeThrough(ts)));
  const sizes = (a) => a.map((x) => x.byteLength).join(",");
  const desc = (o, k) => { const d = Object.getOwnPropertyDescriptor(o, k); return d ? Object.keys(d).map((x) => x + ":" + (typeof d[x] === "function" ? "fn" : d[x])).join(",") : "none"; };

  // ── the surface ──
  await t("typeof CompressionStream", () => typeof CompressionStream);
  await t("typeof DecompressionStream", () => typeof DecompressionStream);
  await t("CompressionStream.length", () => CompressionStream.length);
  await t("DecompressionStream.length", () => DecompressionStream.length);
  await t("CompressionStream.name", () => CompressionStream.name);
  await t("CompressionStream.toString", () => CompressionStream.toString());
  await t("CS proto own names", () => Object.getOwnPropertyNames(CompressionStream.prototype).join(","));
  await t("DS proto own names", () => Object.getOwnPropertyNames(DecompressionStream.prototype).join(","));
  await t("CS proto own symbols", () => Object.getOwnPropertySymbols(CompressionStream.prototype).map(String).join(","));
  await t("CS toStringTag", () => CompressionStream.prototype[Symbol.toStringTag]);
  await t("CS instance toString", () => Object.prototype.toString.call(new CompressionStream("gzip")));
  await t("CS instance own names", () => Object.getOwnPropertyNames(new CompressionStream("gzip")).join(","));
  await t("CS proto readable desc", () => desc(CompressionStream.prototype, "readable"));
  await t("CS proto writable desc", () => desc(CompressionStream.prototype, "writable"));
  await t("DS proto readable desc", () => desc(DecompressionStream.prototype, "readable"));
  await t("CS ctor desc on global", () => desc(globalThis, "CompressionStream"));
  await t("CS instanceof TransformStream", () => new CompressionStream("gzip") instanceof TransformStream);
  await t("CS proto proto", () => Object.getPrototypeOf(CompressionStream.prototype) === Object.prototype ? "Object.prototype" : String(Object.getPrototypeOf(CompressionStream.prototype)));
  await t("CS ctor proto", () => Object.getPrototypeOf(CompressionStream) === Function.prototype ? "Function.prototype" : String(Object.getPrototypeOf(CompressionStream)));
  await t("cs.readable instanceof ReadableStream", () => new CompressionStream("gzip").readable instanceof ReadableStream);
  await t("cs.writable instanceof WritableStream", () => new CompressionStream("gzip").writable instanceof WritableStream);
  await t("ds.readable instanceof ReadableStream", () => new DecompressionStream("gzip").readable instanceof ReadableStream);
  await t("ds.writable instanceof WritableStream", () => new DecompressionStream("gzip").writable instanceof WritableStream);
  await t("foreign receiver readable getter", () => Object.getOwnPropertyDescriptor(CompressionStream.prototype, "readable").get.call({}));
  await t("CS without new", () => CompressionStream("gzip"));
  await t("subclass keeps its prototype", () => { class X extends CompressionStream {} return new X("gzip") instanceof X; });

  // ── the format argument ──
  await t("CS()", () => { new CompressionStream(); return "no throw"; });
  await t("DS()", () => { new DecompressionStream(); return "no throw"; });
  for (const f of ["gzip", "deflate", "deflate-raw"]) {
    await t("CS('" + f + "')", () => { new CompressionStream(f); return "ok"; });
    await t("DS('" + f + "')", () => { new DecompressionStream(f); return "ok"; });
  }
  for (const f of ["br", "brotli", "Brotli", "BROTLI", "zstd", "dcb", "dcz", "identity", "compress", "x-gzip", "GZIP", "gzip ", "", "nope"]) {
    await t("CS('" + f + "')", () => { new CompressionStream(f); return "ok"; });
    await t("DS('" + f + "')", () => { new DecompressionStream(f); return "ok"; });
  }
  await t("CS(undefined)", () => { new CompressionStream(undefined); return "no throw"; });
  await t("CS(null)", () => { new CompressionStream(null); return "no throw"; });
  await t("CS(1)", () => { new CompressionStream(1); return "no throw"; });
  await t("CS({})", () => { new CompressionStream({}); return "no throw"; });
  await t("CS(['gzip'])", () => { new CompressionStream(["gzip"]); return "ok"; });
  await t("CS(toString gzip)", () => { new CompressionStream({ toString: () => "gzip" }); return "ok"; });
  await t("CS(valueOf gzip, no toString)", () => { new CompressionStream({ valueOf: () => "gzip", toString: undefined }); return "ok"; });
  await t("CS(toString throws Error)", () => { new CompressionStream({ toString() { throw new Error("boom"); } }); return "no throw"; });
  await t("CS(toString throws TypeError)", () => { new CompressionStream({ toString() { throw new TypeError("mine"); } }); return "no throw"; });
  await t("DS(toString throws TypeError)", () => { new DecompressionStream({ toString() { throw new TypeError("mine"); } }); return "no throw"; });
  await t("CS(Symbol())", () => { new CompressionStream(Symbol("s")); return "no throw"; });

  // ── round trips and the bytes ──
  const HI = bytes("hello hello hello hello world");
  for (const f of ["gzip", "deflate", "deflate-raw"]) {
    await t("compress " + f + " hex", async () => hex(await through([HI], new CompressionStream(f))));
    await t("compress " + f + " chunk sizes", async () => sizes(await drain(src([HI]).pipeThrough(new CompressionStream(f)))));
    await t("compress " + f + " empty hex", async () => hex(await through([], new CompressionStream(f))));
    await t("compress " + f + " empty chunk sizes", async () => sizes(await drain(src([]).pipeThrough(new CompressionStream(f)))));
    await t("roundtrip " + f, async () => dec(await through([await through([HI], new CompressionStream(f))], new DecompressionStream(f))));
    await t("roundtrip " + f + " empty", async () => (await through([await through([], new CompressionStream(f))], new DecompressionStream(f))).byteLength);
  }
  await t("compress gzip chunk kinds", async () => (await drain(src([HI]).pipeThrough(new CompressionStream("gzip")))).map((c) => Object.prototype.toString.call(c)).join(","));
  await t("compress gzip 3 writes chunk sizes", async () => sizes(await drain(src([bytes("aaa"), bytes("bbb"), bytes("ccc")]).pipeThrough(new CompressionStream("gzip")))));
  await t("multi-chunk compress then decompress", async () => dec(await through([await through([bytes("aaa"), bytes("bbb"), bytes("ccc")], new CompressionStream("gzip"))], new DecompressionStream("gzip"))));
  await t("an empty chunk between two changes nothing", async () => dec(await through([await through([bytes("Hello"), new Uint8Array(0), bytes("Hello")], new CompressionStream("deflate"))], new DecompressionStream("deflate"))));
  await t("compress gzip 1MB zeros length", async () => (await through([new Uint8Array(1024 * 1024)], new CompressionStream("gzip"))).byteLength);
  await t("decompress gzip 1MB zeros chunk sizes", async () => sizes(await drain(src([await through([new Uint8Array(1024 * 1024)], new CompressionStream("gzip"))]).pipeThrough(new DecompressionStream("gzip")))));
  await t("decompress split byte by byte", async () => {
    const c = await through([HI], new CompressionStream("gzip"));
    return dec(await through(Array.from(c).map((b) => new Uint8Array([b])), new DecompressionStream("gzip")));
  });
  await t("decompress accepts ArrayBuffer + DataView", async () => {
    const c = await through([HI], new CompressionStream("gzip"));
    return dec(await through([c.buffer.slice(0, 5), new DataView(c.buffer, 5)], new DecompressionStream("gzip")));
  });
  await t("gzip header bytes", async () => hex((await through([bytes("hi")], new CompressionStream("gzip"))).slice(0, 10)));
  await t("deflate header bytes", async () => hex((await through([bytes("hi")], new CompressionStream("deflate"))).slice(0, 2)));
  await t("gzip header is stable across a second", async () => {
    const a = await drain(src([bytes("x")]).pipeThrough(new CompressionStream("gzip")));
    await new Promise((r) => setTimeout(r, 1100));
    const b = await drain(src([bytes("x")]).pipeThrough(new CompressionStream("gzip")));
    return hex(a[0].slice(0, 10)) + " / " + hex(b[0].slice(0, 10));
  });

  // ── every error path ──
  const G = await through([bytes("hi")], new CompressionStream("gzip"));
  const Z = await through([bytes("hi")], new CompressionStream("deflate"));
  const tail = async (label, fmt, base, extra) => {
    await t(label, async () => dec(await through([cat([base, new Uint8Array(extra)])], new DecompressionStream(fmt))));
  };
  await tail("gzip + [0]", "gzip", G, [0]);
  await tail("gzip + [0,1,2]", "gzip", G, [0, 1, 2]);
  await tail("gzip + [0x1f]", "gzip", G, [0x1f]);
  await tail("gzip + [0x1f,0x8b]", "gzip", G, [0x1f, 0x8b]);
  await tail("zlib + [0,1,2]", "deflate", Z, [0, 1, 2]);
  await t("gzip + whole 2nd member (one chunk)", async () => dec(await through([cat([G, G])], new DecompressionStream("gzip"))));
  await t("gzip + whole 2nd member (two chunks)", async () => dec(await through([G, G], new DecompressionStream("gzip"))));
  await t("zlib + whole 2nd member", async () => dec(await through([cat([Z, Z])], new DecompressionStream("deflate"))));
  await t("deflate-raw + junk", async () => {
    const r = await through([bytes("hi")], new CompressionStream("deflate-raw"));
    return dec(await through([cat([r, new Uint8Array([0, 1, 2])])], new DecompressionStream("deflate-raw")));
  });
  await t("a member's value arrives BEFORE the junk error", async () => {
    const member = await through([bytes("expected output")], new CompressionStream("deflate"));
    const ds = new DecompressionStream("deflate");
    const reader = ds.readable.getReader();
    const first = reader.read().then((r) => (r.done ? "done" : dec(r.value)), (e) => "rej " + E(e));
    src([cat([member, new Uint8Array([0])])]).pipeTo(ds.writable).catch(() => {});
    const a = await first;
    const b = await reader.read().then((r) => (r.done ? "done" : "value"), (e) => "rej " + E(e));
    return a + " | " + b;
  });
  await t("decompress garbage as gzip", async () => dec(await through([new Uint8Array([1, 2, 3, 4, 5])], new DecompressionStream("gzip"))));
  await t("decompress raw deflate as 'deflate'", async () => dec(await through([await through([bytes("hi")], new CompressionStream("deflate-raw"))], new DecompressionStream("deflate"))));
  await t("decompress zlib as 'gzip'", async () => dec(await through([await through([bytes("hi")], new CompressionStream("deflate"))], new DecompressionStream("gzip"))));
  await t("zlib CM=11 header", async () => dec(await through([new Uint8Array([0xcb, 0x48, 0xcd])], new DecompressionStream("deflate"))));
  await t("zlib FCHECK ok but CM!=8", async () => {
    let flg = 0; for (let f = 0; f < 256; f++) { if (((0x1b << 8) + f) % 31 === 0) { flg = f; break; } }
    return dec(await through([new Uint8Array([0x1b, flg, 0, 0])], new DecompressionStream("deflate")));
  });
  await t("decompress bad crc", async () => {
    const c = (await through([HI], new CompressionStream("gzip"))).slice();
    c[c.byteLength - 5] ^= 0xff;
    return dec(await through([c], new DecompressionStream("gzip")));
  });
  await t("decompress bad isize", async () => {
    const c = G.slice(); c[c.byteLength - 1] ^= 0x01;
    return dec(await through([c], new DecompressionStream("gzip")));
  });
  await t("gzip with FNAME", async () => {
    const body = G.slice(10, G.byteLength);
    const head = new Uint8Array([0x1f, 0x8b, 0x08, 0x08, 0, 0, 0, 0, 0, 3, 0x61, 0x00]);
    return dec(await through([cat([head, body])], new DecompressionStream("gzip")));
  });
  await t("gzip os byte 0xff accepted", async () => { const c = G.slice(); c[9] = 0xff; return dec(await through([c], new DecompressionStream("gzip"))); });
  await t("gzip mtime nonzero accepted", async () => { const c = G.slice(); c[4] = 0x01; return dec(await through([c], new DecompressionStream("gzip"))); });
  await t("gzip truncated by 1", async () => dec(await through([G.slice(0, G.byteLength - 1)], new DecompressionStream("gzip"))));
  await t("gzip truncated by 4", async () => dec(await through([G.slice(0, G.byteLength - 4)], new DecompressionStream("gzip"))));
  await t("gzip header only", async () => dec(await through([G.slice(0, 10)], new DecompressionStream("gzip"))));
  await t("gzip empty input", async () => dec(await through([], new DecompressionStream("gzip"))));
  await t("zlib empty input", async () => dec(await through([], new DecompressionStream("deflate"))));
  await t("deflate-raw empty input", async () => dec(await through([], new DecompressionStream("deflate-raw"))));
  await t("deflate-raw truncated", async () => {
    const r = await through([bytes("hi")], new CompressionStream("deflate-raw"));
    return dec(await through([r.slice(0, r.byteLength - 1)], new DecompressionStream("deflate-raw")));
  });
  await t("compress write string chunk", async () => {
    const cs = new CompressionStream("gzip");
    const w = cs.writable.getWriter();
    const rd = cs.readable.getReader();
    const rp = rd.read().then(() => "readok", (e) => "readrej " + E(e));
    const wp = w.write("str").then(() => "writeok", (e) => "writerej " + E(e));
    return (await Promise.all([wp, rp])).join(" | ");
  });
  await t("decompress write number chunk", async () => {
    const ds = new DecompressionStream("gzip");
    const w = ds.writable.getWriter();
    const rd = ds.readable.getReader();
    const rp = rd.read().then(() => "readok", (e) => "readrej " + E(e));
    const wp = w.write(7).then(() => "writeok", (e) => "writerej " + E(e));
    return (await Promise.all([wp, rp])).join(" | ");
  });
  await t("compress write a view onto shared memory", async () => {
    const mem = new WebAssembly.Memory({ shared: true, initial: 1, maximum: 1 });
    const cs = new CompressionStream("gzip");
    const w = cs.writable.getWriter();
    const rd = cs.readable.getReader();
    const rp = rd.read().then(() => "readok", (e) => "readrej " + E(e));
    const wp = w.write(new Uint8Array(mem.buffer)).then(() => "writeok", (e) => "writerej " + E(e));
    return (await Promise.all([wp, rp])).join(" | ");
  });
  await t("compress write a SharedArrayBuffer", async () => {
    const mem = new WebAssembly.Memory({ shared: true, initial: 1, maximum: 1 });
    const cs = new CompressionStream("gzip");
    const w = cs.writable.getWriter();
    const rd = cs.readable.getReader();
    const rp = rd.read().then(() => "readok", (e) => "readrej " + E(e));
    const wp = w.write(mem.buffer).then(() => "writeok", (e) => "writerej " + E(e));
    return (await Promise.all([wp, rp])).join(" | ");
  });
  return R.join("\n");
})
