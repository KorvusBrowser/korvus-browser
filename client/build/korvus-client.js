class u extends Error {
  constructor(t, s) {
    super(s), this.code = t;
  }
}
function y(o) {
  return o.replace(/([?&#]ticket=)[^&]+/, "$1…");
}
class E extends EventTarget {
  constructor(t) {
    super(), this.url = t, this.ws = new WebSocket(t), this.opened = new Promise((s, e) => {
      this.ws.addEventListener("open", () => s(), { once: !0 }), this.ws.addEventListener("close", () => e(new Error(`Could not connect to ${y(t)}`)), { once: !0 });
    }), this.ws.addEventListener("message", (s) => this.receive(String(s.data))), this.ws.addEventListener("close", () => {
      this.closed = !0;
      for (const s of this.pending.values()) s.reject(new u(-1, "The connection closed"));
      this.pending.clear(), this.dispatchEvent(new Event("close"));
    });
  }
  ws;
  next = 1;
  pending = /* @__PURE__ */ new Map();
  listeners = /* @__PURE__ */ new Set();
  opened;
  closed = !1;
  send(t, s = {}, e) {
    if (this.closed) return Promise.reject(new u(-1, "The connection closed"));
    const i = this.next++, n = { id: i, method: t, params: s };
    return e && (n.sessionId = e), new Promise((a, r) => {
      this.pending.set(i, { resolve: a, reject: r }), this.ws.send(JSON.stringify(n));
    });
  }
  /** Listen to every event; the returned function stops listening. */
  on(t) {
    return this.listeners.add(t), () => this.listeners.delete(t);
  }
  close() {
    this.ws.close();
  }
  receive(t) {
    let s;
    try {
      s = JSON.parse(t);
    } catch {
      return;
    }
    if (typeof s.id == "number") {
      const e = this.pending.get(s.id);
      if (!e) return;
      this.pending.delete(s.id), s.error ? e.reject(new u(s.error.code, s.error.message)) : e.resolve(s.result ?? {});
      return;
    }
    if (typeof s.method == "string") {
      const e = { method: s.method, params: s.params ?? {}, sessionId: s.sessionId };
      for (const i of this.listeners) i(e);
    }
  }
}
var h = /* @__PURE__ */ ((o) => (o[o.Alt = 1] = "Alt", o[o.Ctrl = 2] = "Ctrl", o[o.Meta = 4] = "Meta", o[o.Shift = 8] = "Shift", o))(h || {});
class T extends EventTarget {
  constructor(t, s, e) {
    super(), this.conn = t, this.info = s, this.onSession = e;
  }
  sessionId;
  history = { index: 0, entries: [] };
  loading = !1;
  dialog;
  closed = !1;
  /** False once the endpoint refused this connection's input (a view-only viewer ticket). */
  interactive = !0;
  attaching;
  casting = !1;
  castSize = { width: 0, height: 0 };
  windowId;
  /** The window's size beyond its page's: what to add to the page size a view wants (browser chrome, in Chrome). */
  frameDelta;
  get id() {
    return this.info.targetId;
  }
  get canGoBack() {
    return this.history.index > 0;
  }
  get canGoForward() {
    return this.history.index < this.history.entries.length - 1;
  }
  /** Attach a session of its own, flattened onto the connection, and hear its page. */
  attach() {
    return this.attaching ??= (async () => {
      const t = await this.conn.send("Target.attachToTarget", { targetId: this.id, flatten: !0 });
      this.sessionId = t.sessionId, this.onSession?.(this), await this.send("Page.enable"), await this.refreshHistory();
    })(), this.attaching;
  }
  send(t, s = {}) {
    return this.conn.send(t, s, this.sessionId);
  }
  /** An event of this tab's session, routed here by the client. */
  handle(t) {
    switch (t.method) {
      case "Page.screencastFrame": {
        const s = t.params, e = s.metadata ?? {};
        this.send("Page.screencastFrameAck", { sessionId: s.sessionId }).catch(() => {
        }), this.dispatchEvent(
          new CustomEvent("frame", {
            detail: { data: s.data, format: this.castFormat, deviceWidth: e.deviceWidth ?? 0, deviceHeight: e.deviceHeight ?? 0 }
          })
        );
        break;
      }
      case "Page.frameStartedLoading":
        t.params.frameId === this.id && this.setLoading(!0);
        break;
      case "Page.frameStoppedLoading":
        t.params.frameId === this.id && this.setLoading(!1);
        break;
      case "Page.loadEventFired":
        this.setLoading(!1);
        break;
      case "Page.frameNavigated":
        t.params.frame?.parentId || this.refreshHistory();
        break;
      case "Page.navigatedWithinDocument":
        t.params.frameId === this.id && this.refreshHistory();
        break;
      case "Page.javascriptDialogOpening":
        this.dialog = {
          type: t.params.type,
          message: t.params.message ?? "",
          defaultPrompt: t.params.defaultPrompt ?? "",
          url: t.params.url ?? this.info.url
        }, this.dispatchEvent(new Event("dialog"));
        break;
      case "Page.javascriptDialogClosed":
        this.dialog = void 0, this.dispatchEvent(new Event("dialog"));
        break;
    }
  }
  /** The target's info changed (`Target.targetInfoChanged`). */
  update(t) {
    this.info = { ...this.info, ...t }, this.dispatchEvent(new Event("info"));
  }
  markClosed() {
    this.closed = !0, this.dispatchEvent(new Event("closed"));
  }
  setLoading(t) {
    this.loading !== t && (this.loading = t, this.dispatchEvent(new Event("loading")));
  }
  async refreshHistory() {
    try {
      const t = await this.send("Page.getNavigationHistory");
      this.history = { index: t.currentIndex, entries: t.entries }, this.dispatchEvent(new Event("history"));
    } catch {
    }
  }
  // ── navigation ────────────────────────────────────────────────────────────────────────────────
  navigate(t) {
    return this.drive("Page.navigate", { url: t });
  }
  reload() {
    return this.drive("Page.reload");
  }
  stop() {
    return this.drive("Page.stopLoading");
  }
  back() {
    const t = this.history.entries[this.history.index - 1];
    return t && this.drive("Page.navigateToHistoryEntry", { entryId: t.id });
  }
  forward() {
    const t = this.history.entries[this.history.index + 1];
    return t && this.drive("Page.navigateToHistoryEntry", { entryId: t.id });
  }
  answerDialog(t, s) {
    const e = { accept: t };
    return s !== void 0 && (e.promptText = s), this.drive("Page.handleJavaScriptDialog", e);
  }
  // ── the picture ───────────────────────────────────────────────────────────────────────────────
  castFormat = "jpeg";
  /** Frames of the page at up to `width`×`height` device pixels; again with a new size when the view resizes. */
  async startScreencast(t, s, e = "jpeg", i = 82) {
    await this.attach();
    const n = Math.max(1, Math.round(t)), a = Math.max(1, Math.round(s));
    this.casting && this.castSize.width === n && this.castSize.height === a || (this.casting && await this.send("Page.stopScreencast").catch(() => {
    }), this.castFormat = e, this.castSize = { width: n, height: a }, this.casting = !0, await this.send("Page.startScreencast", { format: e, quality: i, maxWidth: n, maxHeight: a, everyNthFrame: 1 }));
  }
  async stopScreencast() {
    this.casting && (this.casting = !1, await this.send("Page.stopScreencast").catch(() => {
    }));
  }
  /**
   * Size the tab's window so its page is `width`×`height` CSS pixels, as a person resizing a browser window does
   * (`Browser.setWindowBounds`; korvus answers within its persona's screen).
   */
  async setPageSize(t, s) {
    await this.attach();
    try {
      if (this.windowId === void 0 || !this.frameDelta) {
        const e = await this.conn.send("Browser.getWindowForTarget", {
          targetId: this.id
        });
        this.windowId = e.windowId;
        const n = (await this.send("Page.getLayoutMetrics")).cssLayoutViewport ?? { clientWidth: e.bounds.width, clientHeight: e.bounds.height };
        this.frameDelta = {
          width: Math.max(0, e.bounds.width - n.clientWidth),
          height: Math.max(0, e.bounds.height - n.clientHeight)
        };
      }
      await this.conn.send("Browser.setWindowBounds", {
        windowId: this.windowId,
        bounds: {
          width: Math.round(t + this.frameDelta.width),
          height: Math.round(s + this.frameDelta.height),
          windowState: "normal"
        }
      });
    } catch (e) {
      this.noteRefusal(e);
    }
  }
  // ── input ─────────────────────────────────────────────────────────────────────────────────────
  mouse(t, s, e, i = "none", n = 0, a = 0, r = 0) {
    this.drive("Input.dispatchMouseEvent", { type: t, x: s, y: e, button: i, buttons: n, clickCount: a, modifiers: r });
  }
  wheel(t, s, e, i, n = 0) {
    this.drive("Input.dispatchMouseEvent", { type: "mouseWheel", x: t, y: s, deltaX: e, deltaY: i, modifiers: n, button: "none" });
  }
  key(t) {
    this.drive("Input.dispatchKeyEvent", t);
  }
  insertText(t) {
    this.drive("Input.insertText", { text: t });
  }
  compose(t) {
    const s = t.length;
    this.drive("Input.imeSetComposition", { text: t, selectionStart: s, selectionEnd: s });
  }
  /** A command that drives the page: a refusal of it says the ticket is view-only. */
  drive(t, s = {}) {
    return this.send(t, s).catch((e) => {
      this.noteRefusal(e);
    });
  }
  noteRefusal(t) {
    t instanceof u && /not allowed on a viewer connection/.test(t.message) && this.interactive && (this.interactive = !1, this.dispatchEvent(new Event("refused")));
  }
}
class v extends EventTarget {
  constructor(t) {
    super(), this.conn = t, t.on((s) => this.onEvent(s)), t.addEventListener("close", () => {
      window.clearInterval(this.titleTimer), this.dispatchEvent(new Event("close"));
    });
  }
  tabs = /* @__PURE__ */ new Map();
  bySession = /* @__PURE__ */ new Map();
  started = !1;
  titleTimer;
  /** Connect to `endpoint` (a DevTools WebSocket URL; a korvus viewer's carries its `?ticket=`). */
  static async connect(t) {
    const s = new E(t);
    await s.opened;
    const e = new v(s);
    return await e.start(), e;
  }
  /** The tabs a person sees in the tab strip, oldest first: every page but the popups. */
  get pages() {
    return [...this.tabs.values()].filter((t) => !t.info.openerId);
  }
  async start() {
    if (this.started) return;
    this.started = !0, await this.conn.send("Target.setDiscoverTargets", { discover: !0 });
    const t = await this.conn.send("Target.getTargets");
    for (const s of t.targetInfos.reverse()) this.add(s, !1);
    this.dispatchEvent(new Event("tabs")), this.titleTimer = window.setInterval(() => void this.refreshInfo(), 2500);
  }
  /** Read every tab's title and address again. */
  async refreshInfo() {
    try {
      const t = await this.conn.send("Target.getTargets");
      for (const s of t.targetInfos) {
        const e = this.tabs.get(s.targetId);
        e && (e.info.title !== s.title || e.info.url !== s.url) && e.update(s);
      }
    } catch {
    }
  }
  /** A new tab at `url`; resolves with it once the endpoint has made it. */
  async newTab(t = "about:blank") {
    const s = await this.conn.send("Target.createTarget", { url: t }), e = this.tabs.get(s.targetId);
    if (e) return e;
    const i = this.add({ targetId: s.targetId, type: "page", title: "", url: t }, !1);
    return this.dispatchEvent(new Event("tabs")), i;
  }
  closeTab(t) {
    return this.conn.send("Target.closeTarget", { targetId: t.id }).catch(() => {
    });
  }
  close() {
    this.conn.close();
  }
  add(t, s) {
    if (t.type !== "page") return;
    let e = this.tabs.get(t.targetId);
    return e ? (e.update(t), e) : (e = new T(this.conn, t, (i) => {
      i.sessionId && this.bySession.set(i.sessionId, i);
    }), this.tabs.set(t.targetId, e), e.attach().catch(() => {
    }), s && t.openerId && this.dispatchEvent(new CustomEvent("popup", { detail: e, cancelable: !0 })), e);
  }
  onEvent(t) {
    if (t.sessionId) {
      this.bySession.get(t.sessionId)?.handle(t), (t.method === "Page.loadEventFired" || t.method === "Page.navigatedWithinDocument") && (window.setTimeout(() => void this.refreshInfo(), 150), window.setTimeout(() => void this.refreshInfo(), 900));
      return;
    }
    switch (t.method) {
      case "Target.targetCreated":
        this.add(t.params.targetInfo, !0) && !t.params.targetInfo.openerId && this.dispatchEvent(new Event("tabs"));
        break;
      case "Target.targetInfoChanged":
        this.tabs.get(t.params.targetInfo.targetId)?.update(t.params.targetInfo);
        break;
      case "Target.targetDestroyed": {
        const s = this.tabs.get(t.params.targetId);
        if (!s) break;
        this.tabs.delete(s.id), s.sessionId && this.bySession.delete(s.sessionId), s.markClosed(), this.dispatchEvent(new Event("tabs"));
        break;
      }
    }
  }
}
const m = `
  --korvus-font: system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", "Noto Sans KR", sans-serif;
  --korvus-ink: #1d2330;
  --korvus-ink-soft: #5b6475;
  --korvus-strip: #e3e8ee;
  --korvus-bar: #f6f8fa;
  --korvus-tab: #ffffff;
  --korvus-line: #c7cfd9;
  --korvus-field: #ffffff;
  --korvus-accent: #4a3fd0;
  --korvus-live: #1d8a5a;
  --korvus-view-only: #9a6410;
  --korvus-off: #a9382c;
  --korvus-page: #ffffff;
  --korvus-scrim: rgb(29 35 48 / 0.32);
`, k = `
  --korvus-ink: #e2e6ed;
  --korvus-ink-soft: #9aa3b2;
  --korvus-strip: #181c24;
  --korvus-bar: #20252f;
  --korvus-tab: #2a303c;
  --korvus-line: #39414f;
  --korvus-field: #171b22;
  --korvus-accent: #a59cff;
  --korvus-live: #4cc58e;
  --korvus-view-only: #e0a447;
  --korvus-off: #f0806f;
  --korvus-scrim: rgb(0 0 0 / 0.45);
`, S = `
  :host { ${m} display: block; position: relative; overflow: hidden; background: var(--korvus-page);
    font: 13px/1.4 var(--korvus-font); color: var(--korvus-ink); contain: strict; }
  @media (prefers-color-scheme: dark) { :host { ${k} } }
  .stage { position: absolute; inset: 0; }
  canvas { position: absolute; inset: 0; width: 100%; height: 100%; display: block; touch-action: none; outline: none; }
  .keys { position: absolute; left: 0; top: 0; width: 1px; height: 1px; padding: 0; border: 0; margin: 0;
    opacity: 0; resize: none; overflow: hidden; caret-color: transparent; font-size: 16px; }
  .note { position: absolute; inset: 0; display: grid; place-items: center; padding: 24px; text-align: center;
    color: var(--korvus-ink-soft); background: var(--korvus-bar); }
  .note[hidden], .dialog-layer[hidden], [hidden] { display: none !important; }
  .dialog-layer { position: absolute; inset: 0; background: var(--korvus-scrim); display: flex; justify-content: center;
    align-items: flex-start; padding-top: 12px; }
  .dialog { width: min(440px, calc(100% - 32px)); background: var(--korvus-tab); color: var(--korvus-ink);
    border: 1px solid var(--korvus-line); border-radius: 10px; padding: 18px 20px 14px;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.22); }
  .from { margin: 0 0 6px; font-weight: 600; }
  .message { margin: 0 0 14px; white-space: pre-wrap; overflow-wrap: anywhere; max-height: 40vh; overflow: auto; }
  .prompt { width: 100%; box-sizing: border-box; font: inherit; color: inherit; background: var(--korvus-field);
    border: 1px solid var(--korvus-line); border-radius: 6px; padding: 6px 8px; margin-bottom: 14px; }
  .buttons { display: flex; justify-content: flex-end; gap: 8px; }
  button { font: inherit; color: inherit; background: var(--korvus-bar); border: 1px solid var(--korvus-line);
    border-radius: 6px; padding: 5px 14px; cursor: pointer; }
  .ok { background: var(--korvus-accent); border-color: var(--korvus-accent); color: #fff; }
  button:focus-visible, .prompt:focus-visible { outline: 2px solid var(--korvus-accent); outline-offset: 2px; }
`, C = `
  :host { ${m} display: flex; flex-direction: column; min-height: 240px; background: var(--korvus-strip);
    font: 13px/1.4 var(--korvus-font); color: var(--korvus-ink); position: relative; overflow: hidden; }
  @media (prefers-color-scheme: dark) { :host { ${k} } }
  [hidden] { display: none !important; }
  button { font: inherit; color: inherit; background: none; border: 0; cursor: pointer; }
  button:focus-visible, input:focus-visible { outline: 2px solid var(--korvus-accent); outline-offset: -2px; }
  svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round;
    stroke-linejoin: round; }

  .strip { display: flex; align-items: flex-end; gap: 2px; padding: 6px 8px 0; min-height: 36px; box-sizing: border-box; }
  .tabs { display: flex; gap: 2px; min-width: 0; flex: 1 1 auto; overflow-x: auto; scrollbar-width: none; }
  .tab { display: flex; align-items: center; gap: 6px; flex: 0 1 220px; min-width: 72px; height: 30px;
    padding: 0 6px 0 12px; border-radius: 8px 8px 0 0; color: var(--korvus-ink-soft); position: relative; }
  .tab:hover { background: color-mix(in srgb, var(--korvus-tab) 55%, transparent); }
  .tab[aria-selected="true"] { background: var(--korvus-bar); color: var(--korvus-ink); }
  .tab .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; text-align: left; }
  .tab .spin { width: 10px; height: 10px; border-radius: 50%; border: 2px solid var(--korvus-line);
    border-top-color: var(--korvus-accent); animation: spin .8s linear infinite; flex: none; }
  .tab .x { display: grid; place-items: center; width: 20px; height: 20px; border-radius: 5px; flex: none; }
  .tab .x:hover { background: var(--korvus-strip); }
  .tab .x svg { width: 12px; height: 12px; }
  .new { display: grid; place-items: center; width: 28px; height: 28px; margin-bottom: 1px; border-radius: 6px;
    color: var(--korvus-ink-soft); flex: none; }
  .new:hover { background: var(--korvus-tab); color: var(--korvus-ink); }

  .capsule { display: flex; align-items: center; gap: 6px; margin: 0 0 5px 8px; padding: 3px 10px 3px 8px;
    border-radius: 999px; border: 1px solid var(--korvus-line); background: var(--korvus-bar); white-space: nowrap;
    color: var(--korvus-ink-soft); flex: none; font-size: 12px; }
  .capsule .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--korvus-live); }
  .capsule[data-state="view-only"] .dot { background: var(--korvus-view-only); }
  .capsule[data-state="off"] .dot { background: var(--korvus-off); }
  .capsule[data-state="connecting"] .dot { background: var(--korvus-line); }
  .capsule .persona { color: var(--korvus-ink); font-weight: 600; }
  .capsule button { text-decoration: underline; padding: 0; color: var(--korvus-ink); }

  .bar { display: flex; align-items: center; gap: 2px; padding: 5px 8px; background: var(--korvus-bar);
    border-bottom: 1px solid var(--korvus-line); }
  .nav { display: grid; place-items: center; width: 30px; height: 30px; border-radius: 50%; color: var(--korvus-ink); }
  .nav:hover:not(:disabled) { background: var(--korvus-strip); }
  .nav:disabled { color: var(--korvus-line); cursor: default; }
  .address { flex: 1; min-width: 0; height: 30px; margin-left: 6px; padding: 0 14px; box-sizing: border-box;
    border-radius: 999px; border: 1px solid transparent; background: var(--korvus-strip); color: var(--korvus-ink);
    font: inherit; font-size: 14px; }
  .address:focus { background: var(--korvus-field); border-color: var(--korvus-accent); outline: none; }

  .pages { flex: 1; position: relative; background: var(--korvus-page); min-height: 0; }
  .pages > korvus-view { position: absolute; inset: 0; }
  .empty { position: absolute; inset: 0; display: grid; place-items: center; color: var(--korvus-ink-soft);
    background: var(--korvus-bar); padding: 24px; text-align: center; }

  .windows { position: absolute; inset: 0; pointer-events: none; }
  .pages[data-dialog] .windows { opacity: 0.3; }
  .pages[data-dialog] .window { pointer-events: none; }
  .window { position: absolute; pointer-events: auto; display: flex; flex-direction: column; background: var(--korvus-bar);
    border: 1px solid var(--korvus-line); border-radius: 10px; overflow: hidden; resize: both;
    box-shadow: 0 18px 48px rgb(0 0 0 / 0.28); min-width: 280px; min-height: 200px; }
  .window header { display: flex; align-items: center; gap: 8px; padding: 0 6px 0 12px; height: 34px; cursor: move;
    user-select: none; border-bottom: 1px solid var(--korvus-line); }
  .window header .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .window header .host { color: var(--korvus-ink-soft); }
  .window korvus-view { flex: 1; min-height: 0; }
  .window .x { display: grid; place-items: center; width: 24px; height: 24px; border-radius: 6px; }
  .window .x:hover { background: var(--korvus-strip); }

  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .tab .spin { animation: none; border-top-color: var(--korvus-line); } }
  @media (max-width: 560px) {
    .capsule .label { display: none; }
    .tab { flex-basis: 140px; }
  }
`, c = {
  back: '<svg viewBox="0 0 16 16"><path d="M10 3 5 8l5 5"/></svg>',
  forward: '<svg viewBox="0 0 16 16"><path d="m6 3 5 5-5 5"/></svg>',
  reload: '<svg viewBox="0 0 16 16"><path d="M13 8a5 5 0 1 1-1.6-3.7"/><path d="M13 2.5V5h-2.5"/></svg>',
  stop: '<svg viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8"/></svg>',
  close: '<svg viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8"/></svg>',
  plus: '<svg viewBox="0 0 16 16"><path d="M8 3v10M3 8h10"/></svg>'
};
function I(o, t) {
  const s = o.trim();
  return s ? /^[a-z][a-z0-9+.-]*:/i.test(s) && !/^[^\s/]+:\d+/.test(s) ? s : !/\s/.test(s) && (/^localhost(:\d+)?(\/|$)/i.test(s) || /^[^\s/]+\.[^\s/]{2,}(\/|$|:)/.test(s) || /^\d+\.\d+\.\d+\.\d+/.test(s)) ? (/^(localhost|127\.|\d+\.\d+\.\d+\.\d+)/i.test(s) ? "http://" : "https://") + s : t.replace("%s", encodeURIComponent(s)) : "about:blank";
}
function g(o) {
  return o === "about:blank" ? "" : o;
}
class L extends HTMLElement {
  static observedAttributes = ["endpoint", "persona"];
  root;
  client;
  active;
  views = /* @__PURE__ */ new Map();
  windows = /* @__PURE__ */ new Map();
  state = "connecting";
  el;
  tabOff = [];
  constructor() {
    super(), this.root = this.attachShadow({ mode: "open" }), this.root.innerHTML = `<style>${C}</style>
      <div class="strip">
        <div class="tabs" role="tablist" aria-label="Tabs"></div>
        <button class="new" type="button" aria-label="New tab" title="New tab">${c.plus}</button>
        <div class="capsule" data-state="connecting" role="status"></div>
      </div>
      <div class="bar">
        <button class="nav back" type="button" aria-label="Back" title="Back" disabled>${c.back}</button>
        <button class="nav forward" type="button" aria-label="Forward" title="Forward" disabled>${c.forward}</button>
        <button class="nav reload" type="button" aria-label="Reload" title="Reload" disabled>${c.reload}</button>
        <input class="address" type="text" aria-label="Address" placeholder="Search or type an address"
          autocomplete="off" autocapitalize="off" spellcheck="false">
      </div>
      <div class="pages">
        <div class="empty" hidden></div>
        <div class="windows"></div>
      </div>`;
    const t = (s) => this.root.querySelector(s);
    this.el = {
      tabs: t(".tabs"),
      newTab: t(".new"),
      capsule: t(".capsule"),
      back: t(".back"),
      forward: t(".forward"),
      reload: t(".reload"),
      address: t(".address"),
      pages: t(".pages"),
      empty: t(".empty"),
      windows: t(".windows")
    }, this.el.newTab.addEventListener("click", () => void this.openTab()), this.el.back.addEventListener("click", () => void this.active?.back()), this.el.forward.addEventListener("click", () => void this.active?.forward()), this.el.reload.addEventListener("click", () => {
      const s = this.active;
      s && (s.loading ? s.stop() : s.reload());
    }), this.el.address.addEventListener("keydown", (s) => {
      if (s.key === "Enter") {
        const e = this.active;
        if (!e) return;
        const i = I(this.el.address.value, this.getAttribute("search") ?? "https://www.google.com/search?q=%s");
        e.navigate(i), this.el.address.blur(), this.views.get(e.id)?.focusPage();
      } else s.key === "Escape" && (this.el.address.value = g(this.active?.info.url ?? ""), this.el.address.select());
    }), this.el.address.addEventListener("focus", () => this.el.address.select()), this.el.address.addEventListener("blur", () => this.syncBar()), this.renderCapsule();
  }
  connectedCallback() {
    this.client || this.connect();
  }
  disconnectedCallback() {
    this.client?.close(), this.client = void 0;
  }
  attributeChangedCallback(t, s, e) {
    t === "endpoint" && s !== null && s !== e && this.isConnected && this.connect(), t === "persona" && this.renderCapsule();
  }
  /** The headless core underneath, for a page that wants more than the element shows. */
  get core() {
    return this.client;
  }
  /** (Re)connect to `endpoint`. */
  async connect() {
    const t = this.getAttribute("endpoint");
    if (this.client?.close(), this.client = void 0, this.reset(), !t) {
      this.setState("off", "No endpoint to connect to.");
      return;
    }
    this.setState("connecting");
    try {
      const s = await v.connect(t);
      this.client = s, s.addEventListener("tabs", () => this.renderTabs()), s.addEventListener("popup", (e) => this.onPopup(e)), s.addEventListener("close", () => this.setState("off", "The connection ended.")), this.setState("live"), s.pages.length === 0 ? await this.openTab() : this.activate(s.pages[s.pages.length - 1]), this.renderTabs();
    } catch (s) {
      this.setState("off", s instanceof Error ? s.message : String(s));
    }
  }
  /** A new tab at the `home` page, made active with the address bar ready to type into. */
  async openTab(t) {
    const s = this.client;
    if (s)
      try {
        const e = await s.newTab(t ?? this.getAttribute("home") ?? "about:blank");
        this.activate(e), this.renderTabs(), t || this.el.address.focus();
      } catch {
        this.setState("view-only");
      }
  }
  reset() {
    for (const t of this.views.values()) t.remove();
    for (const t of this.windows.values()) t.remove();
    this.views.clear(), this.windows.clear(), this.active = void 0, this.el.tabs.replaceChildren();
  }
  setState(t, s = "") {
    this.state = t, this.renderCapsule(), this.el.empty.hidden = t !== "off", this.el.empty.textContent = s, this.el.newTab.disabled = t !== "live", this.dispatchEvent(new CustomEvent("korvus-state", { detail: { state: t, reason: s } }));
  }
  renderCapsule() {
    const t = this.el?.capsule;
    if (!t) return;
    t.dataset.state = this.state;
    const s = { connecting: "Connecting", live: "Live", "view-only": "View only", off: "Disconnected" }[this.state], e = this.getAttribute("persona");
    if (t.innerHTML = '<span class="dot" aria-hidden="true"></span><span class="label"></span>', t.querySelector(".label").textContent = s, e) {
      const i = document.createElement("span");
      i.className = "persona", i.textContent = e, t.append(i);
    }
    if (this.state === "off" && this.getAttribute("endpoint")) {
      const i = document.createElement("button");
      i.type = "button", i.textContent = "Reconnect", i.addEventListener("click", () => void this.connect()), t.append(i);
    }
  }
  // ── tabs ──────────────────────────────────────────────────────────────────────────────────────
  renderTabs() {
    const t = this.client;
    if (!t) return;
    const s = t.pages;
    this.active && !t.tabs.has(this.active.id) && (this.active = void 0), !this.active && s.length && this.activate(s[s.length - 1]), this.el.tabs.replaceChildren(
      ...s.map((i) => {
        const n = document.createElement("div");
        n.className = "tab", n.setAttribute("role", "tab"), n.setAttribute("aria-selected", String(i === this.active));
        const a = document.createElement("button");
        a.type = "button", a.className = "title", a.textContent = i.info.title || g(i.info.url) || "New tab", a.title = a.textContent, a.addEventListener("click", () => this.activate(i));
        const r = document.createElement("span");
        r.className = "spin", r.hidden = !i.loading;
        const d = document.createElement("button");
        return d.type = "button", d.className = "x", d.setAttribute("aria-label", `Close ${a.textContent}`), d.innerHTML = c.close, d.addEventListener("click", () => void t.closeTab(i)), n.append(r, a, d), n;
      })
    );
    for (const [i, n] of this.views)
      t.tabs.has(i) || (n.remove(), this.views.delete(i));
  }
  activate(t) {
    if (this.active === t) return;
    for (const i of this.tabOff) i();
    this.tabOff = [], this.active = t;
    for (const [i, n] of this.views)
      i !== t.id && !this.windows.has(i) && (n.hidden = !0, n.tab = void 0);
    let s = this.views.get(t.id);
    s || (s = document.createElement("korvus-view"), this.views.set(t.id, s), this.el.pages.insertBefore(s, this.el.windows)), s.hidden = !1, s.tab = t;
    const e = (i, n) => {
      t.addEventListener(i, n), this.tabOff.push(() => t.removeEventListener(i, n));
    };
    e("info", () => {
      this.syncBar(), this.renderTabs();
    }), e("history", () => this.syncBar()), e("loading", () => {
      this.syncBar(), this.renderTabs();
    }), e("refused", () => this.setState("view-only")), e("dialog", () => this.syncDialog()), this.syncDialog(), this.syncBar(), this.renderTabs();
  }
  syncDialog() {
    this.el.pages.toggleAttribute("data-dialog", !!this.active?.dialog);
  }
  syncBar() {
    const t = this.active;
    this.el.back.disabled = !t?.canGoBack, this.el.forward.disabled = !t?.canGoForward, this.el.reload.disabled = !t, this.el.reload.innerHTML = t?.loading ? c.stop : c.reload, this.el.reload.setAttribute("aria-label", t?.loading ? "Stop" : "Reload"), this.el.reload.title = t?.loading ? "Stop" : "Reload", this.root.activeElement !== this.el.address && (this.el.address.value = g(t?.info.url ?? ""));
  }
  // ── popups: windows over the page ─────────────────────────────────────────────────────────────
  onPopup(t) {
    const s = t.detail, e = new CustomEvent("korvus-popup", { detail: { tab: s }, cancelable: !0 });
    if (!this.dispatchEvent(e)) {
      t.preventDefault();
      return;
    }
    const i = document.createElement("section");
    i.className = "window", i.setAttribute("aria-label", "Popup window");
    const n = this.el.pages.getBoundingClientRect(), a = Math.min(520, Math.max(280, n.width - 48)), r = Math.min(640, Math.max(200, n.height - 48)), d = this.windows.size;
    i.style.width = `${a}px`, i.style.height = `${r}px`, i.style.left = `${Math.max(0, n.width - a - 24 - d * 24)}px`, i.style.top = `${Math.max(0, 24 + d * 24)}px`, i.innerHTML = `<header><span class="title"></span><span class="host"></span>
      <button class="x" type="button" aria-label="Close window">${c.close}</button></header>`;
    const p = document.createElement("korvus-view");
    i.append(p), this.el.windows.append(i), this.windows.set(s.id, i), this.views.set(s.id, p), p.tab = s;
    const x = i.querySelector(".title"), f = i.querySelector(".host"), w = () => {
      x.textContent = s.info.title || "Untitled";
      try {
        f.textContent = new URL(s.info.url).host;
      } catch {
        f.textContent = "";
      }
    };
    w(), s.addEventListener("info", w), s.addEventListener("closed", () => {
      i.remove(), this.windows.delete(s.id), this.views.delete(s.id);
    }), i.querySelector(".x").addEventListener("click", () => void this.client?.closeTab(s)), this.dragBy(i, i.querySelector("header")), queueMicrotask(() => p.focusPage());
  }
  dragBy(t, s) {
    s.addEventListener("pointerdown", (e) => {
      if (e.target.closest("button")) return;
      s.setPointerCapture(e.pointerId);
      const i = e.clientX - t.offsetLeft, n = e.clientY - t.offsetTop, a = (d) => {
        t.style.left = `${d.clientX - i}px`, t.style.top = `${Math.max(0, d.clientY - n)}px`;
      }, r = () => {
        s.removeEventListener("pointermove", a), s.removeEventListener("pointerup", r);
      };
      s.addEventListener("pointermove", a), s.addEventListener("pointerup", r);
    });
  }
}
function l(o) {
  return (o.altKey ? h.Alt : 0) | (o.ctrlKey ? h.Ctrl : 0) | (o.metaKey ? h.Meta : 0) | (o.shiftKey ? h.Shift : 0);
}
const b = ["left", "middle", "right"];
function M(o) {
  const t = atob(o), s = new Uint8Array(t.length);
  for (let e = 0; e < t.length; e++) s[e] = t.charCodeAt(e);
  return s;
}
class P extends HTMLElement {
  static observedAttributes = ["endpoint", "target"];
  root;
  canvas;
  ctx;
  keys;
  dialogEl;
  note;
  current;
  off = [];
  frame;
  /** Where the last frame was drawn, in CSS pixels of this element. */
  drawn = { x: 0, y: 0, w: 0, h: 0 };
  resizeTimer;
  pendingMove;
  ownClient;
  composing = !1;
  resizer = new ResizeObserver(() => this.resized());
  constructor() {
    super(), this.root = this.attachShadow({ mode: "open" }), this.root.innerHTML = `<style>${S}</style>
      <div class="stage">
        <canvas part="screen"></canvas>
        <textarea class="keys" aria-label="Type into the page" autocapitalize="off" autocomplete="off" spellcheck="false"></textarea>
        <div class="note" hidden></div>
        <div class="dialog-layer" hidden>
          <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="dlg-from" aria-describedby="dlg-msg">
            <p class="from" id="dlg-from"></p>
            <p class="message" id="dlg-msg"></p>
            <input class="prompt" type="text" aria-label="Answer">
            <div class="buttons"><button class="cancel" type="button">Cancel</button><button class="ok" type="button">OK</button></div>
          </div>
        </div>
      </div>`, this.canvas = this.root.querySelector("canvas"), this.ctx = this.canvas.getContext("2d"), this.keys = this.root.querySelector(".keys"), this.dialogEl = this.root.querySelector(".dialog-layer"), this.note = this.root.querySelector(".note"), this.wireInput(), this.wireDialog();
  }
  connectedCallback() {
    this.resizer.observe(this), !this.current && this.getAttribute("endpoint") && this.openOwn();
  }
  disconnectedCallback() {
    this.resizer.disconnect(), this.current?.stopScreencast();
  }
  attributeChangedCallback() {
    this.isConnected && this.getAttribute("endpoint") && this.openOwn();
  }
  /** The page this view shows and drives. */
  get tab() {
    return this.current;
  }
  set tab(t) {
    if (t === this.current) return;
    for (const e of this.off) e();
    if (this.off = [], this.current?.stopScreencast(), this.current = t, this.frame = void 0, this.paint(), this.showDialog(), !t) return;
    const s = (e, i) => {
      t.addEventListener(e, i), this.off.push(() => t.removeEventListener(e, i));
    };
    s("frame", (e) => void this.onFrame(e.detail)), s("dialog", () => this.showDialog()), s("closed", () => this.say("This page was closed.")), this.say(""), this.resized(!0);
  }
  /** Put the keyboard focus on the page. */
  focusPage() {
    this.keys.focus({ preventScroll: !0 });
  }
  async openOwn() {
    const t = this.getAttribute("endpoint"), s = this.getAttribute("target");
    this.ownClient?.close(), this.say("Connecting…");
    try {
      const e = await v.connect(t);
      this.ownClient = e;
      const i = s && e.tabs.get(s) || e.pages[0] || await e.newTab();
      this.tab = i, e.addEventListener("close", () => this.say("The connection ended."));
    } catch (e) {
      this.say(e instanceof Error ? e.message : String(e));
    }
  }
  say(t) {
    this.note.textContent = t, this.note.hidden = !t;
  }
  // ── the picture ───────────────────────────────────────────────────────────────────────────────
  /** The element's size changed (or a tab arrived): size the page's window to it, and the frames to its pixels. */
  resized(t = !1) {
    window.clearTimeout(this.resizeTimer);
    const s = () => {
      const e = this.current, i = this.clientWidth, n = this.clientHeight;
      if (!e || i < 2 || n < 2) return;
      const a = window.devicePixelRatio || 1;
      this.canvas.width = Math.round(i * a), this.canvas.height = Math.round(n * a), this.paint(), e.setPageSize(i, n).then(() => e.startScreencast(i * a, n * a));
    };
    t ? s() : this.resizeTimer = window.setTimeout(s, 120);
  }
  async onFrame(t) {
    const s = new Blob([M(t.data)], { type: `image/${t.format}` }), e = await createImageBitmap(s);
    this.frame?.bitmap.close(), this.frame = { bitmap: e, deviceWidth: t.deviceWidth || e.width, deviceHeight: t.deviceHeight || e.height }, this.paint();
  }
  paint() {
    const t = this.ctx, s = window.devicePixelRatio || 1;
    t.setTransform(1, 0, 0, 1, 0, 0), t.clearRect(0, 0, this.canvas.width, this.canvas.height);
    const e = this.frame;
    if (!e) return;
    const i = this.canvas.width / s, n = this.canvas.height / s, a = Math.min(i / e.deviceWidth, n / e.deviceHeight), r = e.deviceWidth * a, d = e.deviceHeight * a;
    this.drawn = { x: (i - r) / 2, y: (n - d) / 2, w: r, h: d }, t.imageSmoothingQuality = "high", t.drawImage(e.bitmap, this.drawn.x * s, this.drawn.y * s, r * s, d * s);
  }
  /** A point of this element in the page's CSS pixels. */
  toPage(t) {
    const s = this.canvas.getBoundingClientRect(), e = this.frame, i = e?.deviceWidth ?? this.drawn.w, n = e?.deviceHeight ?? this.drawn.h, a = (t.clientX - s.left - this.drawn.x) / (this.drawn.w || 1) * i, r = (t.clientY - s.top - this.drawn.y) / (this.drawn.h || 1) * n;
    return { x: Math.max(0, Math.min(i, a)), y: Math.max(0, Math.min(n, r)) };
  }
  // ── input ─────────────────────────────────────────────────────────────────────────────────────
  wireInput() {
    const t = this.canvas;
    t.addEventListener("pointerdown", (e) => {
      if (e.pointerType === "mouse" || e.pointerType === "pen" || e.pointerType === "touch") {
        t.setPointerCapture(e.pointerId), this.placeKeys(e), this.focusPage();
        const i = this.toPage(e);
        this.current?.mouse("mousePressed", i.x, i.y, b[e.button] ?? "left", e.buttons, Math.max(1, e.detail), l(e)), e.preventDefault();
      }
    }), t.addEventListener("pointerup", (e) => {
      const i = this.toPage(e);
      this.current?.mouse("mouseReleased", i.x, i.y, b[e.button] ?? "left", e.buttons, Math.max(1, e.detail), l(e));
    }), t.addEventListener("pointermove", (e) => {
      const i = !this.pendingMove;
      this.pendingMove = e, i && requestAnimationFrame(() => {
        const n = this.pendingMove;
        this.pendingMove = void 0;
        const a = this.toPage(n);
        this.current?.mouse("mouseMoved", a.x, a.y, "none", n.buttons, 0, l(n));
      });
    }), t.addEventListener("contextmenu", (e) => e.preventDefault()), t.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const i = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? this.clientHeight : 1, n = this.toPage(e);
        this.current?.wheel(n.x, n.y, e.deltaX * i, e.deltaY * i, l(e));
      },
      { passive: !1 }
    );
    const s = this.keys;
    s.addEventListener("keydown", (e) => {
      if (e.isComposing || e.keyCode === 229 || this.composing) return;
      const i = this.current;
      if (!i) return;
      const n = e.ctrlKey || e.metaKey, r = e.key.length === 1 && !n ? e.key : e.key === "Enter" ? "\r" : "";
      i.key({
        type: r ? "keyDown" : "rawKeyDown",
        key: e.key,
        code: e.code,
        windowsVirtualKeyCode: e.keyCode,
        nativeVirtualKeyCode: e.keyCode,
        modifiers: l(e),
        text: r,
        unmodifiedText: r,
        autoRepeat: e.repeat,
        location: e.location
      }), n && (e.key === "v" || e.key === "V") || e.preventDefault();
    }), s.addEventListener("keyup", (e) => {
      e.isComposing || e.keyCode === 229 || (this.current?.key({
        type: "keyUp",
        key: e.key,
        code: e.code,
        windowsVirtualKeyCode: e.keyCode,
        nativeVirtualKeyCode: e.keyCode,
        modifiers: l(e),
        location: e.location
      }), e.preventDefault());
    }), s.addEventListener("compositionstart", () => {
      this.composing = !0;
    }), s.addEventListener("compositionupdate", (e) => {
      this.current?.compose(e.data ?? "");
    }), s.addEventListener("compositionend", (e) => {
      this.composing = !1, this.current?.insertText(e.data ?? ""), queueMicrotask(() => s.value = "");
    }), s.addEventListener("input", (e) => {
      const i = e;
      i.isComposing || this.composing || (i.inputType === "insertText" && i.data && this.current?.insertText(i.data), s.value = "");
    }), s.addEventListener("paste", (e) => {
      const i = e.clipboardData?.getData("text/plain");
      i && this.current?.insertText(i), e.preventDefault();
    });
  }
  /** The hidden field that takes the keyboard sits where the person clicked, so an IME's window opens there. */
  placeKeys(t) {
    const s = this.getBoundingClientRect();
    this.keys.style.left = `${Math.max(0, t.clientX - s.left)}px`, this.keys.style.top = `${Math.max(0, t.clientY - s.top)}px`;
  }
  // ── dialogs ───────────────────────────────────────────────────────────────────────────────────
  wireDialog() {
    const t = this.root.querySelector(".ok"), s = this.root.querySelector(".cancel"), e = this.root.querySelector(".prompt");
    t.addEventListener("click", () => void this.current?.answerDialog(!0, this.current.dialog?.type === "prompt" ? e.value : void 0)), s.addEventListener("click", () => void this.current?.answerDialog(!1)), this.dialogEl.addEventListener("keydown", (i) => {
      i.key === "Enter" ? t.click() : i.key === "Escape" && s.click();
    });
  }
  showDialog() {
    const t = this.current?.dialog;
    if (this.dialogEl.hidden = !t, !t) return;
    let s = "This page";
    try {
      const n = new URL(t.url);
      n.host && (s = n.host);
    } catch {
    }
    this.root.querySelector(".from").textContent = `${s} says`, this.root.querySelector(".message").textContent = t.message;
    const e = this.root.querySelector(".prompt");
    e.hidden = t.type !== "prompt", e.value = t.defaultPrompt, this.root.querySelector(".cancel").hidden = t.type === "alert";
    const i = t.type === "prompt" ? e : this.root.querySelector(".ok");
    queueMicrotask(() => i.focus());
  }
}
customElements.get("korvus-view") || customElements.define("korvus-view", P);
customElements.get("korvus-browser") || customElements.define("korvus-browser", L);
export {
  u as CdpError,
  v as Client,
  E as Connection,
  L as KorvusBrowser,
  P as KorvusView,
  h as Mod,
  T as Tab,
  I as addressFor
};
