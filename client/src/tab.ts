// One page target as a person uses it: its address and title, its history, its frames, its input, its dialogs.

import { CdpError, type CdpEvent, type Connection } from "./cdp.js";

export interface TargetInfo {
  targetId: string;
  type: string;
  title: string;
  url: string;
  attached?: boolean;
  openerId?: string;
  browserContextId?: string;
}

export interface HistoryEntry {
  id: number;
  url: string;
  title: string;
}

export interface Dialog {
  type: "alert" | "confirm" | "prompt" | "beforeunload";
  message: string;
  defaultPrompt: string;
  url: string;
}

/** One screencast frame: a JPEG or PNG of the viewport, and the CSS size it shows. */
export interface Frame {
  data: string;
  format: "jpeg" | "png";
  deviceWidth: number;
  deviceHeight: number;
}

/** CDP's modifier bits. */
export const enum Mod {
  Alt = 1,
  Ctrl = 2,
  Meta = 4,
  Shift = 8,
}

/**
 * Events (all plain `Event`s; read the tab's fields after them): `info` (url, title), `history`, `loading`,
 * `frame` (a `CustomEvent<Frame>`), `dialog` (the `dialog` field set or cleared), `refused` (the endpoint refused
 * input: a view-only ticket), `closed`.
 */
export class Tab extends EventTarget {
  sessionId?: string;
  history: { index: number; entries: HistoryEntry[] } = { index: 0, entries: [] };
  loading = false;
  dialog?: Dialog;
  closed = false;
  /** False once the endpoint refused this connection's input (a view-only viewer ticket). */
  interactive = true;
  private attaching?: Promise<void>;
  private casting = false;
  private castSize = { width: 0, height: 0 };
  private windowId?: number;
  /** The window's size beyond its page's: what to add to the page size a view wants (browser chrome, in Chrome). */
  private frameDelta?: { width: number; height: number };

  constructor(
    readonly conn: Connection,
    public info: TargetInfo,
    /** Told the session as soon as it exists: its events can arrive before `Page.enable` is answered. */
    private onSession?: (tab: Tab) => void,
  ) {
    super();
  }

  get id(): string {
    return this.info.targetId;
  }

  get canGoBack(): boolean {
    return this.history.index > 0;
  }

  get canGoForward(): boolean {
    return this.history.index < this.history.entries.length - 1;
  }

  /** Attach a session of its own, flattened onto the connection, and hear its page. */
  attach(): Promise<void> {
    this.attaching ??= (async () => {
      const r = await this.conn.send<{ sessionId: string }>("Target.attachToTarget", { targetId: this.id, flatten: true });
      this.sessionId = r.sessionId;
      this.onSession?.(this);
      await this.send("Page.enable");
      await this.refreshHistory();
    })();
    return this.attaching;
  }

  send<T = any>(method: string, params: object = {}): Promise<T> {
    return this.conn.send<T>(method, params, this.sessionId);
  }

  /** An event of this tab's session, routed here by the client. */
  handle(e: CdpEvent): void {
    switch (e.method) {
      case "Page.screencastFrame": {
        const p = e.params;
        const m = p.metadata ?? {};
        void this.send("Page.screencastFrameAck", { sessionId: p.sessionId }).catch(() => {});
        this.dispatchEvent(
          new CustomEvent<Frame>("frame", {
            detail: { data: p.data, format: this.castFormat, deviceWidth: m.deviceWidth ?? 0, deviceHeight: m.deviceHeight ?? 0 },
          }),
        );
        break;
      }
      case "Page.frameStartedLoading":
        if (e.params.frameId === this.id) this.setLoading(true);
        break;
      case "Page.frameStoppedLoading":
        if (e.params.frameId === this.id) this.setLoading(false);
        break;
      case "Page.loadEventFired":
        this.setLoading(false);
        break;
      case "Page.frameNavigated":
        if (!e.params.frame?.parentId) void this.refreshHistory();
        break;
      case "Page.navigatedWithinDocument":
        if (e.params.frameId === this.id) void this.refreshHistory();
        break;
      case "Page.javascriptDialogOpening":
        this.dialog = {
          type: e.params.type,
          message: e.params.message ?? "",
          defaultPrompt: e.params.defaultPrompt ?? "",
          url: e.params.url ?? this.info.url,
        };
        this.dispatchEvent(new Event("dialog"));
        break;
      case "Page.javascriptDialogClosed":
        this.dialog = undefined;
        this.dispatchEvent(new Event("dialog"));
        break;
    }
  }

  /** The target's info changed (`Target.targetInfoChanged`). */
  update(info: TargetInfo): void {
    this.info = { ...this.info, ...info };
    this.dispatchEvent(new Event("info"));
  }

  markClosed(): void {
    this.closed = true;
    this.dispatchEvent(new Event("closed"));
  }

  private setLoading(on: boolean): void {
    if (this.loading === on) return;
    this.loading = on;
    this.dispatchEvent(new Event("loading"));
  }

  async refreshHistory(): Promise<void> {
    try {
      const r = await this.send<{ currentIndex: number; entries: HistoryEntry[] }>("Page.getNavigationHistory");
      this.history = { index: r.currentIndex, entries: r.entries };
      this.dispatchEvent(new Event("history"));
    } catch {
      // an endpoint without session history: back and forward stay off
    }
  }

  // ── navigation ────────────────────────────────────────────────────────────────────────────────

  navigate(url: string): Promise<unknown> {
    return this.drive("Page.navigate", { url });
  }

  reload(): Promise<unknown> {
    return this.drive("Page.reload");
  }

  stop(): Promise<unknown> {
    return this.drive("Page.stopLoading");
  }

  back(): Promise<unknown> | undefined {
    const e = this.history.entries[this.history.index - 1];
    return e && this.drive("Page.navigateToHistoryEntry", { entryId: e.id });
  }

  forward(): Promise<unknown> | undefined {
    const e = this.history.entries[this.history.index + 1];
    return e && this.drive("Page.navigateToHistoryEntry", { entryId: e.id });
  }

  answerDialog(accept: boolean, promptText?: string): Promise<unknown> {
    const p: Record<string, unknown> = { accept };
    if (promptText !== undefined) p.promptText = promptText;
    return this.drive("Page.handleJavaScriptDialog", p);
  }

  // ── the picture ───────────────────────────────────────────────────────────────────────────────

  private castFormat: "jpeg" | "png" = "jpeg";

  /** Frames of the page at up to `width`×`height` device pixels; again with a new size when the view resizes. */
  async startScreencast(width: number, height: number, format: "jpeg" | "png" = "jpeg", quality = 82): Promise<void> {
    await this.attach();
    const w = Math.max(1, Math.round(width));
    const h = Math.max(1, Math.round(height));
    if (this.casting && this.castSize.width === w && this.castSize.height === h) return;
    if (this.casting) await this.send("Page.stopScreencast").catch(() => {});
    this.castFormat = format;
    this.castSize = { width: w, height: h };
    this.casting = true;
    await this.send("Page.startScreencast", { format, quality, maxWidth: w, maxHeight: h, everyNthFrame: 1 });
  }

  async stopScreencast(): Promise<void> {
    if (!this.casting) return;
    this.casting = false;
    await this.send("Page.stopScreencast").catch(() => {});
  }

  /**
   * Size the tab's window so its page is `width`×`height` CSS pixels, as a person resizing a browser window does
   * (`Browser.setWindowBounds`; korvus answers within its persona's screen).
   */
  async setPageSize(width: number, height: number): Promise<void> {
    await this.attach();
    try {
      if (this.windowId === undefined || !this.frameDelta) {
        const w = await this.conn.send<{ windowId: number; bounds: { width: number; height: number } }>("Browser.getWindowForTarget", {
          targetId: this.id,
        });
        this.windowId = w.windowId;
        const m = await this.send<{ cssLayoutViewport?: { clientWidth: number; clientHeight: number } }>("Page.getLayoutMetrics");
        const inner = m.cssLayoutViewport ?? { clientWidth: w.bounds.width, clientHeight: w.bounds.height };
        this.frameDelta = {
          width: Math.max(0, w.bounds.width - inner.clientWidth),
          height: Math.max(0, w.bounds.height - inner.clientHeight),
        };
      }
      await this.conn.send("Browser.setWindowBounds", {
        windowId: this.windowId,
        bounds: {
          width: Math.round(width + this.frameDelta.width),
          height: Math.round(height + this.frameDelta.height),
          windowState: "normal",
        },
      });
    } catch (e) {
      this.noteRefusal(e);
    }
  }

  // ── input ─────────────────────────────────────────────────────────────────────────────────────

  mouse(
    type: "mouseMoved" | "mousePressed" | "mouseReleased",
    x: number,
    y: number,
    button: "none" | "left" | "middle" | "right" = "none",
    buttons = 0,
    clickCount = 0,
    modifiers = 0,
  ): void {
    void this.drive("Input.dispatchMouseEvent", { type, x, y, button, buttons, clickCount, modifiers });
  }

  wheel(x: number, y: number, deltaX: number, deltaY: number, modifiers = 0): void {
    void this.drive("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX, deltaY, modifiers, button: "none" });
  }

  key(params: object): void {
    void this.drive("Input.dispatchKeyEvent", params);
  }

  insertText(text: string): void {
    void this.drive("Input.insertText", { text });
  }

  compose(text: string): void {
    const n = text.length;
    void this.drive("Input.imeSetComposition", { text, selectionStart: n, selectionEnd: n });
  }

  /** A command that drives the page: a refusal of it says the ticket is view-only. */
  private drive(method: string, params: object = {}): Promise<unknown> {
    return this.send(method, params).catch((e) => {
      this.noteRefusal(e);
    });
  }

  private noteRefusal(e: unknown): void {
    if (e instanceof CdpError && /not allowed on a viewer connection/.test(e.message) && this.interactive) {
      this.interactive = false;
      this.dispatchEvent(new Event("refused"));
    }
  }
}
