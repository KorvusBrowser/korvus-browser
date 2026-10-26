(async () => {
  const r = [];
  await new Promise((res) => setTimeout(function () { r.push(['sloppy-timer', this === window, this === parent, this.location.href === location.href]); res(); }, 10));
  await new Promise((res) => window.setTimeout(function () { 'use strict'; r.push(['strict-timer', this === window, this.location.href === location.href]); res(); }, 10));
  await new Promise((res) => setInterval(function () { clearInterval(this.__i); r.push(['interval', this === window]); res(); }, 10));
  await new Promise((res) => queueMicrotask(function () { 'use strict'; r.push(['strict-microtask', this === undefined]); res(); }));
  return r;
})()
