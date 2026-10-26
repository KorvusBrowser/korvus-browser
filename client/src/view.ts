// `<korvus-view>`: one page's screen and its input. The browser element puts one in each tab and popup window; on its
// own it shows the page `endpoint` and `target` name.

import { Client } from "./client.js";
import { Mod, type Frame, type Tab } from "./tab.js";
import { viewStyles } from "./styles.js";

function modifiers(e: MouseEvent | KeyboardEvent | WheelEvent): number {
  return (e.altKey ? Mod.Alt : 0) | (e.ctrlKey ? Mod.Ctrl : 0) | (e.metaKey ? Mod.Meta : 0) | (e.shiftKey ? Mod.Shift : 0);
}

const BUTTONS = ["left", "middle", "right"] as const;

function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class KorvusView extends HTMLElement {
  static observedAttributes = ["endpoint", "target"];

  private root: ShadowRoot;
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private keys: HTMLTextAreaElement;
  private dialogEl: HTMLDivElement;
  private note: HTMLDivElement;
  private current?: Tab;
  private off: Array<() => void> = [];
  private frame?: { bitmap: ImageBitmap; deviceWidth: number; deviceHeight: number };
  /** Where the last frame was drawn, in CSS pixels of this element. */
  private drawn = { x: 0, y: 0, w: 0, h: 0 };
  private resizeTimer?: number;
  private pendingMove?: PointerEvent;
  private ownClient?: Client;
  private composing = false;
  private resizer = new ResizeObserver(() => this.resized());

  constructor() {
    super();
    this.root = this.attachShadow({ mode: "open" });
    this.root.innerHTML = `<style>${viewStyles}</style>
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
      </div>`;
    this.canvas = this.root.querySelector("canvas")!;
    this.ctx = this.canvas.getContext("2d")!;
    this.keys = this.root.querySelector(".keys")!;
    this.dialogEl = this.root.querySelector(".dialog-layer")!;
    this.note = this.root.querySelector(".note")!;
    this.wireInput();
    this.wireDialog();
  }

  connectedCallback(): void {
    this.resizer.observe(this);
    if (!this.current && this.getAttribute("endpoint")) void this.openOwn();
  }

  disconnectedCallback(): void {
    this.resizer.disconnect();
    void this.current?.stopScreencast();
  }

  attributeChangedCallback(): void {
    if (this.isConnected && this.getAttribute("endpoint")) void this.openOwn();
  }

  /** The page this view shows and drives. */
  get tab(): Tab | undefined {
    return this.current;
  }

  set tab(t: Tab | undefined) {
    if (t === this.current) return;
    for (const f of this.off) f();
    this.off = [];
    void this.current?.stopScreencast();
    this.current = t;
    this.frame = undefined;
    this.paint();
    this.showDialog();
    if (!t) return;
    const on = (type: string, fn: (e: Event) => void) => {
      t.addEventListener(type, fn);
      this.off.push(() => t.removeEventListener(type, fn));
    };
    on("frame", (e) => void this.onFrame((e as CustomEvent<Frame>).detail));
    on("dialog", () => this.showDialog());
    on("closed", () => this.say("This page was closed."));
    this.say("");
    this.resized(true);
  }

  /** Put the keyboard focus on the page. */
  focusPage(): void {
    this.keys.focus({ preventScroll: true });
  }

  private async openOwn(): Promise<void> {
    const endpoint = this.getAttribute("endpoint")!;
    const target = this.getAttribute("target");
    this.ownClient?.close();
    this.say("Connecting…");
    try {
      const c = await Client.connect(endpoint);
      this.ownClient = c;
      const t = (target && c.tabs.get(target)) || c.pages[0] || (await c.newTab());
      this.tab = t;
      c.addEventListener("close", () => this.say("The connection ended."));
    } catch (e) {
      this.say(e instanceof Error ? e.message : String(e));
    }
  }

  private say(text: string): void {
    this.note.textContent = text;
    this.note.hidden = !text;
  }

  // ── the picture ───────────────────────────────────────────────────────────────────────────────

  /** The element's size changed (or a tab arrived): size the page's window to it, and the frames to its pixels. */
  private resized(now = false): void {
    window.clearTimeout(this.resizeTimer);
    const run = () => {
      const t = this.current;
      const w = this.clientWidth;
      const h = this.clientHeight;
      if (!t || w < 2 || h < 2) return;
      const dpr = window.devicePixelRatio || 1;
      this.canvas.width = Math.round(w * dpr);
      this.canvas.height = Math.round(h * dpr);
      this.paint();
      void t.setPageSize(w, h).then(() => t.startScreencast(w * dpr, h * dpr));
    };
    if (now) run();
    else this.resizeTimer = window.setTimeout(run, 120);
  }

  private async onFrame(f: Frame): Promise<void> {
    const blob = new Blob([decode(f.data) as BlobPart], { type: `image/${f.format}` });
    const bitmap = await createImageBitmap(blob);
    this.frame?.bitmap.close();
    this.frame = { bitmap, deviceWidth: f.deviceWidth || bitmap.width, deviceHeight: f.deviceHeight || bitmap.height };
    this.paint();
  }

  private paint(): void {
    const c = this.ctx;
    const dpr = window.devicePixelRatio || 1;
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.clearRect(0, 0, this.canvas.width, this.canvas.height);
    const f = this.frame;
    if (!f) return;
    // contain: the page keeps its shape when the endpoint could not make it the view's (a persona's screen)
    const vw = this.canvas.width / dpr;
    const vh = this.canvas.height / dpr;
    const s = Math.min(vw / f.deviceWidth, vh / f.deviceHeight);
    const w = f.deviceWidth * s;
    const h = f.deviceHeight * s;
    this.drawn = { x: (vw - w) / 2, y: (vh - h) / 2, w, h };
    c.imageSmoothingQuality = "high";
    c.drawImage(f.bitmap, this.drawn.x * dpr, this.drawn.y * dpr, w * dpr, h * dpr);
  }

  /** A point of this element in the page's CSS pixels. */
  private toPage(e: MouseEvent): { x: number; y: number } {
    const r = this.canvas.getBoundingClientRect();
    const f = this.frame;
    const dw = f?.deviceWidth ?? this.drawn.w;
    const dh = f?.deviceHeight ?? this.drawn.h;
    const x = ((e.clientX - r.left - this.drawn.x) / (this.drawn.w || 1)) * dw;
    const y = ((e.clientY - r.top - this.drawn.y) / (this.drawn.h || 1)) * dh;
    return { x: Math.max(0, Math.min(dw, x)), y: Math.max(0, Math.min(dh, y)) };
  }

  // ── input ─────────────────────────────────────────────────────────────────────────────────────

  private wireInput(): void {
    const cv = this.canvas;
    cv.addEventListener("pointerdown", (e) => {
      if (e.pointerType === "mouse" || e.pointerType === "pen" || e.pointerType === "touch") {
        cv.setPointerCapture(e.pointerId);
        this.placeKeys(e);
        this.focusPage();
        const p = this.toPage(e);
        this.current?.mouse("mousePressed", p.x, p.y, BUTTONS[e.button] ?? "left", e.buttons, Math.max(1, e.detail), modifiers(e));
        e.preventDefault();
      }
    });
    cv.addEventListener("pointerup", (e) => {
      const p = this.toPage(e);
      this.current?.mouse("mouseReleased", p.x, p.y, BUTTONS[e.button] ?? "left", e.buttons, Math.max(1, e.detail), modifiers(e));
    });
    cv.addEventListener("pointermove", (e) => {
      // one move per display frame, as a browser delivers them
      const first = !this.pendingMove;
      this.pendingMove = e;
      if (!first) return;
      requestAnimationFrame(() => {
        const m = this.pendingMove!;
        this.pendingMove = undefined;
        const p = this.toPage(m);
        this.current?.mouse("mouseMoved", p.x, p.y, "none", m.buttons, 0, modifiers(m));
      });
    });
    cv.addEventListener("contextmenu", (e) => e.preventDefault());
    cv.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? this.clientHeight : 1;
        const p = this.toPage(e);
        this.current?.wheel(p.x, p.y, e.deltaX * unit, e.deltaY * unit, modifiers(e));
      },
      { passive: false },
    );

    const k = this.keys;
    k.addEventListener("keydown", (e) => {
      // an IME's keys: its composition events say what it typed
      if (e.isComposing || e.keyCode === 229 || this.composing) return;
      const t = this.current;
      if (!t) return;
      const shortcut = e.ctrlKey || e.metaKey;
      const printable = e.key.length === 1 && !shortcut;
      const text = printable ? e.key : e.key === "Enter" ? "\r" : "";
      t.key({
        type: text ? "keyDown" : "rawKeyDown",
        key: e.key,
        code: e.code,
        windowsVirtualKeyCode: e.keyCode,
        nativeVirtualKeyCode: e.keyCode,
        modifiers: modifiers(e),
        text,
        unmodifiedText: text,
        autoRepeat: e.repeat,
        location: e.location,
      });
      // paste comes as its own event (and leaves the host's clipboard alone otherwise)
      if (!(shortcut && (e.key === "v" || e.key === "V"))) e.preventDefault();
    });
    k.addEventListener("keyup", (e) => {
      if (e.isComposing || e.keyCode === 229) return;
      this.current?.key({
        type: "keyUp",
        key: e.key,
        code: e.code,
        windowsVirtualKeyCode: e.keyCode,
        nativeVirtualKeyCode: e.keyCode,
        modifiers: modifiers(e),
        location: e.location,
      });
      e.preventDefault();
    });
    k.addEventListener("compositionstart", () => {
      this.composing = true;
    });
    k.addEventListener("compositionupdate", (e) => {
      this.current?.compose(e.data ?? "");
    });
    k.addEventListener("compositionend", (e) => {
      this.composing = false;
      this.current?.insertText(e.data ?? "");
      queueMicrotask(() => (k.value = ""));
    });
    // a keyboard that types without key events (a phone's, dictation, an emoji picker)
    k.addEventListener("input", (e) => {
      const ie = e as InputEvent;
      if (ie.isComposing || this.composing) return;
      if (ie.inputType === "insertText" && ie.data) this.current?.insertText(ie.data);
      k.value = "";
    });
    k.addEventListener("paste", (e) => {
      const text = e.clipboardData?.getData("text/plain");
      if (text) this.current?.insertText(text);
      e.preventDefault();
    });
  }

  /** The hidden field that takes the keyboard sits where the person clicked, so an IME's window opens there. */
  private placeKeys(e: MouseEvent): void {
    const r = this.getBoundingClientRect();
    this.keys.style.left = `${Math.max(0, e.clientX - r.left)}px`;
    this.keys.style.top = `${Math.max(0, e.clientY - r.top)}px`;
  }

  // ── dialogs ───────────────────────────────────────────────────────────────────────────────────

  private wireDialog(): void {
    const ok = this.root.querySelector<HTMLButtonElement>(".ok")!;
    const cancel = this.root.querySelector<HTMLButtonElement>(".cancel")!;
    const prompt = this.root.querySelector<HTMLInputElement>(".prompt")!;
    ok.addEventListener("click", () => void this.current?.answerDialog(true, this.current.dialog?.type === "prompt" ? prompt.value : undefined));
    cancel.addEventListener("click", () => void this.current?.answerDialog(false));
    this.dialogEl.addEventListener("keydown", (e) => {
      if (e.key === "Enter") ok.click();
      else if (e.key === "Escape") cancel.click();
    });
  }

  private showDialog(): void {
    const d = this.current?.dialog;
    this.dialogEl.hidden = !d;
    if (!d) return;
    let from = "This page";
    try {
      const u = new URL(d.url);
      if (u.host) from = u.host;
    } catch {
      // not a URL with a host
    }
    this.root.querySelector(".from")!.textContent = `${from} says`;
    this.root.querySelector(".message")!.textContent = d.message;
    const prompt = this.root.querySelector<HTMLInputElement>(".prompt")!;
    prompt.hidden = d.type !== "prompt";
    prompt.value = d.defaultPrompt;
    this.root.querySelector<HTMLButtonElement>(".cancel")!.hidden = d.type === "alert";
    const focus = d.type === "prompt" ? prompt : this.root.querySelector<HTMLButtonElement>(".ok")!;
    queueMicrotask(() => focus.focus());
  }
}
