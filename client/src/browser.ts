// `<korvus-browser>`: a browser in a page -- tabs, the address bar, back / forward / reload, popups as windows,
// dialogs -- on any DevTools endpoint: korvus's viewer channel (`endpoint="ws://host/?ticket=…"`), korvus's
// automation channel, or a Chrome started with `--remote-allow-origins`.
//
// Attributes: `endpoint` (required), `home` (a new tab's page, `about:blank`), `search` (where text that is not an
// address goes, `%s` for the words), `persona` (shown in the session capsule).
// Events: `korvus-popup` (cancelable; `detail.tab`) before a popup opens as a window; `korvus-state` when the
// connection's state changes (`detail.state`: connecting, live, view-only, off).

import { Client } from "./client.js";
import { browserStyles } from "./styles.js";
import type { Tab } from "./tab.js";
import { KorvusView } from "./view.js";

const icon = {
  back: `<svg viewBox="0 0 16 16"><path d="M10 3 5 8l5 5"/></svg>`,
  forward: `<svg viewBox="0 0 16 16"><path d="m6 3 5 5-5 5"/></svg>`,
  reload: `<svg viewBox="0 0 16 16"><path d="M13 8a5 5 0 1 1-1.6-3.7"/><path d="M13 2.5V5h-2.5"/></svg>`,
  stop: `<svg viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8"/></svg>`,
  close: `<svg viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8"/></svg>`,
  plus: `<svg viewBox="0 0 16 16"><path d="M8 3v10M3 8h10"/></svg>`,
};

type State = "connecting" | "live" | "view-only" | "off";

/** What the address bar's text means: an address, or words to search for. */
export function addressFor(input: string, search: string): string {
  const s = input.trim();
  if (!s) return "about:blank";
  if (/^[a-z][a-z0-9+.-]*:/i.test(s) && !/^[^\s/]+:\d+/.test(s)) return s; // has a scheme (and is not host:port)
  if (!/\s/.test(s) && (/^localhost(:\d+)?(\/|$)/i.test(s) || /^[^\s/]+\.[^\s/]{2,}(\/|$|:)/.test(s) || /^\d+\.\d+\.\d+\.\d+/.test(s))) {
    return (/^(localhost|127\.|\d+\.\d+\.\d+\.\d+)/i.test(s) ? "http://" : "https://") + s;
  }
  return search.replace("%s", encodeURIComponent(s));
}

/** The address as the bar shows it: a blank page shows nothing, to type into. */
function shown(url: string): string {
  return url === "about:blank" ? "" : url;
}

export class KorvusBrowser extends HTMLElement {
  static observedAttributes = ["endpoint", "persona"];

  private root: ShadowRoot;
  private client?: Client;
  private active?: Tab;
  private views = new Map<string, KorvusView>();
  private windows = new Map<string, HTMLElement>();
  private state: State = "connecting";
  private el: {
    tabs: HTMLElement;
    newTab: HTMLButtonElement;
    capsule: HTMLElement;
    back: HTMLButtonElement;
    forward: HTMLButtonElement;
    reload: HTMLButtonElement;
    address: HTMLInputElement;
    pages: HTMLElement;
    empty: HTMLElement;
    windows: HTMLElement;
  };
  private tabOff: Array<() => void> = [];

  constructor() {
    super();
    this.root = this.attachShadow({ mode: "open" });
    this.root.innerHTML = `<style>${browserStyles}</style>
      <div class="strip">
        <div class="tabs" role="tablist" aria-label="Tabs"></div>
        <button class="new" type="button" aria-label="New tab" title="New tab">${icon.plus}</button>
        <div class="capsule" data-state="connecting" role="status"></div>
      </div>
      <div class="bar">
        <button class="nav back" type="button" aria-label="Back" title="Back" disabled>${icon.back}</button>
        <button class="nav forward" type="button" aria-label="Forward" title="Forward" disabled>${icon.forward}</button>
        <button class="nav reload" type="button" aria-label="Reload" title="Reload" disabled>${icon.reload}</button>
        <input class="address" type="text" aria-label="Address" placeholder="Search or type an address"
          autocomplete="off" autocapitalize="off" spellcheck="false">
      </div>
      <div class="pages">
        <div class="empty" hidden></div>
        <div class="windows"></div>
      </div>`;
    const q = <T extends Element>(s: string) => this.root.querySelector(s) as T;
    this.el = {
      tabs: q(".tabs"),
      newTab: q(".new"),
      capsule: q(".capsule"),
      back: q(".back"),
      forward: q(".forward"),
      reload: q(".reload"),
      address: q(".address"),
      pages: q(".pages"),
      empty: q(".empty"),
      windows: q(".windows"),
    };
    this.el.newTab.addEventListener("click", () => void this.openTab());
    this.el.back.addEventListener("click", () => void this.active?.back());
    this.el.forward.addEventListener("click", () => void this.active?.forward());
    this.el.reload.addEventListener("click", () => {
      const t = this.active;
      if (!t) return;
      void (t.loading ? t.stop() : t.reload());
    });
    this.el.address.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        const t = this.active;
        if (!t) return;
        const url = addressFor(this.el.address.value, this.getAttribute("search") ?? "https://www.google.com/search?q=%s");
        void t.navigate(url);
        this.el.address.blur();
        this.views.get(t.id)?.focusPage();
      } else if (e.key === "Escape") {
        this.el.address.value = shown(this.active?.info.url ?? "");
        this.el.address.select();
      }
    });
    this.el.address.addEventListener("focus", () => this.el.address.select());
    this.el.address.addEventListener("blur", () => this.syncBar());
    this.renderCapsule();
  }

  connectedCallback(): void {
    if (!this.client) void this.connect();
  }

  disconnectedCallback(): void {
    this.client?.close();
    this.client = undefined;
  }

  attributeChangedCallback(name: string, old: string | null, value: string | null): void {
    if (name === "endpoint" && old !== null && old !== value && this.isConnected) void this.connect();
    if (name === "persona") this.renderCapsule();
  }

  /** The headless core underneath, for a page that wants more than the element shows. */
  get core(): Client | undefined {
    return this.client;
  }

  /** (Re)connect to `endpoint`. */
  async connect(): Promise<void> {
    const endpoint = this.getAttribute("endpoint");
    this.client?.close();
    this.client = undefined;
    this.reset();
    if (!endpoint) {
      this.setState("off", "No endpoint to connect to.");
      return;
    }
    this.setState("connecting");
    try {
      const c = await Client.connect(endpoint);
      this.client = c;
      c.addEventListener("tabs", () => this.renderTabs());
      c.addEventListener("popup", (e) => this.onPopup(e as CustomEvent<Tab>));
      c.addEventListener("close", () => this.setState("off", "The connection ended."));
      this.setState("live");
      if (c.pages.length === 0) await this.openTab();
      else this.activate(c.pages[c.pages.length - 1]);
      this.renderTabs();
    } catch (e) {
      this.setState("off", e instanceof Error ? e.message : String(e));
    }
  }

  /** A new tab at the `home` page, made active with the address bar ready to type into. */
  async openTab(url?: string): Promise<void> {
    const c = this.client;
    if (!c) return;
    try {
      const t = await c.newTab(url ?? this.getAttribute("home") ?? "about:blank");
      this.activate(t);
      this.renderTabs();
      if (!url) this.el.address.focus();
    } catch {
      // a view-only ticket opens no tabs
      this.setState("view-only");
    }
  }

  private reset(): void {
    for (const v of this.views.values()) v.remove();
    for (const w of this.windows.values()) w.remove();
    this.views.clear();
    this.windows.clear();
    this.active = undefined;
    this.el.tabs.replaceChildren();
  }

  private setState(s: State, why = ""): void {
    this.state = s;
    this.renderCapsule();
    this.el.empty.hidden = s !== "off";
    this.el.empty.textContent = why;
    this.el.newTab.disabled = s !== "live";
    this.dispatchEvent(new CustomEvent("korvus-state", { detail: { state: s, reason: why } }));
  }

  private renderCapsule(): void {
    const c = this.el?.capsule;
    if (!c) return;
    c.dataset.state = this.state;
    const label = { connecting: "Connecting", live: "Live", "view-only": "View only", off: "Disconnected" }[this.state];
    const persona = this.getAttribute("persona");
    c.innerHTML = `<span class="dot" aria-hidden="true"></span><span class="label"></span>`;
    c.querySelector(".label")!.textContent = label;
    if (persona) {
      const p = document.createElement("span");
      p.className = "persona";
      p.textContent = persona;
      c.append(p);
    }
    if (this.state === "off" && this.getAttribute("endpoint")) {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "Reconnect";
      b.addEventListener("click", () => void this.connect());
      c.append(b);
    }
  }

  // ── tabs ──────────────────────────────────────────────────────────────────────────────────────

  private renderTabs(): void {
    const c = this.client;
    if (!c) return;
    const pages = c.pages;
    if (this.active && !c.tabs.has(this.active.id)) this.active = undefined;
    if (!this.active && pages.length) this.activate(pages[pages.length - 1]);
    const strip = this.el.tabs;
    strip.replaceChildren(
      ...pages.map((t) => {
        const el = document.createElement("div");
        el.className = "tab";
        el.setAttribute("role", "tab");
        el.setAttribute("aria-selected", String(t === this.active));
        const title = document.createElement("button");
        title.type = "button";
        title.className = "title";
        title.textContent = t.info.title || shown(t.info.url) || "New tab";
        title.title = title.textContent;
        title.addEventListener("click", () => this.activate(t));
        const spin = document.createElement("span");
        spin.className = "spin";
        spin.hidden = !t.loading;
        const x = document.createElement("button");
        x.type = "button";
        x.className = "x";
        x.setAttribute("aria-label", `Close ${title.textContent}`);
        x.innerHTML = icon.close;
        x.addEventListener("click", () => void c.closeTab(t));
        el.append(spin, title, x);
        return el;
      }),
    );
    // closed tabs' views go
    for (const [id, v] of this.views) {
      if (!c.tabs.has(id)) {
        v.remove();
        this.views.delete(id);
      }
    }
  }

  private activate(t: Tab): void {
    if (this.active === t) return;
    for (const f of this.tabOff) f();
    this.tabOff = [];
    this.active = t;
    for (const [id, v] of this.views) {
      if (id !== t.id && !this.windows.has(id)) {
        v.hidden = true;
        v.tab = undefined; // a hidden tab casts no frames
      }
    }
    let v = this.views.get(t.id);
    if (!v) {
      v = document.createElement("korvus-view") as KorvusView;
      this.views.set(t.id, v);
      this.el.pages.insertBefore(v, this.el.windows);
    }
    v.hidden = false;
    v.tab = t;
    const on = (type: string, fn: () => void) => {
      t.addEventListener(type, fn);
      this.tabOff.push(() => t.removeEventListener(type, fn));
    };
    on("info", () => {
      this.syncBar();
      this.renderTabs();
    });
    on("history", () => this.syncBar());
    on("loading", () => {
      this.syncBar();
      this.renderTabs();
    });
    on("refused", () => this.setState("view-only"));
    // a page waiting on its dialog comes first: the windows over it step back
    on("dialog", () => this.syncDialog());
    this.syncDialog();
    this.syncBar();
    this.renderTabs();
  }

  private syncDialog(): void {
    this.el.pages.toggleAttribute("data-dialog", !!this.active?.dialog);
  }

  private syncBar(): void {
    const t = this.active;
    this.el.back.disabled = !t?.canGoBack;
    this.el.forward.disabled = !t?.canGoForward;
    this.el.reload.disabled = !t;
    this.el.reload.innerHTML = t?.loading ? icon.stop : icon.reload;
    this.el.reload.setAttribute("aria-label", t?.loading ? "Stop" : "Reload");
    this.el.reload.title = t?.loading ? "Stop" : "Reload";
    if (this.root.activeElement !== this.el.address) this.el.address.value = shown(t?.info.url ?? "");
  }

  // ── popups: windows over the page ─────────────────────────────────────────────────────────────

  private onPopup(e: CustomEvent<Tab>): void {
    const t = e.detail;
    const ev = new CustomEvent("korvus-popup", { detail: { tab: t }, cancelable: true });
    if (!this.dispatchEvent(ev)) {
      e.preventDefault();
      return;
    }
    const win = document.createElement("section");
    win.className = "window";
    win.setAttribute("aria-label", "Popup window");
    const pages = this.el.pages.getBoundingClientRect();
    const w = Math.min(520, Math.max(280, pages.width - 48));
    const h = Math.min(640, Math.max(200, pages.height - 48));
    const n = this.windows.size;
    win.style.width = `${w}px`;
    win.style.height = `${h}px`;
    // to the right, as a sign-in window opens beside the page, clear of the top centre where the page's dialogs show
    win.style.left = `${Math.max(0, pages.width - w - 24 - n * 24)}px`;
    win.style.top = `${Math.max(0, 24 + n * 24)}px`;
    win.innerHTML = `<header><span class="title"></span><span class="host"></span>
      <button class="x" type="button" aria-label="Close window">${icon.close}</button></header>`;
    const view = document.createElement("korvus-view") as KorvusView;
    win.append(view);
    this.el.windows.append(win);
    this.windows.set(t.id, win);
    this.views.set(t.id, view);
    view.tab = t;
    const title = win.querySelector(".title")!;
    const host = win.querySelector(".host")!;
    const sync = () => {
      title.textContent = t.info.title || "Untitled";
      try {
        host.textContent = new URL(t.info.url).host;
      } catch {
        host.textContent = "";
      }
    };
    sync();
    t.addEventListener("info", sync);
    t.addEventListener("closed", () => {
      win.remove();
      this.windows.delete(t.id);
      this.views.delete(t.id);
    });
    win.querySelector(".x")!.addEventListener("click", () => void this.client?.closeTab(t));
    this.dragBy(win, win.querySelector("header")!);
    queueMicrotask(() => view.focusPage());
  }

  private dragBy(win: HTMLElement, handle: HTMLElement): void {
    handle.addEventListener("pointerdown", (e) => {
      if ((e.target as Element).closest("button")) return;
      handle.setPointerCapture(e.pointerId);
      const sx = e.clientX - win.offsetLeft;
      const sy = e.clientY - win.offsetTop;
      const move = (m: PointerEvent) => {
        win.style.left = `${m.clientX - sx}px`;
        win.style.top = `${Math.max(0, m.clientY - sy)}px`;
      };
      const up = () => {
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", up);
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", up);
    });
  }
}
