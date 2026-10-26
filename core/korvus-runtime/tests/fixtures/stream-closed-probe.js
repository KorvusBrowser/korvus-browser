// PARITY ST1 — the `closed` promise on `ReadableStreamDefaultReader` and `WritableStreamDefaultWriter`.
// Evaluated UNMODIFIED by `tests/stream_closed_live.rs` and by `google-chrome` 150.0.7871.46 over CDP,
// so the two outputs diff line for line. `stream-closed-probe.chrome-150.txt` is Chrome's output,
// RECORDED (two runs, identical), not written. A line that differs is a finding about the engine,
// never a reason to edit the file.
//
// NOTHING HERE TOUCHES THE NETWORK: `BASE` is taken and ignored, the same shape the other
// `*-probe.js` fixtures beside it use.
//
// WHAT IT ASKS. For a reader and for a writer, the same four transitions -- close, cancel/abort,
// error, releaseLock -- plus the orderings a consumer can actually observe (the read/write promise
// against `closed`), plus what `releaseLock` does to a `closed` that has ALREADY settled, plus WHEN
// the underlying sink runs relative to the `write()` call that fed it.
//
// WHAT IS DELIBERATELY ABSENT, because it measures something else:
//   * backpressure. Chrome's `writer.ready` gates on the queue's desired size and this engine models
//     no queue, so the `ready` lines below ask only what `ready` does once the stream is CLOSED or
//     ERRORED -- a lifecycle question, not a queuing one.
//   * whether a `closed` rejection is marked handled. That is not observable from script at all; it
//     is observable from the host, and the engine test reads it there instead.
(async (BASE) => {
  const R = [];
  const E = (e) =>
    e && e.name ? e.name + ": " + e.message : Object.prototype.toString.call(e) + " " + String(e);
  const TMO = (ms) => new Promise((r) => setTimeout(() => r("PENDING"), ms));
  // Settle `p`, or report it still pending after `ms`. Every `closed` read goes through this, so a
  // promise that never settles is a LINE rather than a hang -- which is the whole question here.
  const S = (p, ms) =>
    Promise.race([
      Promise.resolve(p).then(
        (v) => "fulfilled " + (v === undefined ? "undefined" : String(v)),
        (e) => "rejected " + E(e),
      ),
      TMO(ms === undefined ? 300 : ms),
    ]);
  const t = async (name, f) => {
    try {
      R.push(name + " = " + (await Promise.race([Promise.resolve().then(f), TMO(20000)])));
    } catch (e) {
      R.push(name + " = THREW " + E(e));
    }
  };
  const desc = (o, k) => {
    const d = Object.getOwnPropertyDescriptor(o, k);
    if (!d) return "<absent>";
    return (
      (d.get ? "get:fn" : "value:" + typeof d.value) +
      ",set:" + (d.set ? "fn" : String(d.set)) +
      ",enumerable:" + d.enumerable +
      ",configurable:" + d.configurable
    );
  };
  // A readable whose controller this code keeps, so the probe can close or error it at will.
  const mk = () => {
    let c;
    const rs = new ReadableStream({ start(ctl) { c = ctl; } });
    return { rs, c };
  };

  // ── shape ─────────────────────────────────────────────────────────────────────────────────────
  await t("RSDR proto closed desc", () => desc(ReadableStreamDefaultReader.prototype, "closed"));
  await t("WSDW proto closed desc", () => desc(WritableStreamDefaultWriter.prototype, "closed"));
  await t("RSDR proto own names", () => Object.getOwnPropertyNames(ReadableStreamDefaultReader.prototype).join(","));
  await t("WSDW proto own names", () => Object.getOwnPropertyNames(WritableStreamDefaultWriter.prototype).join(","));
  await t("reader.closed typeof", () => typeof mk().rs.getReader().closed);
  await t("reader.closed instanceof Promise", () => mk().rs.getReader().closed instanceof Promise);
  await t("reader.closed is stable", () => { const r = mk().rs.getReader(); return r.closed === r.closed; });
  await t("writer.closed typeof", () => typeof new WritableStream().getWriter().closed);
  await t("writer.closed is stable", () => { const w = new WritableStream().getWriter(); return w.closed === w.closed; });
  await t("writer.ready is stable", () => { const w = new WritableStream().getWriter(); return w.ready === w.ready; });
  // NOT ASKED HERE, deliberately: the foreign-receiver message. Chrome 150 answers
  //   get ReadableStreamDefaultReader.prototype.closed .call({})
  //     TypeError: Failed to read the 'closed' property from 'ReadableStreamDefaultReader': Illegal invocation
  // and this engine answers the bare `TypeError: Illegal invocation` that
  // `install/receiver.rs:163-165` gives EVERY guarded row. That is the shared installer's sentence
  // for 766 of the 832 receiver answers the bundle records -- and Chrome itself is not uniform about
  // it (`compression-probe.chrome-150.txt` records `CompressionStream.prototype.readable` answering
  // the BARE sentence on the same browser). Changing it is a change to the receiver guard for every
  // interface in the surface, on a rule nobody has measured yet; it is not this lane's, so it is
  // recorded here rather than asserted.

  // ── reader.closed: the four transitions ───────────────────────────────────────────────────────
  await t("reader.closed while the stream is live", () => S(mk().rs.getReader().closed));
  await t("reader.closed after controller.close()", () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.close(); return S(r.closed);
  });
  await t("reader.closed after close with a chunk still queued", () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.enqueue("a"); c.close(); return S(r.closed);
  });
  await t("reader.closed after the last chunk of a closed stream is read", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.enqueue("a"); c.close();
    await r.read(); return S(r.closed);
  });
  await t("reader.closed after controller.error(e)", () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.error(new TypeError("boom")); return S(r.closed);
  });
  await t("reader.closed after controller.error() with no reason", () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.error(); return S(r.closed);
  });
  await t("reader.closed after reader.cancel()", () => {
    const { rs } = mk(); const r = rs.getReader(); r.cancel("why"); return S(r.closed);
  });
  await t("reader.closed after reader.cancel() on a queued stream", () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.enqueue("a"); r.cancel(); return S(r.closed);
  });
  await t("reader.closed after releaseLock while live", () => {
    const { rs } = mk(); const r = rs.getReader(); r.releaseLock(); return S(r.closed);
  });
  await t("reader.closed identity survives releaseLock", async () => {
    const { rs } = mk(); const r = rs.getReader(); const before = r.closed; r.releaseLock();
    return (before === r.closed) + " / " + (await S(before)) + " / " + (await S(r.closed));
  });
  await t("reader.closed after releaseLock on an ALREADY closed stream", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.close(); await r.closed;
    const before = r.closed; r.releaseLock();
    return (before === r.closed) + " / " + (await S(r.closed));
  });
  await t("reader.closed after releaseLock on an ALREADY errored stream", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); c.error(new TypeError("boom"));
    try { await r.closed; } catch (e) { /* expected */ }
    const before = r.closed; r.releaseLock();
    return (before === r.closed) + " / " + (await S(r.closed));
  });
  await t("a reader taken from an already-closed stream", () => {
    const { rs, c } = mk(); c.close(); return S(rs.getReader().closed);
  });
  await t("a reader taken from an already-errored stream", () => {
    const { rs, c } = mk(); c.error(new TypeError("boom")); return S(rs.getReader().closed);
  });
  await t("reader.closed when the stream errors AFTER releaseLock", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); r.releaseLock();
    c.error(new TypeError("late boom"));
    return S(r.closed);
  });
  await t("a SECOND reader after releaseLock gets its own closed", async () => {
    const { rs, c } = mk(); const a = rs.getReader(); a.releaseLock();
    const b = rs.getReader();
    const same = a.closed === b.closed;
    c.close();
    return "same=" + same + " / a " + (await S(a.closed)) + " / b " + (await S(b.closed));
  });

  // ── writer.closed: the four transitions ───────────────────────────────────────────────────────
  await t("writer.closed while the stream is live", () => S(new WritableStream().getWriter().closed));
  await t("writer.ready while the stream is live", () => S(new WritableStream().getWriter().ready));
  await t("writer.ready after close", async () => {
    const w = new WritableStream().getWriter(); await w.close(); return S(w.ready);
  });
  await t("writer.closed after writer.close()", () => {
    const w = new WritableStream().getWriter(); w.close(); return S(w.closed);
  });
  await t("writer.closed after writer.abort(reason)", () => {
    const w = new WritableStream().getWriter(); w.abort("why").catch(() => {}); return S(w.closed);
  });
  await t("writer.closed after writer.abort() with no reason", () => {
    const w = new WritableStream().getWriter(); w.abort().catch(() => {}); return S(w.closed);
  });
  await t("writer.closed after a sink write THROWS", () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("x").catch(() => {}); return S(w.closed);
  });
  await t("writer.closed after a sink close THROWS", () => {
    const ws = new WritableStream({ close() { throw new TypeError("close boom"); } });
    const w = ws.getWriter(); w.close().catch(() => {}); return S(w.closed);
  });
  await t("writer.closed after a sink write rejects", () => {
    const ws = new WritableStream({ write() { return Promise.reject(new TypeError("async boom")); } });
    const w = ws.getWriter(); w.write("x").catch(() => {}); return S(w.closed);
  });
  await t("writer.closed after releaseLock while live", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return S(w.closed);
  });
  await t("writer.closed identity survives releaseLock", async () => {
    const w = new WritableStream().getWriter(); const before = w.closed; w.releaseLock();
    return (before === w.closed) + " / " + (await S(before)) + " / " + (await S(w.closed));
  });
  await t("writer.closed after releaseLock on an ALREADY closed stream", async () => {
    const w = new WritableStream().getWriter(); await w.close();
    const before = w.closed; w.releaseLock();
    return (before === w.closed) + " / " + (await S(w.closed));
  });
  await t("a writer taken from an already-closed stream", async () => {
    const ws = new WritableStream(); const a = ws.getWriter(); await a.close(); a.releaseLock();
    try { return await S(ws.getWriter().closed); } catch (e) { return "getWriter THREW " + E(e); }
  });
  await t("a writer taken from an already-errored stream", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const a = ws.getWriter(); a.write("x").catch(() => {}); await S(a.closed); a.releaseLock();
    try { return await S(ws.getWriter().closed); } catch (e) { return "getWriter THREW " + E(e); }
  });

  // ── what the OTHER members answer once the stream is errored, closed or released ──────────────
  await t("writer.write after the stream errored", async () => {
    const ws = new WritableStream({ write(c) { if (c === "bad") throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("bad").catch(() => {}); await S(w.closed);
    return S(w.write("ok"));
  });
  await t("writer.close after the stream errored", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("bad").catch(() => {}); await S(w.closed);
    return S(w.close());
  });
  await t("writer.abort after the stream errored", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("bad").catch(() => {}); await S(w.closed);
    return S(w.abort("late"));
  });
  await t("writer.ready after the stream errored", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("bad").catch(() => {}); await S(w.closed);
    return S(w.ready);
  });
  await t("writer.desiredSize after the stream errored", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); w.write("bad").catch(() => {}); await S(w.closed);
    return String(w.desiredSize);
  });
  await t("writer.write after close", async () => {
    const w = new WritableStream().getWriter(); await w.close(); return S(w.write("x"));
  });
  await t("writer.desiredSize after close", async () => {
    const w = new WritableStream().getWriter(); await w.close(); return String(w.desiredSize);
  });
  await t("writer.desiredSize after releaseLock", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return String(w.desiredSize);
  });
  await t("writer.write after releaseLock", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return S(w.write("x"));
  });
  await t("writer.close after releaseLock", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return S(w.close());
  });
  await t("writer.ready after releaseLock", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return S(w.ready);
  });
  // The IDENTITY of the rejection, not just its sentence: `streams/writable-streams/general.any.js`
  // asserts `assert_equals(readyRejection, closedRejection)`, and two TypeErrors carrying the same
  // text read identically in a log while failing that.
  // BOTH promises are read AFTER the release, which is the only way the question is well posed: a
  // live writer's `ready` is already FULFILLED, so a `.catch` attached before the release never
  // fires and the comparison is against `undefined` rather than against the other rejection.
  await t("releaseLock rejects closed and ready with the SAME object", async () => {
    const w = new WritableStream().getWriter();
    w.releaseLock();
    const ce = await w.closed.catch((e) => e);
    const re = await w.ready.catch((e) => e);
    return (ce === re) + " / " + E(ce);
  });
  await t("writer.abort after releaseLock", () => {
    const w = new WritableStream().getWriter(); w.releaseLock(); return S(w.abort("x"));
  });
  await t("stream.close() while a writer holds it", () => {
    const ws = new WritableStream(); ws.getWriter();
    try { return S(ws.close()); } catch (e) { return "THREW " + E(e); }
  });
  await t("stream.abort() while a writer holds it", () => {
    const ws = new WritableStream(); ws.getWriter();
    try { return S(ws.abort("x")); } catch (e) { return "THREW " + E(e); }
  });
  await t("reader.read after releaseLock", () => {
    const { rs } = mk(); const r = rs.getReader(); r.releaseLock(); return S(r.read());
  });
  await t("reader.cancel after releaseLock", () => {
    const { rs } = mk(); const r = rs.getReader(); r.releaseLock(); return S(r.cancel());
  });
  await t("a read PARKED when releaseLock runs", () => {
    const { rs } = mk(); const r = rs.getReader(); const p = r.read(); r.releaseLock(); return S(p);
  });

  // ── ordering, which is the part a consumer can actually observe ───────────────────────────────
  await t("order: read vs closed on close", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); const log = [];
    const a = r.read().then(() => log.push("read"), () => log.push("read-rej"));
    const b = r.closed.then(() => log.push("closed"), () => log.push("closed-rej"));
    c.close();
    await Promise.all([a, b]);
    return log.join(",");
  });
  await t("order: read vs closed on error", async () => {
    const { rs, c } = mk(); const r = rs.getReader(); const log = [];
    const a = r.read().then(() => log.push("read"), () => log.push("read-rej"));
    const b = r.closed.then(() => log.push("closed"), () => log.push("closed-rej"));
    c.error(new TypeError("boom"));
    await Promise.all([a, b]);
    return log.join(",");
  });
  await t("order: read vs closed on releaseLock", async () => {
    const { rs } = mk(); const r = rs.getReader(); const log = [];
    const a = r.read().then(() => log.push("read"), () => log.push("read-rej"));
    const b = r.closed.then(() => log.push("closed"), () => log.push("closed-rej"));
    r.releaseLock();
    await Promise.all([a, b]);
    return log.join(",");
  });
  await t("order: write vs closed on a throwing sink", async () => {
    const ws = new WritableStream({ write() { throw new TypeError("sink boom"); } });
    const w = ws.getWriter(); const log = [];
    const a = w.write("x").then(() => log.push("write"), () => log.push("write-rej"));
    const b = w.closed.then(() => log.push("closed"), () => log.push("closed-rej"));
    await Promise.all([a, b]);
    return log.join(",");
  });
  await t("order: close vs closed", async () => {
    const w = new WritableStream().getWriter(); const log = [];
    const a = w.close().then(() => log.push("close"), () => log.push("close-rej"));
    const b = w.closed.then(() => log.push("closed"), () => log.push("closed-rej"));
    await Promise.all([a, b]);
    return log.join(",");
  });

  // ── WHEN the sink runs: synchronously inside write(), or a microtask later ────────────────────
  await t("sink write runs sync or deferred", async () => {
    const log = [];
    const ws = new WritableStream({ write() { log.push("sink"); } });
    const w = ws.getWriter();
    log.push("before");
    w.write("x");
    log.push("after");
    await Promise.resolve();
    log.push("microtask1");
    await Promise.resolve();
    log.push("microtask2");
    await new Promise((r) => setTimeout(r, 0));
    log.push("task");
    return log.join(",");
  });
  await t("sink close runs sync or deferred", async () => {
    const log = [];
    const ws = new WritableStream({ close() { log.push("sink-close"); } });
    const w = ws.getWriter();
    log.push("before");
    w.close();
    log.push("after");
    await new Promise((r) => setTimeout(r, 0));
    log.push("task");
    return log.join(",");
  });
  // NOT ASKED HERE, and the reason is the same one that keeps three `writable-streams` WPT rows
  // red. Chrome 150 defers the sink's `abort` too --
  //   `before,after,sink-abort,task`  against this engine's `before,sink-abort,after,task`
  // -- but it cannot simply be deferred here, because WHICH of `close()` and `abort()` wins when
  // both are issued in one turn is decided by Chrome's QUEUE: an abort raised while a close is
  // merely QUEUED errors the stream and the close rejects (the `order: close() then abort()` line
  // below, which this engine reproduces), while an abort raised while the close is already IN
  // FLIGHT waits for it, so the close resolves first (WPT's `writer close() promise should resolve
  // before abort() promise`, which awaits `writer.ready` so that `start` has finished). This engine
  // models no queue, no in-flight operation and no `start`, so it can answer one of those two and
  // not both; it answers the one `closed` turns on. Deferring the abort sink here would flip the
  // line below from Chrome's answer to the other one and gain nothing.
  await t("order: close() then abort()", async () => {
    const w = new WritableStream().getWriter();
    const log = [];
    const c = w.close().then(() => log.push("close"), (e) => log.push("close-rej " + E(e)));
    const a = w.abort().then(() => log.push("abort"), (e) => log.push("abort-rej " + E(e)));
    await Promise.all([c, a]);
    return log.join(",") + " | closed " + (await S(w.closed));
  });
  await t("writer.abort after the stream closed", async () => {
    const log = [];
    const ws = new WritableStream({ abort() { log.push("sink-abort"); } });
    const w = ws.getWriter();
    await w.close();
    return (await S(w.abort("late"))) + " | sink " + (log.join(",") || "<not called>") + " | closed " + (await S(w.closed));
  });
  await t("two writes reach the sink in order", async () => {
    const log = [];
    const ws = new WritableStream({ write(c) { log.push(String(c)); } });
    const w = ws.getWriter();
    w.write("1"); w.write("2"); w.write("3");
    await new Promise((r) => setTimeout(r, 0));
    return log.join(",");
  });
  await t("a chunk written before the read is issued still reaches it", async () => {
    const ts = new TransformStream();
    const w = ts.writable.getWriter();
    w.write("hello").catch(() => {});
    const rd = ts.readable.getReader();
    return Promise.race([rd.read().then((r) => "value " + r.value, (e) => "rej " + E(e)), TMO(500)]);
  });

  // ── the two WPT shapes this lane exists for ───────────────────────────────────────────────────
  for (const fmt of ["deflate", "gzip", "deflate-raw"]) {
    await t("bad chunk (undefined) on " + fmt, async () => {
      const ds = new DecompressionStream(fmt);
      const reader = ds.readable.getReader();
      const writer = ds.writable.getWriter();
      writer.write(undefined).then(() => {}, () => {});
      reader.read().then(() => {}, () => {});
      return [
        "close " + (await S(writer.close())),
        "w.closed " + (await S(writer.closed)),
        "read " + (await S(reader.read())),
        "r.closed " + (await S(reader.closed)),
      ].join(" | ");
    });
    await t("bad chunk (invalid bytes) on " + fmt, async () => {
      const ds = new DecompressionStream(fmt);
      const reader = ds.readable.getReader();
      const writer = ds.writable.getWriter();
      writer.write(new Uint8Array([0, 156, 75, 173, 40, 72, 77, 46, 73, 77, 81, 200, 47, 45, 41, 40, 45, 1, 0, 48, 173, 6, 36])).then(() => {}, () => {});
      reader.read().then(() => {}, () => {});
      return [
        "close " + (await S(writer.close())),
        "w.closed " + (await S(writer.closed)),
        "read " + (await S(reader.read())),
        "r.closed " + (await S(reader.closed)),
      ].join(" | ");
    });
  }
  await t("extra input after a complete gzip member", async () => {
    const good = await (async () => {
      const cs = new CompressionStream("gzip");
      const w = cs.writable.getWriter();
      const rd = cs.readable.getReader();
      const out = [];
      const pump = (async () => { for (;;) { const r = await rd.read(); if (r.done) break; out.push(...r.value); } })();
      await w.write(new TextEncoder().encode("expected output"));
      await w.close();
      await pump;
      return out;
    })();
    const ds = new DecompressionStream("gzip");
    const reader = ds.readable.getReader();
    const writer = ds.writable.getWriter();
    writer.write(new Uint8Array([...good, 0])).catch(() => {});
    const first = await Promise.race([
      reader.read().then((r) => "value " + new TextDecoder().decode(r.value), (e) => "rej " + E(e)),
      TMO(500),
    ]);
    return first + " | second " + (await S(reader.read()));
  });
  await t("a stream that closes cleanly settles both closeds", async () => {
    const cs = new CompressionStream("gzip");
    const w = cs.writable.getWriter();
    const rd = cs.readable.getReader();
    const pump = (async () => { for (;;) { const r = await rd.read(); if (r.done) break; } })();
    await w.write(new TextEncoder().encode("hi"));
    await w.close();
    await pump;
    return "w.closed " + (await S(w.closed)) + " | r.closed " + (await S(rd.closed));
  });

  return R.join("\n");
})
