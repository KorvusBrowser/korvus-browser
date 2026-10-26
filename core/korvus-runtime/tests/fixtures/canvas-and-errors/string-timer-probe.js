(async () => {
  const r = [];
  window.__st = null;
  const id = setTimeout('window.__st = new Error().stack', 0);
  r.push(['returns-an-id', id > 0]);
  await new Promise((res) => setTimeout(res, 30));
  r.push(['stack', String(window.__st).replace(/:\d{2,5}\//g, ':PORT/')]);
  const got = await new Promise((res) => {
    window.onerror = function (msg, url, line, col) { res([String(msg), String(url).replace(/:\d{2,5}\//g, ':PORT/'), line, col]); return true; };
    setTimeout('\n  throw new Error("boom")', 0);
  });
  window.onerror = null;
  r.push(['onerror', got]);
  return r;
})()
