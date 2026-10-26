// REALWEB T7/H34. Each document records how it was reached, then takes the next step of the chain: a reload,
// a script assignment, a declarative refresh to another URL and to its own, a same-URL assignment,
// history.go(0), a cross-site assignment. The requests are synchronous, so a step that leaves the document
// at once still has its record on the server. The record is its key: the step, the navigation timing
// type, performance.navigation.type, navigation.activation.navigationType and document.referrer. The
// FIRST document's activation depends on how the tab was opened (a harness's about:blank), so it is '-'.
(() => {
  const get = u => { const x = new XMLHttpRequest(); x.open('GET', u, false); x.send(); return x.responseText; };
  const k = new URLSearchParams(location.search).get('k') || 'page';
  const e = performance.getEntriesByType('navigation')[0];
  const first = k === 'nav' && e && e.type === 'navigate';
  const act = first ? '-' : (self.navigation && navigation.activation ? navigation.activation.navigationType : 'none');
  const rec = ['t:' + k, e ? e.type : 'none', performance.navigation.type, act, document.referrer || '""'].join(' ');
  get('/r/data?k=' + encodeURIComponent(rec));
  if (document.documentElement.dataset.chain === 'off') return;
  const s = JSON.parse(get('/report'));
  if (!s['nav#2']) location.reload();
  else if (!s.assign) location.href = '/nav.html?k=assign';
  else if (!s.meta) location.href = '/meta.html?k=metapage';
  else if (!s['self#2']) location.href = '/self.html?k=self';
  else if (!s.same) location.href = '/nav.html?k=same';
  else if (!s['same#2']) location.href = location.href;
  else if (!s.go0) location.href = '/nav.html?k=go0';
  else if (!s['go0#2']) history.go(0);
  else if (!s.xsite) location.href = 'http://localhost:' + location.port + '/nav.html?k=xsite';
  else if (!s.end) location.href = '/nav.html?k=end';
  else { window.__hdr = s; document.title = 'done'; }
})();
