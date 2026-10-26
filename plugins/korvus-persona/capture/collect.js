// ONE VISIT, EVERY REFERENCE ARTIFACT THIS PROJECT NEEDS.
//
// Korvus is built against recordings of a real browser, and until now each recording was its own
// errand: a console paste for the window surface, a second for the worker surface, a third for the
// render fingerprint, a CDP run for the behavior corpus, another for the CSS property table. They
// were taken at different times, and it shows -- the values profile describes a Mac with a 1512x982
// screen, 8 GB and an M3 Pro, and the render reference describes one with 1680x1050, 16 GB and an M5.
// Two machines, one persona.
//
// So this collects everything in one pass, from one browser, at one instant, and records the
// conditions it was collected under. It runs from a page the user simply opens -- no DevTools, no
// CDP, no automation flags -- because every one of those changes what is there to see: DevTools
// injects `$`, `$$`, `inspect` and twenty more onto the global (the current baseline carries all of
// them), and a CDP-launched throwaway profile has no extensions, no shields state and no history.
//
// WHAT IT DELIBERATELY DOES NOT DO: prompt. No `alert`, no `getUserMedia`, no `queryLocalFonts`, no
// `Notification.requestPermission`, no navigation. A capture that blocks on a dialog is a capture
// nobody finishes, and two earlier oracle runs died exactly that way.
(() => {
  "use strict";
  const T0 = Date.now();
  // The page's progress logger is moved off the global object and into this closure before anything
  // is recorded. `__capLog` and `__RF` (below) are the harness's own globals, and the first run of
  // this collector duly reported both of them as new globals of Brave 151 -- the exact contamination
  // that put `$0` and `jQuery` in the last baseline. Everything the harness needs it holds by
  // reference; the global object is left as the browser made it.
  const sink = (() => { const f = globalThis.__capLog; try { delete globalThis.__capLog; } catch (e) {} return f; })();
  const log = (msg) => { try { sink && sink(msg); } catch (e) {} };
  const guard = (fn, fallback) => { try { return fn(); } catch (e) { return fallback !== undefined ? fallback : { error: String((e && e.message) || e) }; } };
  const guardAsync = async (fn) => { try { return await fn(); } catch (e) { return { error: String((e && e.message) || e) }; } };
  // A promise that cannot hang the capture. Everything asynchronous goes through it.
  const within = (ms, p, label) => Promise.race([
    Promise.resolve(p).catch((e) => ({ error: String((e && e.message) || e) })),
    new Promise((r) => setTimeout(() => r({ error: "timeout after " + ms + "ms", label }), ms)),
  ]);
  // FNV-1a 32-bit, the same hash the render capture uses, so the two are comparable.
  const fnv = (bytes) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < bytes.length; i++) { h ^= bytes[i]; h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0; }
    return ("0000000" + h.toString(16)).slice(-8);
  };
  const fetchText = (u) => fetch(u, { cache: "no-store" }).then((r) => r.text());

  // ── how the browser was configured when this ran ────────────────────────────────────────────────
  //
  // Every number below is conditional on these, and a capture that does not state them is a capture
  // that will be misread. `farbling` is the one that matters most: Brave re-rolls a seed per session
  // and shuffles exactly the surfaces a fingerprinter reads, so a shields-up recording of
  // `navigator.plugins` is one session's dice roll, not a fact about the browser.
  const conditions = async () => {
    const c = {
      capturedAt: new Date().toISOString(),
      href: location.href,
      isSecureContext: !!globalThis.isSecureContext,
      ua: navigator.userAgent,
      isBrave: await within(2000, guardAsync(async () => !!(navigator.brave && await navigator.brave.isBrave())), "isBrave"),
      hasDevToolsApi: guard(() => ["$", "$$", "$x", "inspect", "getEventListeners"].filter((k) => k in globalThis), []),
      webdriver: guard(() => navigator.webdriver),
      // Farbling detection without needing to know Brave's algorithm: draw the same two pixels twice
      // in two canvases. Brave perturbs readback per (session, origin), so a farbled build disagrees
      // with itself across canvases while a raw one does not.
      farbling: guard(() => {
        const read = () => {
          const cv = document.createElement("canvas"); cv.width = 8; cv.height = 1;
          const x = cv.getContext("2d"); x.fillStyle = "#7f3fbf"; x.fillRect(0, 0, 8, 1);
          return Array.from(x.getImageData(0, 0, 8, 1).data).join(",");
        };
        const a = read(), b = read();
        const exact = a === new Array(8).fill("127,63,191,255").join(",");
        return { readbackStable: a === b, exactColor: exact, sample: a.slice(0, 60),
                 verdict: exact ? "off (raw pixels)" : "on (readback perturbed)" };
      }),
    };
    return c;
  };

  // ── the shared surface dumper, in every realm it can reach ──────────────────────────────────────
  //
  // The SAME file Korvus evaluates in its own isolate, so the two dumps diff directly -- that is the
  // whole reason it is a file rather than a paste. Three realms, because they are three different
  // surfaces: a Worker has no DOM and gains `WorkerGlobalScope`, and an iframe is a fresh realm whose
  // constructors are NOT the parent's (`frame.contentDocument instanceof Document` is false there,
  // which Korvus gets wrong today).
  const surfaces = async (dumperSrc) => {
    const out = {};
    log("surface: window");
    out.window = guard(() => JSON.parse((0, eval)(dumperSrc)));

    log("surface: worker");
    out.worker = await within(20000, guardAsync(() => new Promise((res, rej) => {
      const url = URL.createObjectURL(new Blob(["self.postMessage(\n" + dumperSrc + "\n);"], { type: "text/javascript" }));
      const w = new Worker(url);
      w.onmessage = (e) => { res(JSON.parse(e.data)); w.terminate(); URL.revokeObjectURL(url); };
      w.onerror = (e) => { rej(new Error("worker: " + ((e && e.message) || e))); URL.revokeObjectURL(url); };
    })), "worker");

    log("surface: shared worker");
    out.sharedWorker = await within(15000, guardAsync(() => new Promise((res, rej) => {
      if (typeof SharedWorker !== "function") return rej(new Error("no SharedWorker"));
      const body = "self.onconnect = (ev) => { const p = ev.ports[0]; p.postMessage(\n" + dumperSrc + "\n); };";
      const url = URL.createObjectURL(new Blob([body], { type: "text/javascript" }));
      const w = new SharedWorker(url);
      w.port.onmessage = (e) => { res(JSON.parse(e.data)); URL.revokeObjectURL(url); };
      w.onerror = (e) => { rej(new Error("shared worker: " + ((e && e.message) || e))); URL.revokeObjectURL(url); };
      w.port.start();
    })), "sharedWorker");

    log("surface: iframe realm");
    out.iframe = await within(15000, guardAsync(() => new Promise((res, rej) => {
      const f = document.createElement("iframe");
      f.style.cssText = "position:absolute;width:1px;height:1px;left:-9999px";
      f.srcdoc = "<!doctype html><title>r</title>";
      f.onload = () => {
        try { res(JSON.parse(f.contentWindow.eval(dumperSrc))); } catch (e) { rej(e); } finally { f.remove(); }
      };
      f.onerror = () => { rej(new Error("iframe failed")); f.remove(); };
      document.body.appendChild(f);
    })), "iframe");
    return out;
  };

  // ── the persona's values ────────────────────────────────────────────────────────────────────────
  //
  // Everything a bundle's `values.json` is made of, plus the members that were not in
  // it and turned out to be needed. Read raw: the plugin and mimeType lists are recorded as objects
  // rather than counts, because Korvus has to reproduce the entries and not merely the length.
  const persona = async () => {
    const nav = {};
    for (const k of ["userAgent", "appVersion", "appName", "appCodeName", "product", "productSub",
                     "vendor", "vendorSub", "platform", "language", "hardwareConcurrency",
                     "deviceMemory", "maxTouchPoints", "pdfViewerEnabled", "doNotTrack", "cookieEnabled",
                     "onLine", "webdriver", "globalPrivacyControl", "oscpu", "buildID", "cpuPerformance"]) {
      nav[k] = guard(() => { const v = navigator[k]; return typeof v === "object" ? null : v; }, null);
    }
    nav.languages = guard(() => Array.from(navigator.languages), []);
    // THE WIRE HALF OF `languages`, which no JS can read about its own request: the capture host
    // echoes the headers it received (`/shared/request-headers.json`) and this records the
    // `Accept-Language` the browser actually sent. A plain same-origin `fetch()` on purpose -- it is
    // the ordinary subresource request a page makes, which is the request whose header a server
    // fingerprints. Recorded rather than derived because the derivation is not the same function in
    // every browser: see `spec::NavigatorProfile::accept_language`.
    //
    // `within` answers `{error}` on a timeout, so the result is taken only when it is a string: a
    // capture that could not read the header records the empty string, which the engine treats as
    // "not captured" and answers with no header at all rather than with a preset's guess.
    const seenLang = await within(4000, guardAsync(async () => {
      const r = await fetch("/shared/request-headers.json", { cache: "no-store" });
      return (await r.json())["accept-language"];
    }), "acceptLanguage");
    nav.acceptLanguage = typeof seenLang === "string" ? seenLang : "";
    nav.plugins = guard(() => Array.from(navigator.plugins).map((p) => ({
      name: p.name, description: p.description, filename: p.filename, length: p.length,
      mimes: Array.from(p).map((m) => ({ type: m.type, suffixes: m.suffixes, description: m.description })),
    })), []);
    nav.mimeTypes = guard(() => Array.from(navigator.mimeTypes).map((m) => ({
      type: m.type, suffixes: m.suffixes, description: m.description,
      enabledPlugin: m.enabledPlugin ? m.enabledPlugin.name : null,
    })), []);
    nav.userAgentData = await within(4000, guardAsync(async () => {
      const d = navigator.userAgentData;
      if (!d) return null;
      const base = { brands: d.brands.map((b) => ({ brand: b.brand, version: b.version })), mobile: d.mobile, platform: d.platform };
      // The high-entropy hints are the ones a server asks for by Accept-CH, so they are part of the
      // identity even though the default `userAgentData` does not carry them.
      base.highEntropy = await d.getHighEntropyValues([
        "architecture", "bitness", "model", "platformVersion", "uaFullVersion", "fullVersionList", "wow64", "formFactors"]);
      return base;
    }), "uaData");
    nav.connection = guard(() => {
      const c = navigator.connection;
      return c ? { effectiveType: c.effectiveType, rtt: c.rtt, downlink: c.downlink, saveData: c.saveData, type: c.type } : null;
    }, null);
    nav.storageEstimate = await within(4000, guardAsync(() => navigator.storage && navigator.storage.estimate()), "storage");
    nav.battery = await within(4000, guardAsync(async () => {
      if (!navigator.getBattery) return null;
      const b = await navigator.getBattery();
      // Level and charging state are live; only the SHAPE is worth recording.
      return { has: true, keys: Object.getOwnPropertyNames(Object.getPrototypeOf(b)) };
    }), "battery");
    // Permission states, queried and never REQUESTED -- a request opens a prompt.
    nav.permissions = await within(8000, guardAsync(async () => {
      // The names after storage-access are where Brave and Chrome can differ beyond the
      // engine's defaults (idle-detection: Brave denies, Chrome prompts).
      const names = ["geolocation", "notifications", "camera", "microphone", "midi", "background-sync",
                     "persistent-storage", "clipboard-read", "clipboard-write", "accelerometer",
                     "gyroscope", "magnetometer", "screen-wake-lock", "payment-handler", "storage-access",
                     "idle-detection", "background-fetch", "periodic-background-sync", "keyboard-lock",
                     "pointer-lock", "display-capture", "local-fonts", "window-management",
                     "captured-surface-control", "local-network-access", "local-network", "loopback-network"];
      const out = {};
      for (const name of names) {
        out[name] = await guardAsync(async () => (await navigator.permissions.query({ name })).state);
      }
      return out;
    }), "permissions");
    nav.mediaDevices = await within(5000, guardAsync(async () => {
      if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return null;
      // Without permission the labels are empty; the KINDS and the count are still identity.
      const d = await navigator.mediaDevices.enumerateDevices();
      return { count: d.length, kinds: d.map((x) => x.kind), labelled: d.filter((x) => x.label).length,
               supported: guard(() => navigator.mediaDevices.getSupportedConstraints(), null) };
    }), "mediaDevices");

    const screenObj = {};
    for (const k of ["width", "height", "availWidth", "availHeight", "availLeft", "availTop",
                     "colorDepth", "pixelDepth", "isExtended"]) screenObj[k] = guard(() => screen[k], null);
    screenObj.orientation = guard(() => screen.orientation ? { type: screen.orientation.type, angle: screen.orientation.angle } : null, null);

    const win = {};
    for (const k of ["innerWidth", "innerHeight", "outerWidth", "outerHeight", "screenX", "screenY",
                     "screenLeft", "screenTop", "devicePixelRatio", "scrollX", "scrollY",
                     "originAgentCluster", "crossOriginIsolated"]) win[k] = guard(() => globalThis[k], null);
    win.visualViewport = guard(() => {
      const v = visualViewport;
      return v ? { width: v.width, height: v.height, scale: v.scale, offsetLeft: v.offsetLeft, offsetTop: v.offsetTop } : null;
    }, null);
    win.documentElement = guard(() => ({ clientWidth: document.documentElement.clientWidth, clientHeight: document.documentElement.clientHeight }), null);

    return {
      nav, screen: screenObj, win,
      memory: guard(() => performance.memory ? {
        jsHeapSizeLimit: performance.memory.jsHeapSizeLimit,
        totalJSHeapSize: performance.memory.totalJSHeapSize,
        usedJSHeapSize: performance.memory.usedJSHeapSize,
      } : null, null),
      chrome: guard(() => (typeof chrome === "object" && chrome) ? {
        keys: Object.keys(chrome),
        hasRuntime: !!chrome.runtime,
        loadTimes: typeof chrome.loadTimes,
        csi: typeof chrome.csi,
        app: guard(() => chrome.app ? Object.keys(chrome.app) : null, null),
      } : null, null),
    };
  };

  // ── Intl, and the media queries a page can ask ─────────────────────────────────────────────────
  //
  // The timezone is a coherence signal a detector cross-checks against the exit IP, and the media
  // queries are a surface Korvus answers from `korvus-page`'s evaluator -- a table of what a real
  // browser says is the only way to know whether the two agree.
  const environment = () => ({
    // The names `getComputedStyle` enumerates, in order (Chrome 150: 474, Chromium 151:
    // 475 with `ruby-overhang`) -- promoted into values.json `css.computedStyleProperties`.
    computedStyleProperties: guard(() => Array.from(getComputedStyle(document.documentElement)), null),
    intl: guard(() => {
      const dtf = Intl.DateTimeFormat().resolvedOptions();
      const out = { resolvedOptions: dtf, locale: dtf.locale, timeZone: dtf.timeZone,
                    offsetMinutes: new Date().getTimezoneOffset(),
                    dateString: new Date(0).toString(), toLocaleString: new Date(0).toLocaleString(),
                    numberFormat: Intl.NumberFormat().resolvedOptions(),
                    collator: Intl.Collator().resolvedOptions() };
      if (Intl.supportedValuesOf) {
        out.supportedValues = {};
        for (const k of ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit"]) {
          out.supportedValues[k] = guard(() => { const v = Intl.supportedValuesOf(k); return { count: v.length, first: v.slice(0, 8) }; }, null);
        }
      }
      return out;
    }),
    mediaQueries: guard(() => {
      const qs = ["(min-width: 0px)", "(prefers-color-scheme: dark)", "(prefers-color-scheme: light)",
                  "(prefers-reduced-motion: reduce)", "(prefers-contrast: more)", "(forced-colors: active)",
                  "(any-pointer: fine)", "(any-pointer: coarse)", "(pointer: fine)", "(hover: hover)",
                  "(any-hover: hover)", "(display-mode: browser)", "(orientation: landscape)",
                  "(color-gamut: srgb)", "(color-gamut: p3)", "(dynamic-range: high)", "(update: fast)",
                  "(scripting: enabled)", "(inverted-colors: inverted)", "(min-resolution: 2dppx)",
                  "(prefers-reduced-transparency: reduce)", "(overflow-block: scroll)"];
      const out = {};
      for (const q of qs) out[q] = guard(() => { const m = matchMedia(q); return { matches: m.matches, media: m.media }; }, null);
      return out;
    }),
    cssSupports: guard(() => {
      const out = {};
      for (const q of ["display:grid", "display:flex", "color:color(display-p3 1 0 0)", "width:1dvh",
                       "backdrop-filter:blur(1px)", "container-type:inline-size", "anchor-name:--a",
                       "field-sizing:content", "text-wrap:balance", "position:sticky", "aspect-ratio:1",
                       "scrollbar-width:thin", "view-transition-name:a", "@container", "selector(:has(a))"]) {
        out[q] = guard(() => (q.startsWith("@") || q.startsWith("selector(")) ? CSS.supports(q) : CSS.supports(q.split(":")[0], q.slice(q.indexOf(":") + 1)), null);
      }
      return out;
    }),
  });

  // ── the CSS property table ──────────────────────────────────────────────────────────────────────
  //
  // `getComputedStyle(el).length` is a one-line detect and Korvus answered 127 against 475 until this
  // was measured. Three things are recorded because three are needed and only the first is obvious:
  // the LIST, the ORDER (`cs[i]` exposes it), and the per-property INITIAL value -- which cannot be
  // read off a laid-out box, because for `width` and its family the resolved value IS the used value.
  const cssTable = () => guard(() => {
    const probe = document.createElement("div");
    document.body.appendChild(probe);
    const names = [];
    { const cs = getComputedStyle(probe); for (let i = 0; i < cs.length; i++) names.push(cs[i]); }
    const initial = {}, onDiv = {}, onBody = {};
    for (const n of names) {
      const el = document.createElement("div");
      el.style.setProperty(n, "initial");
      document.body.appendChild(el);
      initial[n] = getComputedStyle(el).getPropertyValue(n);
      el.remove();
    }
    { const cs = getComputedStyle(probe); for (const n of names) onDiv[n] = cs.getPropertyValue(n); }
    { const cs = getComputedStyle(document.body); for (const n of names) onBody[n] = cs.getPropertyValue(n); }
    probe.remove();
    // The shorthands a page reads back but `getComputedStyle` does not enumerate -- Chrome answers
    // them through `getPropertyValue` while leaving them out of the index, and Korvus needs both facts.
    const unenumerated = {};
    for (const n of ["border-spacing", "background-position-x", "background-position-y", "background-position",
                     "margin", "padding", "border", "font", "background", "grid-template", "inset", "gap"]) {
      unenumerated[n] = guard(() => getComputedStyle(document.body).getPropertyValue(n), null);
    }
    // The UA stylesheet, as seen through what it does: the display of every element it styles.
    const uaDisplay = {};
    for (const tag of ["div", "span", "p", "li", "ul", "table", "tr", "td", "thead", "tbody", "a", "b",
                       "button", "input", "select", "textarea", "img", "canvas", "video", "audio",
                       "h1", "hr", "br", "form", "label", "fieldset", "legend", "details", "summary",
                       "dialog", "template", "slot", "head", "title", "style", "script", "noscript",
                       "svg", "math", "marquee", "meter", "progress", "output", "picture", "source"]) {
      uaDisplay[tag] = guard(() => {
        const e = document.createElement(tag);
        document.body.appendChild(e);
        const cs = getComputedStyle(e);
        const r = { display: cs.display, margin: cs.marginTop + " " + cs.marginRight + " " + cs.marginBottom + " " + cs.marginLeft,
                    padding: cs.paddingTop, fontSize: cs.fontSize, fontWeight: cs.fontWeight,
                    fontFamily: cs.fontFamily, lineHeight: cs.lineHeight, color: cs.color };
        e.remove();
        return r;
      }, null);
    }
    return { count: names.length, order: names, initial, onDiv, onBody, unenumerated, uaDisplay };
  });

  // ── fonts ───────────────────────────────────────────────────────────────────────────────────────
  //
  // By measurement, not by `queryLocalFonts` -- that one opens a permission prompt, and a capture that
  // prompts is a capture nobody finishes. A family is present when text in it measures differently
  // from the three generic fallbacks, which is the same test every fingerprinting script uses.
  const fonts = () => guard(() => {
    const BASE = ["monospace", "sans-serif", "serif"];
    const TEST = "mmmmmmmmmmlli WWWWWWWWWW 0123456789";
    const cv = document.createElement("canvas");
    const ctx = cv.getContext("2d");
    const width = (family) => { ctx.font = "72px " + family; return ctx.measureText(TEST).width; };
    const baseW = {}; for (const b of BASE) baseW[b] = width(b);
    const LIST = ["Arial", "Arial Black", "Arial Narrow", "Arial Rounded MT Bold", "Helvetica",
      "Helvetica Neue", "Times", "Times New Roman", "Courier", "Courier New", "Georgia", "Palatino",
      "Palatino Linotype", "Garamond", "Bookman", "Comic Sans MS", "Trebuchet MS", "Impact",
      "Verdana", "Tahoma", "Geneva", "Lucida Console", "Lucida Grande", "Lucida Sans Unicode",
      "Monaco", "Menlo", "Consolas", "Segoe UI", "Segoe UI Emoji", "Calibri", "Cambria", "Candara",
      "Constantia", "Corbel", "Franklin Gothic Medium", "Gill Sans", "Optima", "Futura", "Baskerville",
      "American Typewriter", "Andale Mono", "Apple Chancery", "Apple Color Emoji", "AppleGothic",
      "Avenir", "Avenir Next", "Big Caslon", "Brush Script MT", "Chalkboard", "Cochin", "Copperplate",
      "Didot", "Herculanum", "Hoefler Text", "Marker Felt", "Noteworthy", "Papyrus", "Phosphate",
      "Rockwell", "Savoye LET", "SignPainter", "Skia", "Snell Roundhand", "Zapfino", "PT Sans",
      "PT Serif", "Roboto", "Noto Sans", "Noto Serif", "Noto Color Emoji", "Noto Sans CJK KR",
      "Noto Sans CJK JP", "DejaVu Sans", "DejaVu Serif", "DejaVu Sans Mono", "Liberation Sans",
      "Liberation Serif", "Liberation Mono", "Ubuntu", "Cantarell", "FreeSans", "Malgun Gothic",
      "AppleSDGothicNeo", "Apple SD Gothic Neo", "Nanum Gothic", "NanumGothic", "Batang", "Dotum",
      "Gulim", "MS Gothic", "MS PGothic", "Meiryo", "Yu Gothic", "SimSun", "SimHei", "Microsoft YaHei",
      "PingFang SC", "Hiragino Sans", "Hiragino Kaku Gothic ProN", "Songti SC", "STHeiti", "Heiti SC"];
    const present = [], widths = {};
    for (const f of LIST) {
      let hit = false;
      for (const b of BASE) { if (Math.abs(width('"' + f + '",' + b) - baseW[b]) > 0.05) { hit = true; break; } }
      if (hit) present.push(f);
      widths[f] = guard(() => width('"' + f + '",sans-serif'), null);
    }
    return {
      present, count: present.length, generic: baseW, widths,
      // `document.fonts.check` is the other question a script asks, and it can disagree with the
      // measurement -- worth having both rather than choosing.
      check: guard(() => { const o = {}; for (const f of LIST) o[f] = document.fonts.check('12px "' + f + '"'); return o; }, null),
      status: guard(() => document.fonts.status, null),
    };
  });

  // ── error messages and stack shape ──────────────────────────────────────────────────────────────
  //
  // Chrome's exact wording for a WebIDL failure ("Failed to execute 'querySelector' on 'Document': 1
  // argument required, but only 0 present.") is something a page can read and compare, and Korvus
  // reproduces it by hand -- which means having the strings. The stack FORMAT is the same kind of
  // fact one level down.
  const errors = () => {
    const probes = {
      "querySelector-arity": "document.querySelector()",
      "getElementById-arity": "document.getElementById()",
      "setAttribute-arity": "document.createElement('div').setAttribute('a')",
      "appendChild-type": "document.createElement('div').appendChild(1)",
      "appendChild-arity": "document.createElement('div').appendChild()",
      "removeChild-notfound": "document.createElement('div').removeChild(document.createElement('b'))",
      "insertBefore-type": "document.createElement('div').insertBefore(1, null)",
      "createElement-arity": "document.createElement()",
      "createElement-invalid": "document.createElement('<>')",
      "addEventListener-arity": "document.addEventListener()",
      "illegal-constructor": "new Event.prototype.constructor.call(null)",
      "Event-arity": "new Event()",
      "CustomEvent-arity": "new CustomEvent()",
      "URL-invalid": "new URL('not a url')",
      "URL-arity": "new URL()",
      "Headers-invalid": "new Headers({'a b': 'c'})",
      "Request-arity": "new Request()",
      "Response-status": "new Response('x', { status: 99 })",
      "atob-invalid": "atob('!!!')",
      "atob-arity": "atob()",
      "JSON-parse": "JSON.parse('{')",
      "null-deref": "null.x",
      "undefined-call": "undefined()",
      "not-a-function": "({}).nope()",
      "readonly-assign": "'use strict'; const e = new Event('x'); e.type = 'y'; 'no-throw'",
      "location-proto-assign": "'use strict'; Object.getPrototypeOf(location).href = 'x'; 'no-throw'",
      "Range-badnode": "document.createRange().setStart(document.doctype || document, 0)",
      "IDBKeyRange-arity": "IDBKeyRange.only()",
      "matchMedia-arity": "matchMedia()",
      "getComputedStyle-arity": "getComputedStyle()",
      "getComputedStyle-type": "getComputedStyle(1)",
      "structuredClone-fn": "structuredClone(() => {})",
      "TextDecoder-label": "new TextDecoder('no-such-encoding')",
      "canvas-getContext-arity": "document.createElement('canvas').getContext()",
      "history-pushState-arity": "history.pushState()",
      "Worker-arity": "new Worker()",
      "IntersectionObserver-arity": "new IntersectionObserver()",
      "ResizeObserver-arity": "new ResizeObserver()",
      "MutationObserver-observe-arity": "new MutationObserver(()=>{}).observe()",
      "DOMParser-arity": "new DOMParser().parseFromString()",
      "XHR-open-arity": "new XMLHttpRequest().open()",
      "XHR-send-before-open": "new XMLHttpRequest().send()",
      "crypto-getRandomValues-type": "crypto.getRandomValues([])",
      "crypto-getRandomValues-arity": "crypto.getRandomValues()",
      "AbortSignal-illegal": "new AbortSignal()",
      "Blob-bad": "new Blob(1)",
      "FileReader-illegal-state": "(() => { const r = new FileReader(); r.readAsText(new Blob(['a'])); r.readAsText(new Blob(['b'])); })()",
    };
    const out = {};
    for (const [k, expr] of Object.entries(probes)) {
      out[k] = guard(() => {
        try { const v = (0, eval)(expr); return { threw: false, value: v === undefined ? "<undefined>" : String(v).slice(0, 120) }; }
        catch (e) { return { threw: true, name: e.name, message: String(e.message).slice(0, 400), ctor: e.constructor && e.constructor.name }; }
      });
    }
    // The stack format, from three depths. Chrome writes "    at fn (url:line:col)"; the exact
    // shape -- and whether the first line repeats the message -- is what a script matches on.
    out.__stack = guard(() => {
      const inner = () => { throw new Error("probe"); };
      const middle = () => inner();
      try { middle(); } catch (e) {
        return { message: e.message, first: String(e.stack).split("\n")[0],
                 lines: String(e.stack).split("\n").slice(0, 6).map((l) => l.replace(/https?:\/\/[^\s)]+/g, "<url>")),
                 stackTraceLimit: Error.stackTraceLimit, hasCaptureStackTrace: typeof Error.captureStackTrace };
      }
    });
    out.__toString = guard(() => ({
      nativeMethod: Function.prototype.toString.call(document.querySelector),
      nativeGetter: Function.prototype.toString.call(Object.getOwnPropertyDescriptor(Navigator.prototype, "userAgent").get),
      nativeCtor: Function.prototype.toString.call(Event),
      boundFn: Function.prototype.toString.call(function f() {}.bind(null)),
      arrow: Function.prototype.toString.call(() => 1),
      toStringItself: Function.prototype.toString.call(Function.prototype.toString),
    }));
    return out;
  };

  // ── object identity: what `instanceof` and `toString` say about the things a page is handed ─────
  //
  // Five of the eight outright tells an audit found were here: `document instanceof HTMLDocument`,
  // `location instanceof Location` and three more were all false in Korvus. A table of the answers is
  // the cheapest possible regression gate.
  const identity = () => guard(() => {
    const objs = {
      window: globalThis, document, navigator, location, history, screen, crypto, performance,
      localStorage, sessionStorage, indexedDB, customElements, console,
      documentElement: document.documentElement, body: document.body,
      div: document.createElement("div"), text: document.createTextNode("x"),
      fragment: document.createDocumentFragment(), range: document.createRange(),
      style: document.createElement("div").style, classList: document.createElement("div").classList,
      dataset: document.createElement("div").dataset, attributes: document.createElement("div").attributes,
      plugins: navigator.plugins, mimeTypes: navigator.mimeTypes,
      plugin0: navigator.plugins[0], mime0: navigator.mimeTypes[0],
      event: new Event("x"), messageEvent: new MessageEvent("m"),
      headers: new Headers(), url: new URL("http://x/"), formData: new FormData(),
      abortController: new AbortController(), abortSignal: new AbortController().signal,
      textMetrics: document.createElement("canvas").getContext("2d").measureText("x"),
      rect: document.createElement("div").getBoundingClientRect(),
      computed: getComputedStyle(document.body),
      selection: getSelection(), timeline: document.timeline, fontsSet: document.fonts,
    };
    const out = {};
    for (const [k, v] of Object.entries(objs)) {
      out[k] = guard(() => {
        const chain = [];
        for (let p = Object.getPrototypeOf(v); p; p = Object.getPrototypeOf(p)) {
          chain.push({ tag: Object.prototype.toString.call(p), ctor: p.constructor && p.constructor.name,
                       ownCount: Object.getOwnPropertyNames(p).length });
          if (chain.length > 10) break;
        }
        return { tag: Object.prototype.toString.call(v), ctor: v && v.constructor && v.constructor.name,
                 own: Object.getOwnPropertyNames(v).slice(0, 40), ownCount: Object.getOwnPropertyNames(v).length,
                 symbols: Object.getOwnPropertySymbols(v).map(String), chain };
      });
    }
    // Legacy factory constructors: distinct functions sharing an element interface's prototype.
    out.__factories = guard(() => ({
      Image: { shares: Image.prototype === HTMLImageElement.prototype, length: Image.length, native: Function.prototype.toString.call(Image).includes("[native code]") },
      Audio: { shares: Audio.prototype === HTMLAudioElement.prototype, length: Audio.length },
      Option: { shares: Option.prototype === HTMLOptionElement.prototype, length: Option.length },
    }));
    return out;
  });

  // ── timing ──────────────────────────────────────────────────────────────────────────────────────
  //
  // The resolution of `performance.now` is itself a fingerprint -- Brave coarsens it, cross-origin
  // isolation changes it -- and it is measured here rather than assumed.
  const timing = () => guard(() => {
    const samples = [];
    for (let i = 0; i < 4000; i++) samples.push(performance.now());
    const deltas = [];
    for (let i = 1; i < samples.length; i++) { const d = samples[i] - samples[i - 1]; if (d > 0) deltas.push(d); }
    deltas.sort((a, b) => a - b);
    const nav = performance.getEntriesByType("navigation")[0];
    return {
      timeOrigin: typeof performance.timeOrigin,
      distinctValues: new Set(samples).size,
      minDelta: deltas[0] === undefined ? null : deltas[0],
      medianDelta: deltas.length ? deltas[Math.floor(deltas.length / 2)] : null,
      looksCoarsened: deltas.length ? deltas[0] >= 0.05 : null,
      dateNowResolution: guard(() => { const s = new Set(); for (let i = 0; i < 2000; i++) s.add(Date.now()); return s.size; }),
      entryTypes: guard(() => performance.getEntries().map((e) => e.entryType)),
      navigation: nav ? guard(() => {
        const o = {};
        for (const k of Object.getOwnPropertyNames(Object.getPrototypeOf(nav)).concat(
             Object.getOwnPropertyNames(Object.getPrototypeOf(Object.getPrototypeOf(nav))))) {
          if (k === "constructor" || k === "toJSON") continue;
          const v = nav[k];
          o[k] = (v === null || ["string", "number", "boolean"].includes(typeof v)) ? v : typeof v;
        }
        return o;
      }) : null,
      timingL1: guard(() => JSON.parse(JSON.stringify(performance.timing))),
      navigationL1: guard(() => ({ type: performance.navigation.type, redirectCount: performance.navigation.redirectCount })),
      supportedEntryTypes: guard(() => Array.from(PerformanceObserver.supportedEntryTypes)),
      // The Event Timing map's tracked event types, IN THE BROWSER'S OWN ENUMERATION ORDER (Blink's
      // hash order, which is not alphabetical and not the spec's listing order). It is a fact about
      // the build, so it belongs in the bundle rather than in a hand list -- the one-table rule asks for
      // it as a const the sync emits from the capture. No committed bundle carries it yet, so
      // `core/korvus-webapi/src/core/timing/performance.rs` holds the measured 36 until a human runs
      // the rig again; this is the line that ends that.
      eventCountKeys: guard(() => Array.from(performance.eventCounts.keys())),
    };
  });

  // ── WebGL, in full ──────────────────────────────────────────────────────────────────────────────
  //
  // Every parameter and every extension for both context versions. The render capture takes the
  // canonical probe's PIXELS; this takes the metadata, which is what a fingerprinting script reads
  // first and what the device oracle serves.
  const webglFull = () => guard(() => {
    const dump = (kind) => guard(() => {
      const cv = document.createElement("canvas"); cv.width = 64; cv.height = 64;
      const gl = cv.getContext(kind, { preserveDrawingBuffer: true });
      if (!gl) return { error: "no " + kind + " context" };
      const params = {};
      // Every numeric GL enum the context defines, asked by name -- no table to drift.
      for (const name of Object.getOwnPropertyNames(Object.getPrototypeOf(gl))) {
        const v = gl[name];
        if (typeof v !== "number") continue;
        const got = guard(() => { const p = gl.getParameter(v); return ArrayBuffer.isView(p) ? Array.from(p) : (p && typeof p === "object" ? String(p) : p); }, "<throw>");
        if (got !== null && got !== undefined && got !== "<throw>") params[name] = got;
      }
      const dbg = gl.getExtension("WEBGL_debug_renderer_info");
      const precision = {};
      for (const st of ["VERTEX_SHADER", "FRAGMENT_SHADER"]) {
        for (const pt of ["LOW_FLOAT", "MEDIUM_FLOAT", "HIGH_FLOAT", "LOW_INT", "MEDIUM_INT", "HIGH_INT"]) {
          precision[st + "." + pt] = guard(() => { const p = gl.getShaderPrecisionFormat(gl[st], gl[pt]); return p ? [p.rangeMin, p.rangeMax, p.precision] : null; }, null);
        }
      }
      // The limits an EXTENSION exposes (MAX_TEXTURE_MAX_ANISOTROPY_EXT, MAX_DUAL_SOURCE_DRAW_BUFFERS,
      // MAX_CLIP_DISTANCES, ...) answer only once it is enabled, so the sweep above never saw them:
      // enable every supported extension, then ask each numeric constant of each extension object.
      // On a SEPARATE context, so `params` stays a fresh context's answers.
      const extParams = guard(() => {
        const c2 = document.createElement("canvas"); c2.width = 64; c2.height = 64;
        const g2 = c2.getContext(kind, { preserveDrawingBuffer: true });
        const out = {};
        for (const name of g2.getSupportedExtensions() || []) {
          const ext = g2.getExtension(name);
          if (!ext) continue;
          for (const k of Object.getOwnPropertyNames(Object.getPrototypeOf(ext))) {
            const v = ext[k];
            if (typeof v !== "number") continue;
            const got = guard(() => { const p = g2.getParameter(v); return ArrayBuffer.isView(p) ? Array.from(p) : (p && typeof p === "object" ? String(p) : p); }, "<throw>");
            if (g2.getError() === 0 && got !== null && got !== undefined && got !== "<throw>") out[String(v)] = got;
          }
        }
        return out;
      }, {});
      // WebGL 2's per-format multisample counts, which come from the driver.
      const internalformatSamples = kind !== "webgl2" ? null : guard(() => {
        const out = {};
        const ext = gl.getExtension("EXT_color_buffer_float"); gl.getExtension("EXT_texture_norm16");
        for (const f of [0x8229, 0x822B, 0x8051, 0x8D62, 0x8058, 0x8C43, 0x8057, 0x8056, 0x8059, 0x81A5, 0x81A6, 0x8CAC,
                         0x88F0, 0x8CAD, 0x8D48, 0x822A, 0x822C, 0x805B, 0x822D, 0x822F, 0x881A, 0x822E, 0x8230, 0x8814, 0x8C3A]) {
          const r = gl.getInternalformatParameter(gl.RENDERBUFFER, f, gl.SAMPLES);
          if (r) out[String(f)] = Array.from(r);
        }
        gl.getError();
        return ext === undefined ? null : out;
      }, null);
      return {
        params,
        extParams,
        internalformatSamples,
        unmaskedVendor: dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : null,
        unmaskedRenderer: dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : null,
        extensions: gl.getSupportedExtensions(),
        contextAttributes: gl.getContextAttributes(),
        precision,
      };
    });
    return { webgl: dump("webgl"), webgl2: dump("webgl2") };
  });

  // ── the behavior corpus, answered by the same runner Korvus is graded with ──────────────────────
  //
  // Two passes, because an expression that disagrees with itself is not an answer. This does NOT
  // replace the cross-SESSION check the capture tool does -- Brave's farbling seed is stable within a
  // session, so `navigator.plugins[0].name` is perfectly steady here and different in the next
  // browser. With shields down for this page there is no seed to be stable about, which is the other
  // reason the conditions block above records the shield state.

  // -- THE THREE ARTIFACTS THIS FILE DID NOT RECORD -------------------------------------------------
  //
  // Everything above is one visit to one browser. These three were not, and each had its own errand:
  // a CDP run for the codec table, a headless Chrome for the font advances, a Blink source checkout
  // for the user-agent stylesheet. That is exactly the "two machines, one persona" this file was
  // written to end, applied to itself -- a promoted bundle inherited all three from whatever session
  // had produced them last, and its manifest had to admit so in three `<role>-inherited` entries.
  //
  // They are cheap: codecs and per-tag defaults are synchronous reads, and the font table is three
  // fonts x 95 advances plus the pairs that actually kern. A second or two on a ninety-second
  // capture, and three entries close.

  // `canPlayType` / `isTypeSupported` over the matrix `codecs.mjs` drove via CDP -- the containers,
  // the codec spellings that decide the grammar, the multi-codec lists a page really sends, and the
  // quoting variants. Four askers, because a browser answers them differently and a table built from
  // one of them is wrong for the other three.
  const codecs = () => guard(() => {
    const CONTAINERS = ["video/mp4","audio/mp4","video/webm","audio/webm","video/ogg","audio/ogg",
      "application/ogg","audio/mpeg","audio/mp3","audio/x-mp3","audio/wav","audio/x-wav","audio/wave",
      "audio/x-pn-wav","audio/aac","audio/x-aac","audio/flac","audio/x-flac","video/mp2t","video/3gpp",
      "audio/3gpp","video/quicktime","video/x-matroska","audio/x-matroska","video/x-msvideo","video/avi",
      "application/x-mpegurl","application/vnd.apple.mpegurl","audio/mpegurl","video/mpeg","audio/midi",
      "application/octet-stream","text/plain","application/x-korvus-nonsense","","video/","/mp4"];
    const CODECS = ["avc1.42E01E","avc1.42001E","avc1.4D401E","avc1.4D401F","avc1.64001E","avc1.640028",
      "avc1.58A01E","avc1.42E0","avc1.42E01","avc1.ZZZZZZ","avc1","avc3.42E01E","avc3",
      "hev1.1.6.L93.B0","hvc1.1.6.L93.B0","hev1","hvc1","vp8","vp8.0","vp9","vp9.0","vp09.00.10.08",
      "vp09.02.10.10.01.09.16.09.01","vp09.00.10","vp09","av01.0.04M.08","av01.0.05M.08","av01","av1",
      "mp4a.40.2","mp4a.40.5","mp4a.40.29","mp4a.40.34","mp4a.40","mp4a.66","mp4a.67","mp4a.69",
      "mp4a.6B","mp4a.a5","mp4a.a6","mp4a","mp3","mp4a.40.99","opus","vorbis","flac","alac","ac-3",
      "ec-3","theora","ulaw","pcm","1","2","Opus","VP8","AVC1.42E01E","x","nonsense",""];
    const COMBOS = [["video/mp4","avc1.42E01E, mp4a.40.2"],["video/mp4","avc1.42E01E,mp4a.40.2"],
      ["video/mp4","avc1.42E01E, nonsense"],["video/webm","vp8, opus"],["video/webm","vp9,vorbis"],
      ["video/webm","vp8, mp4a.40.2"],["audio/webm","opus, vorbis"],
      ["video/mp4","avc1.42E01E, avc1.4D401E"]];
    const QUOTINGS = [['video/webm; codecs="vp8"',"quoted"],["video/webm; codecs=vp8","unquoted"],
      ['video/webm;codecs="vp8"',"no-space"],['video/webm ; codecs="vp8"',"space-before-semi"],
      ['VIDEO/WEBM; CODECS="VP8"',"upper"],['video/webm; codecs="vp8 "',"trailing-space-in-codec"],
      ['video/webm; codecs=" vp8"',"leading-space-in-codec"],
      ['video/webm; profiles="x"; codecs="vp8"',"extra-param"],
      ['video/webm; codecs="vp8"; codecs="vorbis"',"duplicate-param"],
      ['video/webm; codecs=""',"empty-codecs"],["video/webm; codecs","bare-codecs"]];
    const v = document.createElement("video"), a = document.createElement("audio");
    const out = {};
    const SEP = "|";
    const ask = (id, mime) => {
      out["v" + SEP + id] = guard(() => v.canPlayType(mime), null);
      out["a" + SEP + id] = guard(() => a.canPlayType(mime), null);
      out["s" + SEP + id] = guard(() => typeof MediaSource !== "undefined" && MediaSource.isTypeSupported(mime), null);
      out["r" + SEP + id] = guard(() => typeof MediaRecorder !== "undefined" && MediaRecorder.isTypeSupported(mime), null);
    };
    for (const c of CONTAINERS) ask("base" + SEP + c, c);
    for (const c of CONTAINERS) for (const k of CODECS) ask("pair" + SEP + c + SEP + k, c + '; codecs="' + k + '"');
    for (const pair of COMBOS) ask("combo" + SEP + pair[0] + SEP + pair[1], pair[0] + '; codecs="' + pair[1] + '"');
    for (const q of QUOTINGS) ask("quote" + SEP + q[1] + SEP + q[0], q[0]);
    return { answers: out, containers: CONTAINERS.length, codecs: CODECS.length, probes: Object.keys(out).length };
  });

  // Per-character advances at 1000px, and only the pairs that actually kern -- a full 95x95 table is
  // mostly zeros. 1000px because advances scale linearly, so one capture serves every size and keeps
  // four decimals of a 16px advance exact.
  //
  // Three fonts, deliberately. Arial / Times New Roman / Courier New are metric-compatible with
  // Liberation on Linux, so these numbers are the numbers a real Arial produces on any platform.
  // Georgia and Verdana resolve to whatever this machine happens to have, and capturing those would
  // embed a lie about the persona rather than a fact about the font.
  const fontMetrics = () => guard(() => {
    const CHARS = [];
    for (let c = 0x20; c < 0x7f; c++) CHARS.push(String.fromCharCode(c));
    const FONTS = ["Arial", "Times New Roman", "Courier New"];
    const SIZE = 1000;
    const cx = document.createElement("canvas").getContext("2d");
    const out = {};
    for (const f of FONTS) {
      cx.font = SIZE + 'px "' + f + '"';
      const adv = {};
      for (const ch of CHARS) adv[ch] = cx.measureText(ch).width;
      const kern = {};
      for (const a of CHARS) for (const b of CHARS) {
        const d = cx.measureText(a + b).width - adv[a] - adv[b];
        if (Math.abs(d) > 0.001) kern[a + b] = d;
      }
      const m = cx.measureText("Hxg");
      out[f] = { size: SIZE, advances: adv, kern,
                 ascent: m.fontBoundingBoxAscent, descent: m.fontBoundingBoxDescent,
                 xheight: cx.measureText("x").actualBoundingBoxAscent, zero: adv["0"] };
    }
    return out;
  });

  // The platform facts form controls and scroll containers are sized by, read back off LAYOUT
  // because no script API names them.
  //
  // A classic scrollbar's thickness is a property of the platform's theme -- Linux Chromium draws a
  // 15px one, an overlay-scrollbar build draws none -- and every scroll container's client box is
  // that much smaller. `offsetWidth - clientWidth` of an `overflow: scroll` box IS the thickness,
  // for `scrollbar-width: auto` and `thin` alike. A drop-down's theme paddings are an empty
  // `<select>` at `font-size: 0`: its label is 0 wide and its font 0 tall, so what is left is the
  // theme's inline and block padding and nothing else.
  //
  // A text field's width is `size` AVERAGE characters plus the font's MAXIMUM character width, as
  // Blink reads them (layout_box.cc TextFieldIntrinsicInlineSize): the OS/2 `xAvgCharWidth` and the
  // `head` bounding box, scaled -- or, for a font Blink judges to have no usable average, the
  // advance of `0` and no maximum at all. Neither number is visible to a script, so both are
  // inverted from the formula:
  //
  //   textarea cols=N, overflow hidden, no padding/border:  ceil(avg' * N)
  //   input size=1, no padding/border:                     ceil(max(avg', maxW))  (maxW integer)
  //   where avg' = max(avg, round(avg))  -- Blink rounds the average only UPWARD
  //
  // The average is read at a size whose fraction is below one half, so `avg'` is `avg` itself and
  // `ceil(avg * N) / N` pins it to 1/N px; such a size is found by asking two widths (a rounded
  // average doubles exactly, an unrounded one does not). The maximum is read at the largest size
  // Blink allows (10000px), where the rounding costs 1e-4 of an em. A font on the `0` path shows
  // no maximum -- `size=1` is then exactly `ceil(avg')` -- and is recorded as `null`: that is what
  // Blink reads for it, not a gap in the capture. Both are written in px at `size`, like advances.
  const layoutMetrics = () => guard(() => {
    const made = [];
    const el = (tag, css, attrs) => {
      const e = document.createElement(tag);
      e.style.cssText = "position:absolute;left:-99999px;top:0;margin:0;padding:0;border:0;" +
        "letter-spacing:normal;word-spacing:normal;text-transform:none;" + css;
      for (const k in attrs || {}) e.setAttribute(k, attrs[k]);
      document.body.appendChild(e);
      made.push(e);
      return e;
    };
    try {
      const bar = (css) => {
        const d = el("div", "width:100px;height:100px;overflow:scroll;" + css);
        return { vertical: d.offsetWidth - d.clientWidth, horizontal: d.offsetHeight - d.clientHeight };
      };
      const scrollbar = { auto: bar(""), thin: bar("scrollbar-width:thin"), none: bar("scrollbar-width:none") };
      const sel = el("select", "font-size:0");
      const menuList = { inlinePadding: sel.offsetWidth, blockPadding: sel.offsetHeight };

      const SIZE = 1000, MAX_SIZE = 10000, COLS = 2000;
      const width = (e) => e.getBoundingClientRect().width;
      const area = (f, px, cols) => width(el("textarea", 'overflow:hidden;font:' + px + 'px "' + f + '"', { cols: String(cols) }));
      const field = (f, px) => width(el("input", 'font:' + px + 'px "' + f + '"', { size: "1" }));
      const fonts = {};
      for (const f of ["Arial", "Times New Roman", "Courier New"]) {
        let avg = null, at = null;
        for (let px = 4000; px < 4100 && avg === null; px++) {
          const one = area(f, px, 1), two = area(f, px, 2);
          if (two !== 2 * one) { avg = area(f, px, COLS) / COLS / px * SIZE; at = px; }
        }
        const areaMax = area(f, MAX_SIZE, 1), fieldMax = field(f, MAX_SIZE);
        const valid = fieldMax > areaMax;
        fonts[f] = {
          size: SIZE,
          avgCharWidth: valid ? Math.round(avg * 1e4) / 1e4 : null,
          maxCharWidth: valid ? Math.round(fieldMax / MAX_SIZE * SIZE * 1e4) / 1e4 : null,
          readings: { avgAtPx: at, cols: COLS, maxAtPx: MAX_SIZE, textareaCols1: areaMax, inputSize1: fieldMax },
        };
      }
      return { scrollbar, menuList, fonts };
    } finally {
      for (const e of made) e.remove();
    }
  });

  // The user-agent stylesheet, OBSERVED rather than extracted. `core/korvus-page/ua/html.css` came
  // from a Blink source checkout at an unrecorded revision -- the one artifact in the bundle that
  // never came from a browser at all. A UA default is just the computed style of an untouched
  // element, so asking one of every element for the properties a UA sheet actually sets records the
  // same thing from the browser this persona IS, with a provenance attached.
  const uaDefaults = () => guard(() => {
    const TAGS = ("html body div span p h1 h2 h3 h4 h5 h6 ul ol li dl dt dd table thead tbody tfoot " +
      "tr td th caption col colgroup form fieldset legend label input button select option optgroup " +
      "textarea a img figure figcaption blockquote pre code kbd samp var b i u s strong em small sub " +
      "sup mark hr br article section nav aside header footer main address details summary dialog " +
      "menu template slot canvas video audio source track iframe object embed param picture abbr " +
      "bdi bdo cite data dfn del ins q ruby rt rp time wbr meter progress output datalist noscript " +
      "script style link meta title head").split(/\s+/);
    const PROPS = ["display","margin-top","margin-right","margin-bottom","margin-left","padding-top",
      "padding-right","padding-bottom","padding-left","font-size","font-weight","font-style",
      "font-family","line-height","text-align","text-decoration-line","white-space","list-style-type",
      "border-top-width","border-collapse","vertical-align","color","background-color","cursor",
      "overflow-x","overflow-y","position","unicode-bidi","visibility"];
    // MEASURED IN AN EMPTY DOCUMENT, NOT IN THIS ONE. The first version of this probe appended to
    // the capture page's own body and dutifully recorded the CAPTURE PAGE's styles as the browser's
    // defaults: `body { font: 15px/1.55 ...; margin: 3rem auto }` came back as margin-top 48px and
    // font-size 15px, and `h1` as 21.6px -- 1.44em of the page's 15px rather than 2em of the real
    // 16px default. A UA default is only visible where no author sheet has spoken, so this uses a
    // same-origin iframe with an empty srcdoc: same browser, same settings, no styles at all.
    const frame = document.createElement("iframe");
    frame.setAttribute("srcdoc", "<!doctype html><html><head></head><body></body></html>");
    frame.style.cssText = "position:absolute;left:-99999px;top:0;width:800px;height:600px;border:0";
    document.body.appendChild(frame);
    const out = {};
    try {
      const doc = frame.contentDocument;
      if (!doc || !doc.body) return { error: "no same-origin frame document" };
      const view = frame.contentWindow;
      for (const tag of TAGS) {
        let el;
        try { el = doc.createElement(tag); } catch (e) { continue; }
        // `html` and `body` already exist in the frame and cannot be nested; read the real ones.
        const target = tag === "html" ? doc.documentElement : tag === "body" ? doc.body : el;
        if (target === el) doc.body.appendChild(el);
        const cs = view.getComputedStyle(target);
        const rec = {};
        for (const prop of PROPS) rec[prop] = cs.getPropertyValue(prop);
        out[tag] = rec;
        if (target === el) doc.body.removeChild(el);
      }
    } finally { frame.remove(); }
    return { props: PROPS, tags: out, measuredIn: "empty same-origin iframe" };
  });

  const behavior = async (runnerSrc, cases) => guardAsync(async () => {
    if (!cases || !cases.length) return { skipped: "no corpus served" };
    // The corpus runs in a same-origin frame, never in this page. Four of its expressions assign to
    // `location` -- pathname, hostname, protocol, hash -- and a browser answers those by NAVIGATING.
    // The answers themselves are fine: each value is read back in the same task, before the
    // navigation commits, which is exactly why the reference records the OLD "/" and "127.0.0.1".
    // What is not fine is that the queued navigation then commits at the end of that task and takes
    // the capture page with it, destroying the collector before it can POST the result. Two early
    // runs of this file survived by winning that race; every later one lost it. Inside a frame the
    // navigation can only destroy the frame, and the frame is removed in the same task, before it
    // gets the chance.
    //
    // The frame is loaded from "/" so protocol, hostname, pathname and hash read exactly as they do
    // in the top page, and with referrerpolicy=no-referrer so document.referrer stays "" the way it
    // is in a directly-opened tab. The answers are therefore the top page's answers.
    // A REAL SIZE, OFFSCREEN. The frame was 1x1 px, and the first Linux capture recorded
    // `ShadowRoot.elementsFromPoint` as 0 -- there was nothing under any point of a one-pixel
    // document. Hit-testing answers are about layout, and a viewport is part of that.
    const mkFrame = async () => {
      const frame = document.createElement("iframe");
      frame.setAttribute("referrerpolicy", "no-referrer");
      frame.style.cssText = "position:absolute;left:-9999px;top:0;width:1024px;height:768px;border:0";
      frame.src = "/";
      await new Promise((res, rej) => {
        frame.onload = res;
        frame.onerror = () => rej(new Error("corpus frame failed to load"));
        document.body.appendChild(frame);
      });
      return frame;
    };
    // THE NAVIGATING CASES RUN LAST, IN A FRAME OF THEIR OWN. Four expressions assign to `location`,
    // and on the chunked path below -- the one every captureVersion >= 2 corpus takes, because it
    // has awaited cases -- the task boundary lets that navigation COMMIT. The first Linux capture
    // shows what a realm looks like after it: `location.hash` answered "#x", and the navigation
    // entry every later Performance* case read was a 404 text/plain for "/x". Every case after
    // the assignment was answering about a page nobody meant to ask about. So the mutators are
    // partitioned out, and the realm the rest run in is never navigated.
    // Matched on ANY receiver, not on the spelling `location.` -- the corpus writes these as
    // `const o = location; o.pathname = "x"`, and a pattern that wanted the word `location` before
    // the dot let all four through into the main frame, which then spent the rest of the run at
    // `/x#x`. That is the capture this comment was learned from.
    const ASSIGNS = /\.\s*(hash|pathname|hostname|protocol|href|search|host|port)\s*=[^=]|\.\s*(assign|replace|reload)\s*\(/;
    const NAVIGATES = { test: (e) => /\blocation\b/.test(e) && ASSIGNS.test(e) };
    // THE CASES A FRAME CANNOT ANSWER AS A TAB run in this page, the top-level one. `frameElement` is
    // an element in a frame and null in a tab; `getInstalledRelatedApps` throws InvalidStateError
    // anywhere but a top-level browsing context; the navigation entry (`getEntriesByType(
    // "navigation")[0]`, `performance.navigation`) describes the frame's own load, where a tab's
    // has a real `confidence` and the status of the page the user opened. The first Linux capture
    // promoted every one of those as the device's answer and `emit.py` had to hold them back by
    // name. Selected by the shape of the expression, so the list cannot go stale: nothing here
    // navigates or writes, which is the only reason it is safe to run in the collector's own realm.
    const TOP_REALM = /\bframeElement\b|\bgetInstalledRelatedApps\b|getEntriesByType\(["']navigation["']\)|\bperformance\.navigation\b/;
    const all = cases.map((c, i) =>
      c.await === true ? { id: String(i), expr: c.expr, await: true } : { id: String(i), expr: c.expr });
    const movers = all.filter((p) => NAVIGATES.test(p.expr));
    const tops = all.filter((p) => !NAVIGATES.test(p.expr) && TOP_REALM.test(p.expr));
    const pairs = all.filter((p) => !NAVIGATES.test(p.expr) && !TOP_REALM.test(p.expr));
    const frame = await mkFrame();
    let w = frame.contentWindow;
    // w.eval(src) is an indirect eval belonging to the frame's realm, so the corpus sees the frame's
    // globals -- its location, its document, its navigator -- and not this page's.
    const run = () => { w.__KORVUS_CASES = pairs; return w.eval(runnerSrc); };
    let a, b;
    // THE SINGLE-TASK PATH IS KEPT EXACTLY AS IT WAS, and the branch is why.
    //
    // The runner answers a corpus of purely synchronous cases with a STRING, synchronously, so both
    // passes and the frame removal still happen in ONE task -- which is the whole reason the four
    // `location.x = ...` cases can be asked at all: the navigation they queue commits at the end of
    // the task, and by then the frame is gone. Awaiting here unconditionally would put a task boundary
    // between pass A and pass B and hand the navigation its chance.
    //
    // A corpus that contains an awaited case cannot have that guarantee -- there is no way to wait
    // for a promise inside one task -- so it takes the second branch and gives it up knowingly. Do
    // not "simplify" these two branches into one await.
    if (!pairs.some((p) => p.await)) {
      try { a = JSON.parse(run()); b = JSON.parse(run()); } finally { frame.remove(); }
    } else {
      // CHUNKED, AND ONLY ON THIS BRANCH. Two passes over fourteen thousand expressions is the
      // longest thing this capture does by an order of magnitude, and it used to report nothing
      // between "starting" and "done" -- so a single expression whose promise never settles looked
      // exactly like a corpus that was still working, and the operator's only option was to wait and
      // guess. Measured: a run sat silent for over ten minutes with no way to tell which it was.
      //
      // Chunking is safe HERE and nowhere else. The synchronous branch above must stay one task,
      // because four cases assign to `location` and the navigation they queue commits at the end of
      // that task, by which point the frame is already gone. This branch has awaited cases, so it has
      // task boundaries anyway and gave that guarantee up knowingly (see the note above). Do not lift
      // this into the other branch.
      const CHUNK = 500;
      // A CHUNK THAT OVERRUNS BISECTS ITSELF UNTIL IT NAMES THE CASE.
      //
      // The runner already bounds every AWAITED case at 1500 ms, so an unsettled promise cannot cost
      // more than that. What it cannot bound is a case that blocks the task synchronously -- a native
      // dialog, an OS device enumeration -- because the deadline's own timer cannot fire either.
      //
      // And that only happens in the operator's browser. Measured here: this exact range runs in 36 ms
      // under headless Chrome, while a headed Brave spent the whole 60 s budget on it. A capture rig
      // whose failures are only reproducible on the far side of the screen has to diagnose itself, so
      // an overrunning chunk is re-run in halves, and each half in halves, until the log names a
      // single case. The cost is bounded (log2(500) ≈ 9 extra attempts) and is paid only on failure.
      // THE BUDGET IS 5 s OF SLACK PLUS WHAT THE RUNNER ITSELF MAY SPEND.
      //
      // A flat number cannot be right for both halves of this corpus. `behavior-run.js` bounds every
      // AWAITED case at 1500 ms and runs them sequentially, so a range's honest worst case is
      // proportional to how many awaited cases it holds -- and they are not spread evenly: of the
      // thirty 500-case chunks, half hold three or fewer, while one holds 86. A flat 5 s would send
      // that chunk bisecting on a run where nothing is wrong (86 x 1.5 s = 129 s is its LEGITIMATE
      // worst case), and each level of that bisection re-pays the awaited cost.
      //
      // So: 5 s for a range with no awaited case, which is the fast detection this wants, plus the
      // runner's own declared allowance for the ones that are there. A range that exceeds THAT is
      // blocked rather than slow, because the runner has promised it cannot take longer.
      const SLACK_MS = 5000, PER_AWAIT_MS = 1600;
      const runRange = async (label, from, to, depth) => {
        const slice = pairs.slice(from, to);
        w.__KORVUS_CASES = slice;
        const budget = SLACK_MS + slice.filter((c) => c.await).length * PER_AWAIT_MS;
        const out = await within(budget, Promise.resolve(w.eval(runnerSrc)), label + " @" + from);
        if (!out || !out.error) return JSON.parse(out);
        if (to - from <= 1) {
          const c = cases[from];
          log("      " + label + ": CASE " + from + " never returned -- " +
              (c ? c.iface + "." + c.member : "?") + "  " + (c ? String(c.expr).slice(0, 90) : ""));
          return {};
        }
        const mid = from + Math.floor((to - from) / 2);
        if (depth === 0) {
          log("      " + label + ": " + from + "-" + to + " overran " + budget + "ms; bisecting to name it");
        }
        const a = await runRange(label, from, mid, depth + 1);
        const b = await runRange(label, mid, to, depth + 1);
        return Object.assign(a, b);
      };
      const passOnce = async (label) => {
        const merged = {};
        for (let i = 0; i < pairs.length; i += CHUNK) {
          const end = Math.min(i + CHUNK, pairs.length);
          Object.assign(merged, await runRange(label, i, end, 0));
          log("      " + label + ": " + end + "/" + pairs.length);
        }
        return merged;
      };
      // A FRESH FRAME FOR THE SECOND PASS. The two passes are a determinism filter, and a filter
      // that runs pass 2 in the document pass 1 has already mutated (doctype replaced, history
      // pushed, sheets adopted) drops cases for the wrong reason. Both passes start from the same
      // bare document, so a case that differs between them differs on its own account.
      try {
        a = await passOnce("pass 1");
      } finally {
        frame.remove();
      }
      const frameB = await mkFrame();
      try {
        w = frameB.contentWindow;
        b = await passOnce("pass 2");
      } finally {
        frameB.remove();
      }
    }
    // The mutators: a fresh frame, both passes in ONE synchronous task (none of them awaits), and
    // the frame removed in that same task -- the single-task path above, applied to exactly the
    // cases that need it. Their answers are read back before the navigation they queue commits.
    if (movers.length) {
      const f2 = await mkFrame();
      const w2 = f2.contentWindow;
      const run2 = () => { w2.__KORVUS_CASES = movers; return w2.eval(runnerSrc); };
      try {
        Object.assign(a, JSON.parse(run2()));
        Object.assign(b, JSON.parse(run2()));
      } finally { f2.remove(); }
      log("      navigating cases: " + movers.length + " run last in their own frame");
    }
    // The top-realm cases, in THIS page. Indirect eval, so the runner's global is this window's;
    // it reads `__KORVUS_CASES` from there and answers about this tab's navigation. Awaited cases
    // (getInstalledRelatedApps) make it return a promise, bounded like the frame's chunks.
    if (tops.length) {
      const runTop = async (label) => {
        globalThis.__KORVUS_CASES = tops;
        const budget = 5000 + tops.filter((c) => c.await).length * 1600;
        const out = await within(budget, Promise.resolve((0, eval)(runnerSrc)), label + " (top realm)");
        return out && out.error ? {} : JSON.parse(out);
      };
      try {
        Object.assign(a, await runTop("pass 1"));
        Object.assign(b, await runTop("pass 2"));
      } finally { delete globalThis.__KORVUS_CASES; }
      log("      top-realm cases: " + tops.length + " answered by this page, not the frame");
    }
    const answers = {}, unstable = [];
    cases.forEach((c, i) => {
      const k = String(i);
      if (JSON.stringify(a[k]) !== JSON.stringify(b[k])) { unstable.push(c.iface + "." + c.member); return; }
      answers[c.iface + "\u0000" + c.member + "\u0000" + c.expr] = a[k];
    });
    return { total: cases.length, recorded: Object.keys(answers).length, unstableWithinSession: unstable, answers };
  });

  // ── run ─────────────────────────────────────────────────────────────────────────────────────────
  return (async () => {
    log("fetching the shared scripts");
    const [dumper, runner, renderSrc, corpusRaw] = await Promise.all([
      fetchText("/shared/surface-dump.js"),
      fetchText("/shared/behavior-run.js"),
      fetchText("/shared/capture-render-fingerprint.console.js"),
      fetchText("/shared/behavior-cases.json").catch(() => "[]"),
    ]);
    const corpus = guard(() => JSON.parse(corpusRaw), []);

    log("conditions");
    const cond = await conditions();

    log("render fingerprint (canvas / WebGL / audio)");
    const render = await within(60000, guardAsync(async () => {
      delete globalThis.__RF;
      (0, eval)(renderSrc);
      for (let i = 0; i < 600; i++) {
        if (globalThis.__RF) { const rf = globalThis.__RF; delete globalThis.__RF; return JSON.parse(rf); }
        await new Promise((r) => setTimeout(r, 100));
      }
      throw new Error("render capture did not finish");
    }), "render");

    log("surfaces (window / worker / shared worker / iframe)");
    const surf = await surfaces(dumper);
    // Anything of the harness's that outlived the two removals above is stripped here and named in
    // the result. A name that shows up in `harnessGlobalsRemoved` is a bug in this file, not a
    // property of the browser -- but it is a bug the capture reports rather than bakes in.
    const HARNESS = ["__capLog", "__RF", "__KORVUS_CASES", "__korvusResult"];
    const removed = [];
    for (const realm of Object.values(surf || {})) {
      if (!realm || !Array.isArray(realm.globals)) continue;
      realm.globals = realm.globals.filter((n) => {
        if (!HARNESS.includes(n)) return true;
        if (!removed.includes(n)) removed.push(n);
        return false;
      });
    }

    log("persona values");
    const per = await persona();

    log("environment (Intl / media queries / CSS.supports)");
    const env = environment();
    // `speechSynthesis.getVoices()` once the list has arrived -- one `voiceschanged`
    // ~8 ms after the first read in every Chromium measured; 3 s is the ceiling for a browser that never
    // fires it. Promoted into values.json `speech.voices`.
    env.speechVoices = await guard(() => new Promise((resolve) => {
      const read = () => speechSynthesis.getVoices().map(v => ({ name: v.name, lang: v.lang, localService: v.localService, default: v.default, voiceURI: v.voiceURI }));
      const t = setTimeout(() => resolve(read()), 3000);
      speechSynthesis.addEventListener("voiceschanged", () => { clearTimeout(t); setTimeout(() => resolve(read()), 100); }, { once: true });
      speechSynthesis.getVoices();
    }), null);
    // The device's own offer for receive-only audio and video -- which codecs, payload
    // types, header extensions and feedback a section lists is the build's (Chromium 151 adds RTCP XR rrtr;
    // Opera's Linux build has no H.264). Promoted into webrtc-offer.json (`emit.py --role webrtc-offer`).
    env.webrtcOffer = await within(5000, guardAsync(async () => {
      const pc = new RTCPeerConnection();
      try { return (await pc.createOffer({ offerToReceiveAudio: 1, offerToReceiveVideo: 1 })).sdp; } finally { pc.close(); }
    }), "webrtcOffer");
    // RTCRtpReceiver/RTCRtpSender.getCapabilities(kind) -- libwebrtc's decoder and encoder
    // factories, the device's like the offer (the receiver's video list adds decode-only codecs the offer never
    // lists). Promoted into webrtc-capabilities.json (`emit.py --role webrtc-capabilities`).
    env.webrtcCapabilities = guard(() => {
      const o = {};
      for (const side of ["RTCRtpReceiver", "RTCRtpSender"]) for (const kind of ["audio", "video"]) o[side + "." + kind] = self[side].getCapabilities(kind);
      return o;
    }, null);
    // A CSSStyleDeclaration's NAMED properties -- the JS name of every CSS property and alias
    // the build exposes, as its named-property enumerator lists them. Promoted into css-named-properties.json
    // (`emit.py --role css-named-properties`).
    env.cssNamedProperties = guard(() => Object.getOwnPropertyNames(document.createElement("div").style).filter(n => !/^[0-9]+$/.test(n)), null);

    log("CSS property table (475 properties x 3 readings)");
    const css = cssTable();

    log("fonts");
    const fnt = fonts();

    log("error messages and stack shape");
    const err = errors();

    log("object identity");
    const ident = identity();

    log("timing");
    const tim = timing();

    log("WebGL parameters and extensions");
    const gl = webglFull();

    log("media codecs");
    const cod = codecs();

    log("font advances and kerning");
    const fmet = fontMetrics();

    log("layout metrics (scrollbar, form-control theme, average/maximum character widths)");
    const lmet = layoutMetrics();

    log("user-agent stylesheet defaults");
    const uad = uaDefaults();

    log("behavior corpus (" + (corpus.length || 0) + " expressions, twice)");
    const beh = await behavior(runner, corpus);

    log("done in " + ((Date.now() - T0) / 1000).toFixed(1) + "s");
    return {
      captureVersion: 5,
      conditions: cond,
      surfaces: surf,
      persona: per,
      environment: env,
      css,
      fonts: fnt,
      fontMetrics: fmet,
      layoutMetrics: lmet,
      codecs: cod,
      uaDefaults: uad,
      errors: err,
      identity: ident,
      timing: tim,
      webgl: gl,
      render,
      behavior: beh,
      harnessGlobalsRemoved: removed,
      elapsedMs: Date.now() - T0,
    };
  })();
})()
