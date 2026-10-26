// Behavioral detector EMULATOR: replicates a real behavior-tier detector's feature extraction (a device-check script's
// es_* from the reverse-engineered payload + Castle's mouse rules) as PAGE JS running inside Korvus. It
// listens to the events the engine dispatches and scores them through the SAME JS event-listener path a
// real detector uses -- so it validates the actual dispatched events (event.isTrusted, movementX/Y,
// timeStamp) end to end, not the generator's raw Rust samples. `__detScore()` returns a per-feature
// verdict; a naive teleport / JS-dispatched bot must fail it (the required negative control).
(function () {
  const MM = []; // mousemove: { sx, sy, t, mx, my, tr }
  const PM = []; // pointermove count
  const rec = (e) => {
    if (e.type === 'mousemove') {
      MM.push({ sx: e.screenX, sy: e.screenY, t: e.timeStamp, mx: e.movementX, my: e.movementY, tr: e.isTrusted });
    } else if (e.type === 'pointermove') {
      PM.push(1);
    }
  };
  document.addEventListener('mousemove', rec);
  document.addEventListener('pointermove', rec);

  const mean = (a) => (a.length ? a.reduce((s, x) => s + x, 0) / a.length : 0);
  const std = (a) => {
    if (a.length < 2) return 0;
    const m = mean(a);
    return Math.sqrt(a.reduce((s, x) => s + (x - m) * (x - m), 0) / (a.length - 1));
  };

  globalThis.__detScore = function () {
    const n = MM.length;
    // 1. every mousemove genuinely trusted (isTrusted) -- the #1 injection tell.
    const allTrusted = n > 0 && MM.every((m) => m.tr === true);
    // 2. Castle: hundreds of events per interaction (a teleport bot emits ~4).
    const enoughEvents = n >= 100;
    // 3. movementX/Y == the consecutive screenX/Y delta (physics consistency).
    let badMove = 0;
    for (let i = 1; i < n; i++) {
      if (MM[i].mx !== MM[i].sx - MM[i - 1].sx || MM[i].my !== MM[i].sy - MM[i - 1].sy) badMove++;
    }
    const moveConsistent = badMove === 0;
    // 4. Castle: acceleration almost never ~0 (a constant-velocity line has accel~0 everywhere).
    const vel = [];
    for (let i = 1; i < n; i++) {
      const d = Math.hypot(MM[i].sx - MM[i - 1].sx, MM[i].sy - MM[i - 1].sy);
      const dt = Math.max(1, MM[i].t - MM[i - 1].t);
      vel.push(d / dt);
    }
    const accelVaries = std(vel) > 1e-6;
    // 5. a device-check script es_sigmdn: stddev of log(timeStamp) -- the timing-entropy feature (uniform timing -> ~0).
    const logs = MM.filter((m) => m.t > 0).map((m) => Math.log(m.t));
    const esSigmdn = std(logs);
    // 6. pointer-primary: pointermove present alongside mousemove.
    const hasPointer = PM.length > 0;

    const human = allTrusted && enoughEvents && moveConsistent && accelVaries && esSigmdn > 0.01 && hasPointer;
    return { n, pm: PM.length, allTrusted, enoughEvents, moveConsistent, accelVaries, esSigmdn, hasPointer, human };
  };
  globalThis.__detReset = function () { MM.length = 0; PM.length = 0; };
})();
