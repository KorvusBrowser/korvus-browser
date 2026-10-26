(async () => {
  // A frame's History, used from the page: what each call answers, and which URL and state it moved.
  const out = [];
  const host = (s) => String(s).replace(/127\.0\.0\.1:\d+/g, 'HOST');
  const f = document.createElement('iframe');
  document.body.append(f);
  const w = f.contentWindow;
  const t = (label, fn) => {
    let v;
    try { v = 'ok ' + host(fn()); } catch (e) { v = e.name + ': ' + host(e.message); }
    out.push([label, v, host(w.location.href), host(location.href), JSON.stringify(w.history.state), JSON.stringify(history.state)]);
  };
  t('frame pushState /path', () => w.history.pushState(null, '', '/test-push-path'));
  t('frame pushState #frag', () => w.history.pushState(null, '', '#frag'));
  t('frame pushState about:blank?x', () => w.history.pushState(null, '', 'about:blank?x'));
  t('frame replaceState state', () => w.history.replaceState({a: 1}, ''));
  t('frame state is a clone', () => { const o = {b: 2}; w.history.replaceState(o, ''); return w.history.state === o; });
  t('frame pushState uncloneable', () => w.history.pushState(() => 1, ''));
  t('frame pushState empty url', () => w.history.pushState({c: 3}, '', ''));
  t('page state untouched', () => history.state);
  return out;
})()
