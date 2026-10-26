(async function () {
  // Where a microtask checkpoint falls between two callbacks the engine calls (ABPROG batch 4). HTML "clean up after
  // running a callback": with no script on the stack a listener's microtasks run before the next listener; with one
  // (`dispatchEvent`, `click()` from script) they wait for the script. Each case puts two listeners on one event; each
  // listener logs itself and queues a microtask (`m1`, `p1` a promise job, `m2`). A row is [case, the log in order].
  const out = [];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  function two(target, type, log) {
    target.addEventListener(type, () => {
      log.push('L1');
      queueMicrotask(() => log.push('m1'));
      Promise.resolve().then(() => log.push('p1'));
    });
    target.addEventListener(type, () => {
      log.push('L2');
      queueMicrotask(() => log.push('m2'));
    });
  }
  async function run(name, ms, setup) {
    const log = [];
    try {
      await setup(log);
    } catch (e) {
      log.push('threw ' + e.name);
    }
    await wait(ms);
    out.push([name, log.join(',')]);
  }
  const blob = (text, type) => URL.createObjectURL(new Blob([text], { type }));

  await run('postMessage', 50, (log) => {
    two(window, 'message', log);
    window.postMessage('x', '*');
  });
  // the listeners above stay on window: later window messages would log into a dead array, so none is sent
  await run('MessageChannel', 50, (log) => {
    const c = new MessageChannel();
    two(c.port1, 'message', log);
    c.port1.start();
    c.port2.postMessage(1);
  });
  await run('dispatchEvent from script', 50, (log) => {
    const t = new EventTarget();
    two(t, 'x', log);
    t.dispatchEvent(new Event('x'));
    log.push('after');
  });
  await run('click() from script', 50, (log) => {
    const b = document.createElement('button');
    document.body.append(b);
    two(b, 'click', log);
    b.click();
    log.push('after');
    b.remove();
  });
  await run('dispatchEvent from a timer', 50, (log) => new Promise((r) => setTimeout(() => {
    const t = new EventTarget();
    two(t, 'x', log);
    t.dispatchEvent(new Event('x'));
    log.push('after');
    r();
  }, 0)));
  await run('two timers', 50, (log) => {
    setTimeout(() => { log.push('t1'); queueMicrotask(() => log.push('m1')); });
    setTimeout(() => { log.push('t2'); queueMicrotask(() => log.push('m2')); });
  });
  await run('window.event in the checkpoint', 50, (log) => {
    const c = new MessageChannel();
    c.port1.onmessage = () => { log.push('L1'); queueMicrotask(() => log.push('event:' + (window.event ? window.event.type : String(window.event)))); };
    c.port1.addEventListener('message', () => log.push('L2'));
    c.port2.postMessage(1);
  });
  await run('a listener throws', 50, (log) => {
    const c = new MessageChannel();
    const onerr = (e) => { log.push('error'); queueMicrotask(() => log.push('me')); e.preventDefault(); };
    window.addEventListener('error', onerr);
    c.port1.addEventListener('message', () => { log.push('L1'); queueMicrotask(() => log.push('m1')); throw new Error('x'); });
    c.port1.addEventListener('message', () => { log.push('L2'); queueMicrotask(() => log.push('m2')); });
    c.port1.start();
    c.port2.postMessage(1);
    setTimeout(() => window.removeEventListener('error', onerr), 30);
  });
  await run('img load', 300, (log) => {
    const i = new Image();
    two(i, 'load', log);
    i.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
  });
  await run('img error', 300, (log) => {
    const i = new Image();
    two(i, 'error', log);
    i.src = blob('not an image', 'image/png');
  });
  await run('xhr load', 300, (log) => {
    const x = new XMLHttpRequest();
    x.open('GET', blob('hello', 'text/plain'));
    two(x, 'load', log);
    x.send();
  });
  // until the message arrives (a worker's start is slow on a loaded box), then the checkpoint's 50 ms
  await run('worker message', 50, (log) => new Promise((r) => {
    const w = new Worker(blob('postMessage(1)', 'text/javascript'));
    two(w, 'message', log);
    w.addEventListener('message', () => r());
    setTimeout(r, 3000);
  }));
  await run('stylesheet load', 300, (log) => {
    const l = document.createElement('link');
    l.rel = 'stylesheet';
    l.href = blob('p{color:red}', 'text/css');
    two(l, 'load', log);
    document.head.append(l);
  });
  await run('script load', 300, (log) => {
    const s = document.createElement('script');
    s.src = blob('window.__ran = 1', 'text/javascript');
    two(s, 'load', log);
    document.head.append(s);
  });
  await run('iframe element load', 400, (log) => {
    const f = document.createElement('iframe');
    f.srcdoc = '<p>x</p>';
    two(f, 'load', log);
    document.body.append(f);
  });
  await run("a frame window's load and DOMContentLoaded", 400, async (log) => {
    const f = document.createElement('iframe');
    f.srcdoc = '<script>window.__log = [];' +
      'for (const t of ["DOMContentLoaded", "load"]) {' +
      ' addEventListener(t, () => { __log.push(t + "1"); queueMicrotask(() => __log.push("m1")); });' +
      ' addEventListener(t, () => { __log.push(t + "2"); queueMicrotask(() => __log.push("m2")); }); }<\/script><p>x</p>';
    document.body.append(f);
    await wait(300);
    for (const e of f.contentWindow.__log || ['no log']) log.push(e);
  });
  await run('hashchange', 300, (log) => {
    two(window, 'hashchange', log);
    location.hash = '#mt' + Math.random().toString(36).slice(2, 6);
  });
  await run('requestAnimationFrame', 300, (log) => {
    requestAnimationFrame(() => { log.push('r1'); queueMicrotask(() => log.push('m1')); });
    requestAnimationFrame(() => { log.push('r2'); queueMicrotask(() => log.push('m2')); });
  });
  return out;
})()
