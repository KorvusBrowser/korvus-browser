// PARITY MP1 — `MessagePort.postMessage`'s TRANSFER LIST, and what a transferred port arrives as.
// Evaluated UNMODIFIED by `tests/message_port_transfer_live.rs` and by `google-chrome` 150.0.7871.46
// over CDP, so the two outputs diff line for line. `message-port-probe.chrome-150.txt` is Chrome's
// output, RECORDED (two runs, identical), not written. A line that differs is a finding about the
// engine, never a reason to edit the file.
//
// NOTHING HERE TOUCHES THE NETWORK: a `MessageChannel` is entirely in-realm, so `BASE` is taken and
// ignored, and this file shares the shape of the other `*-probe.js` fixtures beside it.
//
// WHAT IS DELIBERATELY ABSENT, because it measures something else:
//   * the CROSS-ISOLATE (`Worker`) transfer path. It needs a worker script URL and therefore a
//     server, and this engine's worker port plane is a different mechanism from the same-agent one
//     (`PortForward` / `op_port_deliver_external`). It is measured by its own test, not here.
//   * anything keyed to a site. The live cluster this fixture exists for was found on one vendor's
//     script; the RULE is `MessagePort`'s, and only the rule is asserted.
//   * `kEv.ports[0] instanceof MessagePort` in group K. Chrome answers FALSE -- the port that lands
//     in a nested browsing context belongs to THAT context's realm, so the parent's interface object
//     is not on its chain. Korvus gives a nested context the parent's realm (one set of interface
//     objects, `browsing_context.rs`), so it must answer true, and that is a finding about FRAME
//     REALMS and not about the transfer list. Measuring it here would make this fixture about two
//     defects and let the second one hold the first hostage. Recorded in MP1's report instead.
//   * whether a DOM node is clonable AT ALL. Chrome refuses one ("HTMLDivElement object could not be
//     cloned."); this engine's `structuredClone` deliberately copies an object with its PROTOTYPE so
//     a platform object survives `instanceof` (`modules/structured_clone.rs`'s own header says so),
//     and teaching it which objects are host objects is that module's lane, not the transfer list's.
//     What IS asserted here is the SENTENCE a refusal carries, on the values both engines refuse.
(async (BASE) => {
  void BASE;
  const R = [];
  const P = (k, v) => R.push(k + " = " + v);
  const E = (e) => "THREW " + (e && e.name) + ": " + (e && e.message);
  // Every reading goes through here, so a throw is a RECORDED LINE rather than a lost probe.
  const t = async (name, f) => { try { P(name, await f()); } catch (e) { P(name, E(e)); } };
  const TMO = (ms) => new Promise((r) => setTimeout(() => r("TIMEOUT"), ms));
  const tag = (v) => (v === null ? "null" : v === undefined ? "undefined" : Object.prototype.toString.call(v));
  // The next `message` event on `port`. Assigning `onmessage` is itself the implicit `start()`.
  const evOnce = (port) => new Promise((res) => { port.onmessage = (e) => res(e); });
  const first = (port, ms) => Promise.race([evOnce(port), TMO(ms || 5000)]);
  const shape = (pre, e) => {
    if (e === "TIMEOUT") { P(pre + " event", "TIMEOUT"); return; }
    P(pre + " data", String(e.data));
    P(pre + " ports tag", tag(e.ports));
    P(pre + " ports isArray", Array.isArray(e.ports));
    P(pre + " ports length", e.ports.length);
    P(pre + " ports frozen", Object.isFrozen(e.ports));
    P(pre + " ports stable ref", e.ports === e.ports);
  };

  // ── A. a port transferred through a port ──────────────────────────────────────────────────────
  const wire = new MessageChannel();
  const car = new MessageChannel();
  const aGot = first(wire.port2);
  await t("A post returns", () => String(wire.port1.postMessage("A", [car.port1])));
  const aEv = await aGot;
  shape("A", aEv);
  await t("A ports[0] tag", () => tag(aEv.ports[0]));
  await t("A ports[0] instanceof MessagePort", () => aEv.ports[0] instanceof MessagePort);
  await t("A ports[0] is the carrier object", () => aEv.ports[0] === car.port1);
  await t("A ports[0] is the carrier peer", () => aEv.ports[0] === car.port2);
  await t("A ports[0] ctor name", () => aEv.ports[0].constructor.name);
  await t("A ports[0] own names", () => Object.getOwnPropertyNames(aEv.ports[0]).join(","));
  await t("A event origin", () => JSON.stringify(aEv.origin));
  await t("A event source", () => String(aEv.source));
  await t("A event lastEventId", () => JSON.stringify(aEv.lastEventId));
  // The whole point: the arrived port is live against the carrier's OTHER end.
  const aBack = aEv !== "TIMEOUT" && aEv.ports[0] ? first(car.port2) : Promise.resolve("NO PORT");
  await t("A roundtrip through arrived port", () => {
    if (aEv === "TIMEOUT" || !aEv.ports[0]) return "NO PORT";
    aEv.ports[0].postMessage("A-pong");
    return aBack.then((x) => (typeof x === "string" ? x : String(x.data)));
  });
  // ... and the carrier the sender held is DEAD: a post on it is a silent no-op, not a throw.
  await t("A post on transferred port", () => { car.port1.postMessage("dead"); return "no throw"; });

  // ── B. no transfer list, and an EMPTY one ─────────────────────────────────────────────────────
  const b1 = first(wire.port2);
  wire.port1.postMessage("B1");
  shape("B1", await b1);
  const b2 = first(wire.port2);
  wire.port1.postMessage("B2", []);
  shape("B2", await b2);
  const b3 = first(wire.port2);
  wire.port1.postMessage("B3", { transfer: [] });
  shape("B3", await b3);

  // ── C. the error cases, with Chrome's exact type and sentence ─────────────────────────────────
  const c = new MessageChannel();
  await t("C source port in its own list", () => { c.port1.postMessage("x", [c.port1]); return "no throw"; });
  await t("C entangled peer in the list", () => { const k = new MessageChannel(); k.port1.postMessage("x", [k.port2]); return "no throw"; });
  // Transferring the very port this message is ADDRESSED to. Chrome takes it, and the message must
  // still arrive -- at the port that replaced it, with itself in `event.ports`.
  await t("C the message addressed to a transferred peer still arrives", () => {
    const k = new MessageChannel();
    const seen = new Promise((res) => { k.port2.onmessage = (e) => res("wrong side"); });
    k.port1.postMessage("self", [k.port2]);
    return Promise.race([seen, TMO(300).then(() => "not on the old port")]);
  });
  await t("C ... and arrives on the port that replaced it", () => {
    const k = new MessageChannel();
    const w = new MessageChannel();
    const got = first(w.port2, 3000);
    w.port1.postMessage("carrier", [k.port2]);
    return got.then((e) => {
      if (e === "TIMEOUT" || !e.ports[0]) return "NO PORT";
      const inner = new Promise((res) => { e.ports[0].onmessage = (m) => res(String(m.data)); });
      k.port1.postMessage("to-the-replacement");
      return Promise.race([inner, TMO(2000)]);
    });
  });
  await t("C already-transferred port", () => { c.port1.postMessage("x", [car.port1]); return "no throw"; });
  await t("C same port twice in one list", () => { const k = new MessageChannel(); c.port1.postMessage("x", [k.port1, k.port1]); return "no throw"; });
  await t("C plain object in the list", () => { c.port1.postMessage("x", [{}]); return "no throw"; });
  await t("C null in the list", () => { c.port1.postMessage("x", [null]); return "no throw"; });
  await t("C number in the list", () => { c.port1.postMessage("x", [7]); return "no throw"; });
  await t("C transfer list is a number", () => { c.port1.postMessage("x", 7); return "no throw"; });
  await t("C transfer list is null", () => { c.port1.postMessage("x", null); return "no throw"; });
  await t("C transfer list is undefined", () => { c.port1.postMessage("x", undefined); return "no throw"; });
  await t("C transfer list is a string", () => { c.port1.postMessage("x", "nope"); return "no throw"; });
  await t("C no arguments", () => { c.port1.postMessage(); return "no throw"; });
  await t("C unclonable data", () => { c.port1.postMessage(function () {}); return "no throw"; });
  await t("C unclonable data with a port in the list", () => { const k = new MessageChannel(); c.port1.postMessage(Symbol("s"), [k.port1]); return "no throw"; });
  await t("C detached ArrayBuffer in the list", () => {
    const ab = new ArrayBuffer(8);
    c.port1.postMessage("first", [ab]);
    c.port1.postMessage("again", [ab]);
    return "no throw";
  });
  await t("C illegal receiver", () => { MessagePort.prototype.postMessage.call({}, "x"); return "no throw"; });
  await t("C closed port in the list", () => { const k = new MessageChannel(); k.port1.close(); c.port1.postMessage("x", [k.port1]); return "no throw"; });
  await t("C post on a closed port", () => { const k = new MessageChannel(); k.port1.close(); k.port1.postMessage("x"); return "no throw"; });
  // A THROWN call must not have transferred anything: the port named in a failed list is still live.
  const cLive = new MessageChannel();
  await t("C after a throw the named port is still usable", () => {
    try { c.port1.postMessage("x", [cLive.port1, cLive.port1]); } catch (e) { void e; }
    cLive.port1.postMessage("still alive");
    return "no throw";
  });

  // ── C2. the rest of the argument surface, and WHICH error wins ───────────────────────────────
  const c2 = new MessageChannel();
  await t("C2 transfer list is true", () => { c2.port1.postMessage("x", true); return "no throw"; });
  await t("C2 second argument is a bare object", () => { c2.port1.postMessage("x", {}); return "no throw"; });
  await t("C2 options transfer is a number", () => { c2.port1.postMessage("x", { transfer: 5 }); return "no throw"; });
  await t("C2 options transfer holds a plain object", () => { c2.port1.postMessage("x", { transfer: [{}] }); return "no throw"; });
  await t("C2 undefined in the list", () => { c2.port1.postMessage("x", [undefined]); return "no throw"; });
  await t("C2 duplicate ArrayBuffer", () => { const ab = new ArrayBuffer(8); c2.port1.postMessage("x", [ab, ab]); return "no throw"; });
  await t("C2 bad value after a good port", () => { const k = new MessageChannel(); c2.port1.postMessage("x", [k.port1, {}]); return "no throw"; });
  await t("C2 an Element in the list", () => { c2.port1.postMessage("x", [document.createElement("div")]); return "no throw"; });
  await t("C2 unclonable data AND a bad list", () => { c2.port1.postMessage(function () {}, [{}]); return "no throw"; });
  const c2Live = new MessageChannel();
  await t("C2 a clone throw leaves the listed port alive", () => {
    try { c2.port1.postMessage(Symbol("z"), [c2Live.port1]); } catch (e) { void e; }
    c2Live.port1.postMessage("still alive");
    return "no throw";
  });
  await t("C2 nested function", () => { c2.port1.postMessage({ a: function () {} }); return "no throw"; });
  await t("C2 nested symbol", () => { c2.port1.postMessage({ a: Symbol("q") }); return "no throw"; });
  await t("C2 structuredClone of a function", () => { structuredClone(function () {}); return "no throw"; });
  await t("C2 structuredClone of a nested function", () => { structuredClone({ a: function () {} }); return "no throw"; });
  await t("C2 structuredClone of a symbol", () => { structuredClone(Symbol("w")); return "no throw"; });
  // The transfer list is validated BEFORE anything is neutered, so a call that throws leaves every
  // port in it usable -- which is the only reason a page can retry.
  const c2Order = new MessageChannel();
  await t("C2 a list-error leaves an earlier valid port alive", () => {
    try { c2.port1.postMessage("x", [c2Order.port1, {}]); } catch (e) { void e; }
    const k = new MessageChannel();
    k.port1.postMessage("probe", [c2Order.port1]);
    return "no throw";
  });

  // ── D. a transferred port carries the messages queued on it ───────────────────────────────────
  const d = new MessageChannel();
  const dCar = new MessageChannel();
  dCar.port2.postMessage("D-1");
  dCar.port2.postMessage("D-2");
  const dGot = first(d.port2);
  d.port1.postMessage("D", [dCar.port1]);
  const dEv = await dGot;
  await t("D arrived ports length", () => (dEv === "TIMEOUT" ? "TIMEOUT" : dEv.ports.length));
  const dSeen = [];
  await t("D queued messages survive the transfer", () => {
    if (dEv === "TIMEOUT" || !dEv.ports[0]) return "NO PORT";
    const done = new Promise((res) => {
      dEv.ports[0].onmessage = (e) => { dSeen.push(String(e.data)); if (dSeen.length === 2) res("ok"); };
    });
    return Promise.race([done, TMO(2000)]);
  });
  await t("D queued messages in order", () => dSeen.join(","));

  // ── E. delivery ordering versus `start()` ─────────────────────────────────────────────────────
  const e1 = new MessageChannel();
  const eSeen = [];
  e1.port2.addEventListener("message", (ev) => eSeen.push(String(ev.data)));
  e1.port1.postMessage("E-1");
  e1.port1.postMessage("E-2");
  await t("E addEventListener alone does not start", () => TMO(200).then(() => eSeen.join(",") || "(nothing)"));
  e1.port2.start();
  await t("E after start", () => TMO(200).then(() => eSeen.join(",")));
  e1.port1.postMessage("E-3");
  await t("E after start, later post", () => TMO(200).then(() => eSeen.join(",")));
  const e2 = new MessageChannel();
  const e2Seen = [];
  e2.port1.postMessage("F-1");
  e2.port2.onmessage = (ev) => e2Seen.push(String(ev.data));
  await t("E onmessage setter starts the port", () => TMO(200).then(() => e2Seen.join(",") || "(nothing)"));
  const e3 = new MessageChannel();
  await t("E start() is idempotent", () => { e3.port1.start(); e3.port1.start(); return "no throw"; });

  // ── F. `window.postMessage`, the same rule on the document plane ──────────────────────────────
  const fCar = new MessageChannel();
  const fGot = new Promise((res) => {
    const h = (ev) => { if (ev.data === "F") { window.removeEventListener("message", h); res(ev); } };
    window.addEventListener("message", h);
  });
  window.postMessage("F", "*", [fCar.port1]);
  const fEv = await Promise.race([fGot, TMO(5000)]);
  shape("F", fEv);
  await t("F ports[0] is the carrier object", () => (fEv === "TIMEOUT" ? "TIMEOUT" : fEv.ports[0] === fCar.port1));
  await t("F ports[0] instanceof MessagePort", () => (fEv === "TIMEOUT" ? "TIMEOUT" : fEv.ports[0] instanceof MessagePort));
  const fBack = fEv !== "TIMEOUT" && fEv.ports[0] ? first(fCar.port2) : Promise.resolve("NO PORT");
  await t("F roundtrip through arrived port", () => {
    if (fEv === "TIMEOUT" || !fEv.ports[0]) return "NO PORT";
    fEv.ports[0].postMessage("F-pong");
    return fBack.then((x) => (typeof x === "string" ? x : String(x.data)));
  });
  await t("F post on transferred carrier", () => { fCar.port1.postMessage("dead"); return "no throw"; });
  await t("F duplicate port in the list", () => { const k = new MessageChannel(); window.postMessage("x", "*", [k.port1, k.port1]); return "no throw"; });
  await t("F plain object in the list", () => { window.postMessage("x", "*", [{}]); return "no throw"; });
  await t("F already-transferred port", () => { window.postMessage("x", "*", [fCar.port1]); return "no throw"; });
  await t("F transfer list is a number", () => { window.postMessage("x", "*", 5); return "no throw"; });
  await t("F options transfer is a number", () => { window.postMessage("x", { targetOrigin: "*", transfer: 5 }); return "no throw"; });
  await t("F options transfer is a list", () => { const k = new MessageChannel(); window.postMessage("x", { targetOrigin: "*", transfer: [k.port1] }); return "no throw"; });

  // ── G. an ArrayBuffer through a port: detach at the source, arrive whole ──────────────────────
  const g = new MessageChannel();
  const gab = new ArrayBuffer(8);
  new Uint8Array(gab)[0] = 42;
  const gGot = first(g.port2);
  g.port1.postMessage(gab, [gab]);
  const gEv = await gGot;
  await t("G source byteLength after transfer", () => gab.byteLength);
  await t("G source detached", () => gab.detached);
  await t("G arrived byteLength", () => (gEv === "TIMEOUT" ? "TIMEOUT" : gEv.data.byteLength));
  await t("G arrived first byte", () => (gEv === "TIMEOUT" ? "TIMEOUT" : new Uint8Array(gEv.data)[0]));
  await t("G arrived ports length", () => (gEv === "TIMEOUT" ? "TIMEOUT" : gEv.ports.length));
  const g2 = new MessageChannel();
  const g2ab = new ArrayBuffer(4);
  const g2Got = first(g2.port2);
  g2.port1.postMessage(g2ab);
  const g2Ev = await g2Got;
  await t("G no transfer list leaves the buffer alone", () => g2ab.byteLength);
  await t("G no transfer list still copies", () => (g2Ev === "TIMEOUT" ? "TIMEOUT" : g2Ev.data.byteLength));

  // ── H. a port and a buffer in ONE list ────────────────────────────────────────────────────────
  const h = new MessageChannel();
  const hCar = new MessageChannel();
  const hab = new ArrayBuffer(16);
  const hGot = first(h.port2);
  h.port1.postMessage({ b: hab }, [hCar.port1, hab]);
  const hEv = await hGot;
  await t("H ports length", () => (hEv === "TIMEOUT" ? "TIMEOUT" : hEv.ports.length));
  await t("H buffer detached", () => hab.byteLength);
  await t("H arrived buffer byteLength", () => (hEv === "TIMEOUT" ? "TIMEOUT" : hEv.data.b.byteLength));
  const h2Got = first(h.port2);
  const h2Car1 = new MessageChannel();
  const h2Car2 = new MessageChannel();
  h.port1.postMessage("H2", [h2Car1.port1, h2Car2.port1]);
  const h2Ev = await h2Got;
  await t("H two ports arrive", () => (h2Ev === "TIMEOUT" ? "TIMEOUT" : h2Ev.ports.length));
  await t("H two ports are distinct", () => (h2Ev === "TIMEOUT" ? "TIMEOUT" : h2Ev.ports[0] !== h2Ev.ports[1]));
  await t("H second arrived port works", () => {
    if (h2Ev === "TIMEOUT" || !h2Ev.ports[1]) return "NO PORT";
    const back = first(h2Car2.port2, 2000);
    h2Ev.ports[1].postMessage("H2-pong");
    return back.then((e) => (e === "TIMEOUT" ? "TIMEOUT" : String(e.data)));
  });

  // ── I. `MessageEvent`'s own `ports`, which the transfer has to agree with ─────────────────────
  const iEv = new MessageEvent("message");
  await t("I default ports tag", () => tag(iEv.ports));
  await t("I default ports length", () => iEv.ports.length);
  await t("I default ports frozen", () => Object.isFrozen(iEv.ports));
  await t("I default ports stable ref", () => iEv.ports === iEv.ports);
  const iCh = new MessageChannel();
  const iEv2 = new MessageEvent("message", { ports: [iCh.port1] });
  await t("I init ports length", () => iEv2.ports.length);
  await t("I init ports[0] is the given object", () => iEv2.ports[0] === iCh.port1);
  await t("I init ports frozen", () => Object.isFrozen(iEv2.ports));
  await t("I init does not neuter", () => { iCh.port1.postMessage("alive"); return "no throw"; });
  await t("I ports descriptor", () => {
    const d = Object.getOwnPropertyDescriptor(MessageEvent.prototype, "ports");
    return "get:" + typeof d.get + ",set:" + typeof d.set + ",enumerable:" + d.enumerable + ",configurable:" + d.configurable;
  });

  // ── J. the member itself ──────────────────────────────────────────────────────────────────────
  await t("J postMessage length", () => MessagePort.prototype.postMessage.length);
  await t("J postMessage name", () => MessagePort.prototype.postMessage.name);
  await t("J postMessage toString", () => MessagePort.prototype.postMessage.toString());
  await t("J window postMessage length", () => window.postMessage.length);

  // ── K. a port transferred INTO a nested browsing context, which is the shape the live
  //    cluster was found in: a frame is handed one end of a channel and answers on it. ───────────
  const fr = document.createElement("iframe");
  fr.src = "about:blank";
  document.body.appendChild(fr);
  await Promise.race([new Promise((res) => { fr.onload = () => res("load"); }), TMO(3000)]);
  const kCar = new MessageChannel();
  const kGot = new Promise((res) => {
    fr.contentWindow.addEventListener("message", (ev) => { if (ev.data === "K") res(ev); });
  });
  fr.contentWindow.postMessage("K", "*", [kCar.port1]);
  const kEv = await Promise.race([kGot, TMO(5000)]);
  shape("K", kEv);
  await t("K ports[0] is the carrier object", () => (kEv === "TIMEOUT" ? "TIMEOUT" : kEv.ports[0] === kCar.port1));
  const kBack = kEv !== "TIMEOUT" && kEv.ports[0] ? first(kCar.port2, 3000) : Promise.resolve("NO PORT");
  await t("K roundtrip through arrived port", () => {
    if (kEv === "TIMEOUT" || !kEv.ports[0]) return "NO PORT";
    kEv.ports[0].postMessage("K-pong");
    return kBack.then((x) => (typeof x === "string" ? x : String(x.data)));
  });
  await t("K post on transferred carrier", () => { kCar.port1.postMessage("dead"); return "no throw"; });

  return R.join("\n");
})
