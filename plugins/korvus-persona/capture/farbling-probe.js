// The probe `farbling.py` evaluates once per (session, origin) cell — the farbled surface, read raw.
//
// WHY IT IS A FILE AND NOT A PYTHON STRING. The same reason `collect.js` is: a probe that lives
// inside its driver is a probe nobody reads, and this one carries the whole definition of what
// "Brave's farbling" is measured to BE. Everything here is a raw reading. It computes no verdict,
// no delta and no policy — the deltas need the unfarbled base, which is a different browser launch,
// and deciding a policy from one cell is exactly the mistake R19 exists to prevent.
//
// It is evaluated with `Runtime.evaluate(..., awaitPromise: true, returnByValue: true)` and returns a
// plain object. Every read is guarded: a throw in one signal must not cost the other ten, because a
// cell that came back empty is indistinguishable from a session that never ran.
(async () => {
  const guard = (fn, fallback = null) => { try { return fn(); } catch (e) { return fallback; } };

  // collect.js's 32-bit hash, same constants, so a digest here is comparable with one from there.
  const h32 = (s) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = (h * 0x01000193) >>> 0; }
    return ("0000000" + h.toString(16)).slice(-8);
  };

  // ── canvas ────────────────────────────────────────────────────────────────────────────────────
  //
  // Eight flat fills, because the operation's SIGN turned out to be the parity of the unfarbled
  // channel and a single even-channelled fill (#123456) reads as "+1 only" — the correction §14.2
  // of the sampling findings paid 480 cells for. Three buffer LENGTHS from one uniform strip,
  // because the restriction to four flip patterns is a property of the buffer's length and not of
  // the colour. Two 1x1 reads at different coordinates of the same strip, because the perturbation
  // is keyed on the bytes returned rather than on where they were read.
  const FILLS = ["#123456", "#000000", "#ffffff", "#7f7f7f",
                 "#010203", "#fefdfc", "#ff0000", "#00ff01"];
  const flat = guard(() => {
    const out = {};
    for (const fill of FILLS) {
      const cv = document.createElement("canvas");
      cv.width = 8; cv.height = 1;
      const x = cv.getContext("2d", { willReadFrequently: false });
      x.fillStyle = fill; x.fillRect(0, 0, 8, 1);
      out[fill] = {
        one:      Array.from(x.getImageData(0, 0, 1, 1).data),
        oneElsewhere: Array.from(x.getImageData(5, 0, 1, 1).data),
        two:      Array.from(x.getImageData(1, 0, 2, 1).data),
        four:     Array.from(x.getImageData(3, 0, 4, 1).data),
        eight:    Array.from(x.getImageData(0, 0, 8, 1).data),
      };
    }
    return out;
  }, {});

  // A real drawing, so `toDataURL` has something to recompress: text and a stroked arc over a fill.
  const drawing = guard(() => {
    const cv = document.createElement("canvas");
    cv.width = 220; cv.height = 60;
    const x = cv.getContext("2d");
    x.fillStyle = "#123456"; x.fillRect(0, 0, 220, 60);
    x.font = "16px serif"; x.fillStyle = "#f0f"; x.fillText("korvus farbling probe", 6, 24);
    x.strokeStyle = "#0ff"; x.beginPath(); x.arc(180, 40, 14, 0, 4); x.stroke();
    const url = cv.toDataURL();
    const px = Array.from(x.getImageData(2, 50, 1, 1).data);
    return { urlHash: h32(url), urlLen: url.length, flatPixel: px };
  });

  // ── audio ─────────────────────────────────────────────────────────────────────────────────────
  //
  // FP0's fixture, kept so the numbers stay comparable: the fudge is MULTIPLICATIVE and one factor
  // per cell, so both a sum and two individual samples are returned — if they are all the same
  // ratio against the unfarbled render, the factor is one per buffer rather than per sample.
  const audio = await guard(() => new Promise((res) => {
    const ctx = new OfflineAudioContext(1, 44100, 44100);
    const osc = ctx.createOscillator();
    osc.type = "triangle"; osc.frequency.value = 10000;
    const comp = ctx.createDynamicsCompressor();
    osc.connect(comp); comp.connect(ctx.destination);
    osc.start(0);
    ctx.startRendering().then((buf) => {
      const d = buf.getChannelData(0);
      let sum = 0;
      for (let i = 4500; i < 5000; i++) sum += Math.abs(d[i]);
      res({ sum, at4500: d[4500], at4600: d[4600] });
    }).catch(() => res(null));
  }), Promise.resolve(null));

  return {
    href: location.href,
    origin: location.origin,
    webdriver: guard(() => navigator.webdriver),
    isBrave: guard(() => !!navigator.brave),
    nav: {
      hardwareConcurrency: guard(() => navigator.hardwareConcurrency),
      deviceMemory: guard(() => navigator.deviceMemory),
      languages: guard(() => Array.from(navigator.languages)),
      language: guard(() => navigator.language),
      mimeTypesLength: guard(() => navigator.mimeTypes.length),
      mimeTypes: guard(() => Array.from(navigator.mimeTypes).map((m) => m.type)),
      plugins: guard(() => Array.from(navigator.plugins).map((p) => ({
        name: p.name, filename: p.filename, description: p.description, length: p.length,
        mimes: Array.from(p).map((m) => ({ type: m.type, suffixes: m.suffixes })),
      }))),
    },
    screen: guard(() => ({
      width: screen.width, height: screen.height,
      availWidth: screen.availWidth, availHeight: screen.availHeight,
      availTop: screen.availTop, availLeft: screen.availLeft,
      colorDepth: screen.colorDepth, pixelDepth: screen.pixelDepth,
    })),
    win: guard(() => ({
      innerWidth: innerWidth, innerHeight: innerHeight,
      outerWidth: outerWidth, outerHeight: outerHeight,
      screenX: screenX, screenY: screenY, devicePixelRatio: devicePixelRatio,
    })),
    canvas: { flat, drawing },
    audio,
  };
})()
