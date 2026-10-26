// One DevTools-protocol connection: commands answered by id, events handed to whoever listens.
//
// The client speaks only standard CDP, so the same connection drives korvus's viewer channel
// (`ws://host/?ticket=…`), korvus's automation channel, or any Chrome started with
// `--remote-allow-origins`.

export interface CdpEvent {
  method: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  params: any;
  sessionId?: string;
}

/** A command the other end refused, with its own code and message. */
export class CdpError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

/** The URL with its ticket hidden, for messages a person may copy somewhere. */
export function redact(url: string): string {
  return url.replace(/([?&#]ticket=)[^&]+/, "$1…");
}

export class Connection extends EventTarget {
  private ws: WebSocket;
  private next = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private listeners = new Set<(e: CdpEvent) => void>();
  readonly opened: Promise<void>;
  closed = false;

  constructor(readonly url: string) {
    super();
    this.ws = new WebSocket(url);
    this.opened = new Promise((resolve, reject) => {
      this.ws.addEventListener("open", () => resolve(), { once: true });
      this.ws.addEventListener("close", () => reject(new Error(`Could not connect to ${redact(url)}`)), { once: true });
    });
    this.ws.addEventListener("message", (m) => this.receive(String(m.data)));
    this.ws.addEventListener("close", () => {
      this.closed = true;
      for (const p of this.pending.values()) p.reject(new CdpError(-1, "The connection closed"));
      this.pending.clear();
      this.dispatchEvent(new Event("close"));
    });
  }

  send<T = any>(method: string, params: object = {}, sessionId?: string): Promise<T> {
    if (this.closed) return Promise.reject(new CdpError(-1, "The connection closed"));
    const id = this.next++;
    const msg: Record<string, unknown> = { id, method, params };
    if (sessionId) msg.sessionId = sessionId;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.ws.send(JSON.stringify(msg));
    });
  }

  /** Listen to every event; the returned function stops listening. */
  on(fn: (e: CdpEvent) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  close(): void {
    this.ws.close();
  }

  private receive(text: string): void {
    let msg: any;
    try {
      msg = JSON.parse(text);
    } catch {
      return;
    }
    if (typeof msg.id === "number") {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.reject(new CdpError(msg.error.code, msg.error.message));
      else p.resolve(msg.result ?? {});
      return;
    }
    if (typeof msg.method === "string") {
      const e: CdpEvent = { method: msg.method, params: msg.params ?? {}, sessionId: msg.sessionId };
      for (const fn of this.listeners) fn(e);
    }
  }
}
