// The headless core: one endpoint's tabs and popups, kept current from the target events. No UI -- build your own on
// it, or use `<korvus-browser>` / `<korvus-view>`.

import { Connection, type CdpEvent } from "./cdp.js";
import { Tab, type TargetInfo } from "./tab.js";

/**
 * Events: `tabs` (a tab came or went, or the order changed), `popup` (a `CustomEvent<Tab>` for a page a script
 * opened -- cancelable; the browser element shows it as a window unless a listener prevents that), `close` (the
 * connection ended).
 */
export class Client extends EventTarget {
  readonly tabs = new Map<string, Tab>();
  private bySession = new Map<string, Tab>();
  private started = false;
  private titleTimer?: number;

  constructor(readonly conn: Connection) {
    super();
    conn.on((e) => this.onEvent(e));
    conn.addEventListener("close", () => {
      window.clearInterval(this.titleTimer);
      this.dispatchEvent(new Event("close"));
    });
  }

  /** Connect to `endpoint` (a DevTools WebSocket URL; a korvus viewer's carries its `?ticket=`). */
  static async connect(endpoint: string): Promise<Client> {
    const conn = new Connection(endpoint);
    await conn.opened;
    const c = new Client(conn);
    await c.start();
    return c;
  }

  /** The tabs a person sees in the tab strip, oldest first: every page but the popups. */
  get pages(): Tab[] {
    return [...this.tabs.values()].filter((t) => !t.info.openerId);
  }

  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;
    await this.conn.send("Target.setDiscoverTargets", { discover: true });
    const r = await this.conn.send<{ targetInfos: TargetInfo[] }>("Target.getTargets");
    // Chrome and korvus list the newest first; the strip shows them in the order they were opened
    for (const info of r.targetInfos.reverse()) this.add(info, false);
    this.dispatchEvent(new Event("tabs"));
    // A page's title reaches CDP's target events only with its next URL (Chrome announces a title a script sets at the
    // next navigation, if ever), so the tabs' titles are read again now and then, and after each load.
    this.titleTimer = window.setInterval(() => void this.refreshInfo(), 2500);
  }

  /** Read every tab's title and address again. */
  async refreshInfo(): Promise<void> {
    try {
      const r = await this.conn.send<{ targetInfos: TargetInfo[] }>("Target.getTargets");
      for (const info of r.targetInfos) {
        const t = this.tabs.get(info.targetId);
        if (t && (t.info.title !== info.title || t.info.url !== info.url)) t.update(info);
      }
    } catch {
      // the connection is going; its close says so
    }
  }

  /** A new tab at `url`; resolves with it once the endpoint has made it. */
  async newTab(url = "about:blank"): Promise<Tab> {
    const r = await this.conn.send<{ targetId: string }>("Target.createTarget", { url });
    const existing = this.tabs.get(r.targetId);
    if (existing) return existing;
    const t = this.add({ targetId: r.targetId, type: "page", title: "", url }, false);
    this.dispatchEvent(new Event("tabs"));
    return t!;
  }

  closeTab(tab: Tab): Promise<unknown> {
    return this.conn.send("Target.closeTarget", { targetId: tab.id }).catch(() => {});
  }

  close(): void {
    this.conn.close();
  }

  private add(info: TargetInfo, announce: boolean): Tab | undefined {
    if (info.type !== "page") return undefined;
    let t = this.tabs.get(info.targetId);
    if (t) {
      t.update(info);
      return t;
    }
    t = new Tab(this.conn, info, (tab) => {
      if (tab.sessionId) this.bySession.set(tab.sessionId, tab);
    });
    this.tabs.set(info.targetId, t);
    void t.attach().catch(() => {});
    if (announce && info.openerId) {
      this.dispatchEvent(new CustomEvent<Tab>("popup", { detail: t, cancelable: true }));
    }
    return t;
  }

  private onEvent(e: CdpEvent): void {
    if (e.sessionId) {
      this.bySession.get(e.sessionId)?.handle(e);
      if (e.method === "Page.loadEventFired" || e.method === "Page.navigatedWithinDocument") {
        window.setTimeout(() => void this.refreshInfo(), 150);
        window.setTimeout(() => void this.refreshInfo(), 900);
      }
      return;
    }
    switch (e.method) {
      case "Target.targetCreated":
        if (this.add(e.params.targetInfo, true) && !e.params.targetInfo.openerId) this.dispatchEvent(new Event("tabs"));
        break;
      case "Target.targetInfoChanged":
        this.tabs.get(e.params.targetInfo.targetId)?.update(e.params.targetInfo);
        break;
      case "Target.targetDestroyed": {
        const t = this.tabs.get(e.params.targetId);
        if (!t) break;
        this.tabs.delete(t.id);
        if (t.sessionId) this.bySession.delete(t.sessionId);
        t.markClosed();
        this.dispatchEvent(new Event("tabs"));
        break;
      }
    }
  }
}
