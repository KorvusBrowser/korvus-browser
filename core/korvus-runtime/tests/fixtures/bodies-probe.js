// PARITY P9e — bodies are bytes, and the byte streams are real. Evaluated UNMODIFIED by
// `tests/bodies_live.rs` (against its loopback server) and by `google-chrome --headless=new` 150.0.7871.46
// (against `bodies-probe-server.py`, which answers every route byte-identically), so the two outputs
// diff line for line. `bodies-probe.chrome-150.txt` is Chrome's output, recorded, not written.
//
// `BASE` is the server's origin with a trailing slash. Resolves with the result lines joined by "\n".
(async (BASE) => {
  const R = [];
  const E = (e) => "THREW " + (e && e.name) + ": " + (e && e.message);
  const t = async (name, f) => {
    try { R.push(name + " = " + (await f())); } catch (e) { R.push(name + " = " + E(e)); }
  };
  const tag = (v) => Object.prototype.toString.call(v);
  const ctl = () => { let c; const rs = new ReadableStream({ start(x) { c = x; } }); return [rs, c]; };
  const drain = async (readable) => { const rd = readable.getReader(); const out = []; for (;;) { const r = await rd.read(); if (r.done) break; out.push(r.value); } return out; };
  const xhr = (setup) => new Promise((res) => { const x = new XMLHttpRequest(); setup(x); x.onloadend = () => res(x); x.send(); });

  // ── the brief's table ──
  await t("TextDecoderStream().writable instanceof WritableStream", () => new TextDecoderStream().writable instanceof WritableStream);
  await t("Response('hi').body === null", () => new Response("hi").body === null);
  await t("sync XHR b.bin responseText.length", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "b.bin", false); x.send(); return x.responseText.length; });
  await t("sync XHR b.bin x-user-defined responseText.length", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "b.bin", false); x.overrideMimeType("text/plain; charset=x-user-defined"); x.send(); return x.responseText.length + "/" + x.responseText.charCodeAt(128); });
  await t("fetch b.bin arrayBuffer byteLength/u[128]", async () => { const u = new Uint8Array(await (await fetch(BASE + "b.bin")).arrayBuffer()); return u.byteLength + "/" + u[128]; });
  await t("XHR arraybuffer response", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "arraybuffer"; }); const r = x.response; return tag(r) + " " + (r && r.byteLength) + "/" + (r && new Uint8Array(r)[128]); });
  await t("XHR json response", async () => { const x = await xhr((x) => { x.open("GET", BASE + "j.json"); x.responseType = "json"; }); return typeof x.response + " " + JSON.stringify(x.response); });
  await t("pipeThrough(TextDecoderStream) read", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode("hi")); c.close(); } }); const rd = rs.pipeThrough(new TextDecoderStream()).getReader(); const a = await rd.read(); const b = await rd.read(); return JSON.stringify([a, b.done]); });

  // ── streams ──
  await t("TextDecoderStream props", () => { const d = new TextDecoderStream("utf-8", { fatal: true }); return [d.encoding, d.fatal, d.ignoreBOM, d.readable instanceof ReadableStream].join(","); });
  await t("TextDecoderStream latin1", () => new TextDecoderStream("latin1").encoding);
  await t("TextDecoderStream bad label", () => new TextDecoderStream("nope"));
  await t("TextDecoderStream replacement label", () => new TextDecoderStream("iso-2022-kr"));
  await t("TextDecoderStream foreign readable", () => Object.getOwnPropertyDescriptor(TextDecoderStream.prototype, "readable").get.call({}));
  await t("TextEncoderStream props", () => { const d = new TextEncoderStream(); return [d.encoding, d.readable instanceof ReadableStream, d.writable instanceof WritableStream].join(","); });
  await t("TDS split utf8", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([0xc3])); c.enqueue(new Uint8Array([0xa9, 0x41])); c.enqueue(new Uint8Array([0xc3])); c.close(); } }); return JSON.stringify(await drain(rs.pipeThrough(new TextDecoderStream()))); });
  await t("TDS BOM across chunks", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([0xef, 0xbb])); c.enqueue(new Uint8Array([0xbf, 0x41])); c.close(); } }); return JSON.stringify(await drain(rs.pipeThrough(new TextDecoderStream()))); });
  await t("TDS buffer kinds", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([104, 105]).buffer); c.enqueue(new DataView(new Uint8Array([33]).buffer)); c.close(); } }); return JSON.stringify(await drain(rs.pipeThrough(new TextDecoderStream()))); });
  await t("TDS fatal", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([0xff])); c.close(); } }); const rd = rs.pipeThrough(new TextDecoderStream("utf-8", { fatal: true })).getReader(); try { return JSON.stringify(await rd.read()); } catch (e) { return "rej " + E(e); } });
  await t("TDS write non-buffer", async () => { const d = new TextDecoderStream(); const w = d.writable.getWriter(); const rd = d.readable.getReader(); const rp = rd.read().then(() => "readok", (e) => "readrej " + e.message); const wp = w.write("str").then(() => "ok", (e) => "rej " + E(e)); return (await Promise.all([wp, rp])).join(" | "); });
  await t("TDS async producer", async () => { const [rs, c] = ctl(); const rd = rs.pipeThrough(new TextDecoderStream()).getReader(); setTimeout(() => { c.enqueue(new TextEncoder().encode("late")); c.close(); }, 10); const a = await rd.read(); const b = await rd.read(); return JSON.stringify([a.value, b.done]); });
  await t("TES chunks", async () => { const rs = new ReadableStream({ start(c) { c.enqueue("é"); c.enqueue(""); c.enqueue(12); c.close(); } }); return JSON.stringify((await drain(rs.pipeThrough(new TextEncoderStream()))).map((u) => tag(u) + Array.from(u).join("."))); });
  await t("TES surrogate split", async () => { const rs = new ReadableStream({ start(c) { c.enqueue("\uD83D"); c.enqueue("\uDE00"); c.enqueue("\uD83D"); c.close(); } }); return JSON.stringify((await drain(rs.pipeThrough(new TextEncoderStream()))).map((u) => Array.from(u).join("."))); });
  await t("controller shape", () => { const [, c] = ctl(); return [typeof c.error, tag(c), c instanceof ReadableStreamDefaultController, c.desiredSize, Object.getOwnPropertyNames(c).length].join(","); });
  await t("controller desiredSize seq", () => { const [, c] = ctl(); const a = [c.desiredSize]; c.enqueue(1); a.push(c.desiredSize); c.enqueue(2); a.push(c.desiredSize); c.close(); a.push(c.desiredSize); return a.join(","); });
  await t("controller desiredSize errored", () => { const [, c] = ctl(); c.error(1); return String(c.desiredSize); });
  await t("enqueue after close", () => { const [, c] = ctl(); c.close(); c.enqueue(1); return "ok"; });
  await t("close after close", () => { const [, c] = ctl(); c.close(); c.close(); return "ok"; });
  await t("error after close", () => { const [, c] = ctl(); c.close(); c.error(new Error("x")); return "ok"; });
  await t("enqueue after error", () => { const [, c] = ctl(); c.error(new Error("x")); c.enqueue(1); return "ok"; });
  await t("close after error", () => { const [, c] = ctl(); c.error(new Error("x")); c.close(); return "ok"; });
  await t("error rejects parked read", async () => { const [rs, c] = ctl(); const p = rs.getReader().read(); c.error(new TypeError("boom")); try { await p; return "resolved"; } catch (e) { return "rejected " + e.message; } });
  await t("error drops queue", async () => { const [rs, c] = ctl(); c.enqueue(1); c.error(new Error("e2")); try { await rs.getReader().read(); return "resolved"; } catch (e) { return "rejected " + e.message; } });
  await t("cancel errored", async () => { const [rs, c] = ctl(); c.error(new Error("ce")); try { await rs.cancel(); return "resolved"; } catch (e) { return "rej " + e.message; } });
  await t("pipeTo live", async () => { const [rs, c] = ctl(); const got = []; const ws = new WritableStream({ write(x) { got.push(x); }, close() { got.push("close"); } }); const p = rs.pipeTo(ws); const l1 = [rs.locked, ws.locked]; c.enqueue("a"); c.enqueue("b"); c.close(); await p; return JSON.stringify([l1, got, rs.locked, ws.locked]); });
  await t("pipeTo errored source", async () => { const [rs, c] = ctl(); const got = []; const ws = new WritableStream({ write(x) { got.push(x); }, abort(r) { got.push("abort:" + r.message); } }); const p = rs.pipeTo(ws); c.enqueue("a"); c.error(new Error("pe")); try { await p; return "resolved " + got; } catch (e) { return "rej " + e.message + " " + got.join(";"); } });
  await t("pipeThrough bad writable", () => { const [rs] = ctl(); return rs.pipeThrough({ writable: {}, readable: new ReadableStream() }); });
  await t("pipeThrough locked source", () => { const [rs] = ctl(); rs.getReader(); return rs.pipeThrough(new TransformStream()); });
  await t("pipeTo locked source", async () => { const [rs] = ctl(); rs.getReader(); return rs.pipeTo(new WritableStream()); });
  await t("tee live",async () => { const [rs, c] = ctl(); const [a, b] = rs.tee(); setTimeout(() => { c.enqueue("x"); c.close(); }, 5); const ra = a.getReader(), rb = b.getReader(); return JSON.stringify([await ra.read(), await rb.read(), await ra.read(), await rb.read()]); });
  await t("writer write after close", async () => { const d = new TextDecoderStream(); const w = d.writable.getWriter(); await w.close(); try { await w.write(new Uint8Array(1)); return "ok"; } catch (e) { return "rej " + E(e); } });

  // ── Response / Request bodies ──
  await t("Response body tag, identity", () => { const r = new Response("hi"); return tag(r.body) + " " + (r.body === r.body); });
  await t("Response() / Response(null) body", () => String(new Response().body) + " " + String(new Response(null).body));
  await t("Response body read", async () => { const r = new Response("hi"); const rd = r.body.getReader(); const a = await rd.read(); const b = await rd.read(); return [tag(a.value), Array.from(a.value).join("."), a.done, b.done, String(b.value), r.bodyUsed].join("|"); });
  await t("bodyUsed after getReader", () => { const r = new Response("hi"); r.body.getReader(); return r.bodyUsed; });
  await t("bodyUsed after text", async () => { const r = new Response("hi"); const s = await r.text(); return [s, r.bodyUsed, r.body.locked].join("|"); });
  await t("bodyUsed null body text", async () => { const r = new Response(); return JSON.stringify(await r.text()) + " " + r.bodyUsed; });
  await t("text twice", async () => { const r = new Response("hi"); await r.text(); return await r.text(); });
  await t("getReader then text", async () => { const r = new Response("hi"); r.body.getReader(); return await r.text(); });
  await t("read then arrayBuffer", async () => { const r = new Response("x"); const rd = r.body.getReader(); await rd.read(); rd.releaseLock(); return await r.arrayBuffer(); });
  await t("clone both read", async () => { const r = new Response("hi"); const b = r.body; const c = r.clone(); return [await r.text(), await c.text(), r.bodyUsed, c.bodyUsed, b === r.body].join("|"); });
  await t("clone after used", async () => { const r = new Response("hi"); await r.text(); return r.clone(); });
  await t("clone while locked", () => { const r = new Response("x"); r.body.getReader(); return r.clone(); });
  await t("Response(ReadableStream)", async () => { const rs = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([104, 105])); c.close(); } }); const r = new Response(rs); return [r.body === rs, await r.text()].join("|"); });
  await t("Response(ReadableStream) late chunks", async () => { const [rs, c] = ctl(); setTimeout(() => { c.enqueue(new Uint8Array([111, 107])); c.close(); }, 10); return await new Response(rs).text(); });
  await t("Response(ReadableStream) non-bytes chunk", async () => new Response(new ReadableStream({ start(c) { c.enqueue("hi"); c.close(); } })).text());
  await t("Response(Uint8Array)", async () => { const r = new Response(new Uint8Array([1, 2, 255])); return new Uint8Array(await r.arrayBuffer()).join(".") + " " + r.headers.get("content-type"); });
  await t("Response(ArrayBuffer).blob()", async () => { const b = await new Response(new Uint8Array([1, 2, 255]).buffer, { headers: { "content-type": "X/Y" } }).blob(); return [b.size, b.type].join("|"); });
  await t("Response(Blob)", async () => { const r = new Response(new Blob([new Uint8Array([200, 201])], { type: "a/b" })); return new Uint8Array(await r.arrayBuffer()).join(".") + "|" + r.headers.get("content-type"); });
  await t("Response(URLSearchParams)", async () => { const r = new Response(new URLSearchParams("a=1&b=2")); return [await r.text(), r.headers.get("content-type")].join("|"); });
  await t("Response string content-type", () => new Response("x").headers.get("content-type") + " " + new Response("x", { headers: { "content-type": "a/b" } }).headers.get("content-type"));
  await t("Response bytes()", async () => { const u = await new Response("é").bytes(); return tag(u) + u.length; });
  await t("Response text BOM", async () => JSON.stringify(await new Response(new Uint8Array([0xef, 0xbb, 0xbf, 0x41])).text()));
  await t("Response json", async () => JSON.stringify(await new Response('{"a":1}').json()));
  await t("Response json bad", async () => new Response("{").json());
  await t("Response blob type", async () => (await new Response("x", { headers: { "Content-Type": "Text/HTML; Charset=UTF-8" } }).blob()).type);
  await t("Response 204 with body", () => new Response("x", { status: 204 }));
  await t("Response 204 null body", () => String(new Response(null, { status: 204 }).body));
  await t("Response status 99", () => new Response("x", { status: 99 }));
  await t("Request body tag", () => tag(new Request(BASE + "x", { method: "POST", body: "hi" }).body) + " " + String(new Request(BASE + "x").body));
  await t("Request text, bodyUsed", async () => { const r = new Request(BASE + "x", { method: "POST", body: "hi" }); return (await r.text()) + " " + r.bodyUsed; });
  await t("Request text twice", async () => { const r = new Request(BASE + "x", { method: "POST", body: "x" }); await r.text(); return await r.text(); });
  await t("Request GET with body", () => new Request(BASE + "x", { body: "x" }));
  await t("Request stream body without duplex", () => new Request(BASE + "x", { method: "POST", body: new ReadableStream() }));
  await t("Request clone", async () => { const r = new Request(BASE + "x", { method: "POST", body: "x", headers: { "x-a": "1" } }); const c = r.clone(); return [await r.text(), await c.text(), c.headers.get("x-a"), c.method].join("|"); });
  await t("Request clone used", async () => { const r = new Request(BASE + "x", { method: "POST", body: "x" }); await r.text(); return r.clone(); });
  await t("Request content-type", () => new Request(BASE + "x", { method: "POST", body: "x" }).headers.get("content-type") + " " + new Request(BASE + "x", { method: "POST", body: new Uint8Array(2) }).headers.get("content-type"));
  await t("Request from used Request", async () => { const r = new Request(BASE + "x", { method: "POST", body: "x" }); await r.text(); return new Request(r); });
  await t("Request from Request takes body", async () => { const r = new Request(BASE + "x", { method: "POST", body: "xy" }); const r2 = new Request(r); return [r.bodyUsed, await r2.text()].join("|"); });

  // ── fetch over the wire ──
  await t("fetch b.bin body reader", async () => { const r = await fetch(BASE + "b.bin"); const rd = r.body.getReader(); let n = 0, last; for (;;) { const x = await rd.read(); if (x.done) break; n += x.value.length; last = x.value[x.value.length - 1]; } return n + "/" + last + "/" + r.bodyUsed; });
  await t("fetch b.bin blob", async () => { const b = await (await fetch(BASE + "b.bin")).blob(); return b.size + "|" + b.type; });
  await t("fetch b.bin text", async () => { const s = await (await fetch(BASE + "b.bin")).text(); return s.length + "/" + s.charCodeAt(128); });
  await t("fetch latin.txt text", async () => { const s = await (await fetch(BASE + "latin.txt")).text(); return s.length + "/" + s.charCodeAt(3); });
  await t("fetch j.json json", async () => JSON.stringify(await (await fetch(BASE + "j.json")).json()));
  await t("fetch body pipeThrough TDS", async () => { const r = await fetch(BASE + "j.json"); return (await drain(r.body.pipeThrough(new TextDecoderStream()))).join(""); });
  await t("fetch clone both", async () => { const r = await fetch(BASE + "b.bin"); const c = r.clone(); return (await r.arrayBuffer()).byteLength + "|" + (await c.arrayBuffer()).byteLength; });
  await t("fetch text twice", async () => { const r = await fetch(BASE + "j.json"); await r.text(); return await r.text(); });
  await t("fetch 404 body", async () => { const r = await fetch(BASE + "nope"); return r.status + "|" + (r.body === null) + "|" + (await r.text()); });
  await t("fetch 204 body", async () => { const r = await fetch(BASE + "empty204"); return r.status + "|" + String(r.body); });
  await t("fetch POST Uint8Array", async () => { const r = await fetch(BASE + "echo", { method: "POST", body: new Uint8Array([0, 128, 255]) }); return new Uint8Array(await r.arrayBuffer()).join(".") + "|" + r.headers.get("x-ct"); });
  await t("fetch POST string", async () => { const r = await fetch(BASE + "echo", { method: "POST", body: "hé" }); return (await r.text()) + "|" + r.headers.get("x-ct"); });
  await t("fetch POST Headers instance", async () => { const r = await fetch(BASE + "echo", { method: "POST", body: "x", headers: new Headers({ "content-type": "a/b" }) }); return r.headers.get("x-ct"); });

  // ── XMLHttpRequest ──
  await t("XHR arraybuffer responseText", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "arraybuffer"; }); return x.responseText; });
  await t("XHR arraybuffer same object", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "arraybuffer"; }); return x.response === x.response; });
  await t("XHR blob", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "blob"; }); return tag(x.response) + " " + x.response.size + "|" + x.response.type; });
  await t("XHR blob override type", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "blob"; x.overrideMimeType("Text/Plain; charset=x-user-defined"); }); return x.response.type; });
  await t("XHR json bad", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "json"; }); return String(x.response); });
  await t("XHR json BOM", async () => { const x = await xhr((x) => { x.open("GET", BASE + "bom.json"); x.responseType = "json"; }); return JSON.stringify(x.response); });
  await t("XHR json same object", async () => { const x = await xhr((x) => { x.open("GET", BASE + "j.json"); x.responseType = "json"; }); return x.response === x.response; });
  await t("XHR json responseText", async () => { const x = await xhr((x) => { x.open("GET", BASE + "j.json"); x.responseType = "json"; }); return x.responseText; });
  await t("XHR document response", async () => { const x = await xhr((x) => { x.open("GET", BASE + "j.json"); x.responseType = "document"; }); return String(x.response); });
  await t("XHR text b.bin", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.responseType = "text"; }); return x.responseText.length + "/" + x.responseText.charCodeAt(128) + "/" + (x.response === x.responseText); });
  await t("XHR default latin.txt", async () => { const x = await xhr((x) => { x.open("GET", BASE + "latin.txt"); }); return x.responseText.length + "/" + x.responseText.charCodeAt(3); });
  await t("XHR x-user-defined", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.overrideMimeType("text/plain; charset=x-user-defined"); }); return [x.responseText.length, x.responseText.charCodeAt(128), x.responseText.charCodeAt(255), x.responseText.charCodeAt(65)].join("/"); });
  await t("XHR windows-1252 override", async () => { const x = await xhr((x) => { x.open("GET", BASE + "b.bin"); x.overrideMimeType("text/plain; charset=windows-1252"); }); return x.responseText.charCodeAt(128) + "/" + x.responseText.length; });
  await t("XHR UTF-16 BOM", async () => { const x = await xhr((x) => { x.open("GET", BASE + "u16.txt"); }); return JSON.stringify(x.responseText); });
  await t("XHR response before DONE", () => { const a = new XMLHttpRequest(); a.open("GET", BASE + "b.bin"); a.responseType = "arraybuffer"; const b = new XMLHttpRequest(); b.open("GET", BASE + "b.bin"); const c = new XMLHttpRequest(); c.open("GET", BASE + "b.bin"); c.responseType = "json"; return [String(a.response), JSON.stringify(b.response), String(c.response)].join("|"); });
  await t("XHR arraybuffer 404", async () => { const x = await xhr((x) => { x.open("GET", BASE + "nope"); x.responseType = "arraybuffer"; }); return tag(x.response) + x.response.byteLength; });
  await t("XHR responseText by state", () => new Promise((res) => { const x = new XMLHttpRequest(); x.open("GET", BASE + "j.json"); const seen = []; x.onreadystatechange = () => seen.push(x.readyState + ":" + x.responseText.length); x.onloadend = () => res(seen.join(",")); x.send(); }));
  await t("XHR send Uint8Array", () => new Promise((res) => { const x = new XMLHttpRequest(); x.open("POST", BASE + "echo"); x.responseType = "arraybuffer"; x.onloadend = () => res(new Uint8Array(x.response).join(".") + "|" + x.getResponseHeader("x-ct")); x.send(new Uint8Array([0, 128, 255])); }));
  await t("XHR send string", () => new Promise((res) => { const x = new XMLHttpRequest(); x.open("POST", BASE + "echo"); x.onloadend = () => res(x.responseText + "|" + x.getResponseHeader("x-ct")); x.send("hé"); }));
  await t("sync XHR events", () => { const ev = []; const x = new XMLHttpRequest(); x.onreadystatechange = () => ev.push("rs" + x.readyState); x.onloadstart = () => ev.push("ls"); x.onprogress = () => ev.push("p"); x.onload = () => ev.push("load"); x.onloadend = () => ev.push("le"); x.open("GET", BASE + "j.json", false); x.send(); ev.push("after:" + x.status + ":" + x.responseText); return ev.join(","); });
  await t("sync XHR responseType", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "b.bin", false); x.responseType = "arraybuffer"; return "ok"; });
  await t("sync XHR timeout", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "b.bin", false); x.timeout = 5; return "ok"; });
  await t("sync XHR open after responseType", () => { const x = new XMLHttpRequest(); x.responseType = "arraybuffer"; x.open("GET", BASE + "b.bin", false); return "ok"; });
  await t("sync XHR async undefined", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "b.bin", undefined); x.send(); return x.readyState; });
  await t("sync XHR 404", () => { const x = new XMLHttpRequest(); x.open("GET", BASE + "nope", false); x.send(); return x.status + "|" + x.responseText; });
  await t("sync XHR POST", () => { const x = new XMLHttpRequest(); x.open("POST", BASE + "echo", false); x.send("abc"); return x.status + "|" + x.responseText; });
  return R.join("\n");
})
