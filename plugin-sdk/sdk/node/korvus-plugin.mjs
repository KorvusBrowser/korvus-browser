// korvus-plugin -- write a korvus plugin in Node, over the remote-plugin bridge.
//
// korvus's plugin ABI is a C ABI: every slot is a C function pointer, which is exactly right for the hot slots
// (on_call, on_request) where a plugin must be fast and in-process. The `remote-bridge` first-party plugin opens
// that door to the *infrequent* slots for people who would rather not write C: it is a tiny C shim loaded into
// korvus that forwards the slot callbacks to a server over a socket, so the real logic lives on the other end of the
// wire, in whatever language the author likes. This module is that other end, in Node.
//
// You subclass `KorvusPlugin` (or hand it callbacks) and fill the slots the bridge forwards, then call `listen()`:
//
//   import { KorvusPlugin, CONTINUE, BLOCK } from "./korvus-plugin.mjs";
//
//   class MyPlugin extends KorvusPlugin {
//     onEvent(ev)   { console.error(ev.kind, ev.detail); }
//     onRequest(req) { return req.url.includes("ads.") ? BLOCK : CONTINUE; }
//   }
//   await new MyPlugin({ name: "my-plugin" }).listen(9099);
//
// Then load the bridge into korvus, pointed at this server:
//
//   korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
//
// WHAT THE BRIDGE FORWARDS. The bridge speaks a plain TCP stream of newline-delimited JSON (NDJSON) -- one object
// per line, UTF-8, no framing or handshake. It carries exactly two of korvus's slots:
//
//   * onEvent (the observer slot) -- every context / page / navigation / request / response / console event korvus
//     reports. Fire-and-forget: this server sends no reply, so observing never slows the browser down.
//   * onRequest (the network verdict slot), when the bridge is loaded with `forward_requests:true` -- each request
//     hop, and the bridge BLOCKS on this server's one-line reply, then applies it. Return `BLOCK` to refuse the hop
//     (korvus's KORVUS_BLOCK); anything else continues. `onRequest` may be async -- its promise is awaited.
//
// korvus runs the browser endpoint and each page as separate OS processes and loads the bridge into each, so
// several connections arrive at once (one per process); `tag`/`pid` on each message tell them apart. Messages on one
// connection are strictly serial (the bridge waits for each request's reply before sending the next line), so this
// server processes each connection's lines in order even when a slot is async. A slot that throws (or rejects) is
// logged and treated as "did nothing" (onRequest continues), so a bug on this side never wedges the browser.
//
// Node built-ins only (`node:net`); no dependencies.

import net from "node:net";

// onRequest verdicts (the wire values the bridge understands). Return one from onRequest.
export const CONTINUE = "continue"; // let the hop proceed (the default)
export const BLOCK = "block"; // refuse the hop (korvus's KORVUS_BLOCK)

/**
 * One observer event the bridge forwarded (onEvent). Fire-and-forget: there is nothing to answer.
 * `kind` is korvus's event name ("context.created", "page.created", "navigation", "request", "response",
 * "console", ...); `detail` is the event's own JSON payload (object/array) or null; `pid`/`tag` identify the korvus
 * process and bridge instance; `raw` is the whole decoded line.
 */
export class Event {
  constructor(raw) {
    this.raw = raw;
    this.kind = String(raw.kind ?? "");
    this.detail = raw.json != null && typeof raw.json === "object" ? raw.json : null;
    this.pid = Number(raw.pid ?? 0);
    this.tag = String(raw.tag ?? "");
  }
}

/**
 * One request hop awaiting a verdict (onRequest). Return CONTINUE or BLOCK.
 * `url`/`method`/`destination` describe the hop (destination is the Fetch request destination -- "document",
 * "image", "script", ...); `pid`/`tag` identify the source; `id` is the bridge's per-connection request id (the
 * reply is matched to it automatically).
 */
export class Request {
  constructor(raw) {
    this.raw = raw;
    this.url = String(raw.url ?? "");
    this.method = String(raw.method ?? "");
    this.destination = String(raw.destination ?? "");
    this.pid = Number(raw.pid ?? 0);
    this.tag = String(raw.tag ?? "");
    this.id = raw.id;
  }
}

// Map whatever a handler returned onto the wire's "block"/"continue" (continue unless it clearly means block).
function normalizeVerdict(result) {
  if (result === true) return BLOCK;
  if (typeof result === "string" && result.trim().toLowerCase() === BLOCK) return BLOCK;
  return CONTINUE;
}

/**
 * A korvus plugin written in Node, served over the remote-plugin bridge. Fill the slots the bridge forwards, either
 * by subclassing and overriding `onEvent` / `onRequest`, or by passing callbacks to the constructor (a callback
 * wins over the subclass method for that slot). Then call `listen()`.
 */
export class KorvusPlugin {
  constructor({ name = "node-plugin", onEvent, onRequest } = {}) {
    this.name = name;
    if (onEvent) this._onEvent = onEvent;
    if (onRequest) this._onRequest = onRequest;
    this.port = 0; // the bound port, filled by listen()
    this._server = null;
  }

  // ── the slots (override these, or pass callbacks to the constructor) ──
  onEvent(_event) {} // an observer event arrived; the return value is ignored. Default: do nothing.
  onRequest(_request) {
    return CONTINUE; // a request hop awaits a verdict. Return CONTINUE (default) or BLOCK; may be async.
  }

  _dispatchEvent(event) {
    return (this._onEvent || this.onEvent).call(this, event);
  }
  _dispatchRequest(request) {
    return (this._onRequest || this.onRequest).call(this, request);
  }

  /** Serve the bridge's connections. Resolves with the bound port (useful with port 0); the server runs until close(). */
  listen(port = 9099, host = "127.0.0.1") {
    return new Promise((resolve, reject) => {
      const server = net.createServer((sock) => this._serveConn(sock));
      server.on("error", reject);
      server.listen(port, host, () => {
        this.port = server.address().port;
        this._server = server;
        resolve(this.port);
      });
    });
  }

  /** Stop accepting new connections. Resolves once the server is closed. */
  close() {
    return new Promise((resolve) => {
      if (!this._server) return resolve();
      this._server.close(() => resolve());
      this._server = null;
    });
  }

  _serveConn(sock) {
    sock.setNoDelay(true);
    let buf = Buffer.alloc(0);
    let chain = Promise.resolve(); // process this connection's lines strictly in order, even if a slot is async
    sock.on("data", (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      let nl;
      while ((nl = buf.indexOf(0x0a)) !== -1) {
        const line = buf.subarray(0, nl);
        buf = buf.subarray(nl + 1);
        const text = line.toString("utf8");
        chain = chain.then(() => this._handleLine(text, sock));
      }
    });
    sock.on("error", () => {});
  }

  async _handleLine(text, sock) {
    const trimmed = text.trim();
    if (!trimmed) return;
    let obj;
    try {
      obj = JSON.parse(trimmed);
    } catch {
      return; // junk line: skip it
    }
    if (obj == null || typeof obj !== "object") return;
    if (obj.t === "event") {
      try {
        await this._dispatchEvent(new Event(obj));
      } catch (e) {
        this._warn(`onEvent threw: ${e}`);
      }
      return;
    }
    if (obj.t === "request") {
      let verdict = CONTINUE;
      try {
        verdict = normalizeVerdict(await this._dispatchRequest(new Request(obj)));
      } catch (e) {
        this._warn(`onRequest threw: ${e} (continuing the hop)`);
      }
      const reply = JSON.stringify({ id: obj.id ?? null, verdict }) + "\n";
      if (!sock.destroyed) sock.write(reply);
    }
  }

  _warn(msg) {
    console.error(`korvus-plugin[${this.name}]: ${msg}`);
  }
}

export default KorvusPlugin;
