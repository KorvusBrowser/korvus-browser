(async () => {
  // The Navigation API's events around every same-document navigation a page can make, in order, with the
  // microtask and task boundaries marked. Rows: [label, log, location.hash, entries().length, history.length, index].
  const out = [];
  const log = [];
  const mark = (s) => log.push(s);
  const frag = (u) => { const i = u.indexOf('#'); const p = new URL(u).pathname + new URL(u).search; return (p === '/' ? '' : p) + (i < 0 ? '' : u.slice(i)); };
  const ent = (e) => (e ? frag(e.url) + '@' + e.index : String(e));
  let hook = null;
  navigation.addEventListener('navigate', (e) => {
    const d = e.destination;
    const cur = navigation.currentEntry;
    mark(['navigate', 'type=' + e.navigationType, 'hash=' + e.hashChange, 'canI=' + e.canIntercept, 'user=' + e.userInitiated,
      'cancelable=' + e.cancelable, 'trusted=' + e.isTrusted, 'info=' + String(e.info), 'dest=' + frag(d.url),
      'key=' + (d.key === '' ? 'empty' : d.key === cur.key ? 'cur' : navigation.entries().some((x) => x.key === d.key) ? 'listed' : 'other'),
      'id=' + (d.id === '' ? 'empty' : 'set'), 'index=' + d.index, 'same=' + d.sameDocument, 'state=' + JSON.stringify(d.getState()),
      'aborted=' + e.signal.aborted, 'form=' + e.formData, 'dl=' + e.downloadRequest, 'src=' + (e.sourceElement && e.sourceElement.tagName),
      'uav=' + e.hasUAVisualTransition, 'at=' + location.hash, 'cur=' + ent(cur), 'transition=' + navigation.transition,
      'bubbles=' + e.bubbles, 'composed=' + e.composed, 'phase=' + e.eventPhase, 'proto=' + Object.getPrototypeOf(e).constructor.name].join(' '));
    if (hook) hook(e);
  });
  navigation.addEventListener('currententrychange', (e) => mark(['cec', 'type=' + e.navigationType, 'from=' + ent(e.from), 'cur=' + ent(navigation.currentEntry),
    'at=' + location.hash, 'trusted=' + e.isTrusted, 'cancelable=' + e.cancelable, 'transition=' + (navigation.transition && navigation.transition.navigationType)].join(' ')));
  navigation.addEventListener('navigatesuccess', (e) => mark('success ' + Object.getPrototypeOf(e).constructor.name + ' cur=' + ent(navigation.currentEntry) + ' transition=' + navigation.transition + ' cancelable=' + e.cancelable));
  navigation.addEventListener('navigateerror', (e) => mark('error ' + Object.getPrototypeOf(e).constructor.name + ' msg=' + e.message + ' err=' + (e.error && e.error.name) + ':' + (e.error && e.error.message) + ' file=' + JSON.stringify(e.filename) + ' line=' + e.lineno + ' col=' + e.colno + ' cancelable=' + e.cancelable + ' transition=' + navigation.transition));
  addEventListener('popstate', (e) => mark('popstate state=' + JSON.stringify(e.state) + ' at=' + location.hash + ' sameAsHistory=' + (e.state === history.state) + ' hasUA=' + e.hasUAVisualTransition));
  addEventListener('hashchange', (e) => mark('hashchange ' + frag(e.oldURL) + '->' + frag(e.newURL)));
  const res = (r) => {
    mark('result keys=' + Object.keys(r).join() + ' proto=' + (Object.getPrototypeOf(r) === Object.prototype));
    r.committed.then((e) => mark('committed ' + ent(e) + ' isCur=' + (e === navigation.currentEntry)), (e) => mark('committed! ' + e.name + ': ' + e.message));
    r.finished.then((e) => mark('finished ' + ent(e) + ' isCur=' + (e === navigation.currentEntry)), (e) => mark('finished! ' + e.name + ': ' + e.message));
    return r;
  };
  const step = async (label, f, h) => {
    log.length = 0;
    hook = h || null;
    try { f(); mark('returned'); } catch (e) { mark('threw ' + e.name + ': ' + e.message); }
    queueMicrotask(() => mark('microtask'));
    await new Promise((r) => setTimeout(r, 0));
    mark('task');
    await new Promise((r) => setTimeout(r, 60));
    hook = null;
    out.push([label, log.slice(), location.hash, navigation.entries().length, history.length, navigation.currentEntry.index]);
  };
  await step('navigate #a', () => res(navigation.navigate('#a')));
  await step('navigate #b replace state info', () => res(navigation.navigate('#b', {history: 'replace', state: {s: 1}, info: 'I'})));
  await step('getState after', () => mark('state=' + JSON.stringify(navigation.currentEntry.getState())));
  await step('navigate same #b', () => res(navigation.navigate('#b')));
  await step('navigate #c push', () => res(navigation.navigate('#c', {history: 'push'})));
  await step('location.hash d', () => { location.hash = 'd'; });
  await step('location.hash same d', () => { location.hash = 'd'; });
  await step('location.href #e', () => { location.href = '#e'; });
  await step('location.replace #f', () => { location.replace('#f'); });
  await step('anchor click #g', () => { const a = document.createElement('a'); a.href = '#g'; document.body.append(a); a.click(); a.remove(); });
  await step('pushState #h', () => { history.pushState({p: 1}, '', '#h'); });
  await step('replaceState same', () => { history.replaceState({p: 2}, ''); });
  await step('pushState no url', () => { history.pushState({p: 3}, ''); });
  await step('pushState empty url', () => { history.pushState(null, '', ''); });
  await step('pushState uncloneable', () => { history.pushState(() => 1, ''); });
  await step('history.state identity', () => { const o = {q: 1}; history.replaceState(o, ''); mark('same=' + (history.state === o) + ' stable=' + (history.state === history.state) + ' eq=' + JSON.stringify(history.state)); });
  await step('history.back', () => { history.back(); });
  await step('history.forward', () => { history.forward(); });
  await step('navigation.back', () => res(navigation.back()));
  await step('navigation.forward', () => res(navigation.forward()));
  await step('traverseTo first', () => res(navigation.traverseTo(navigation.entries()[0].key)));
  await step('go(2)', () => { history.go(2); });
  await step('prevent navigate #i', () => res(navigation.navigate('#i')), (e) => e.preventDefault());
  await step('prevent location.hash j', () => { location.hash = 'j'; }, (e) => e.preventDefault());
  await step('prevent pushState #k', () => { history.pushState(null, '', '#k'); }, (e) => e.preventDefault());
  await step('intercept #l', () => res(navigation.navigate('#l')), (e) => e.intercept({handler: async () => { mark('handler at=' + location.hash + ' transition=' + (navigation.transition && [navigation.transition.navigationType, ent(navigation.transition.from), frag(navigation.transition.to.url)].join('/'))); await null; mark('handler-done'); }}));
  await step('intercept cross-doc /p2?x', () => res(navigation.navigate('/p2?x', {state: 7})), (e) => e.intercept({handler: () => { mark('handler path=' + location.pathname + location.search); }}));
  await step('intercept rejecting #m', () => res(navigation.navigate('#m')), (e) => e.intercept({handler: () => Promise.reject(new RangeError('no'))}));
  await step('intercept pushState #n', () => { history.pushState({z: 1}, '', '#n'); }, (e) => e.intercept({handler: () => { mark('handler'); }}));
  await step('intercept twice + scroll', () => res(navigation.navigate('#o')), (e) => { e.intercept(); e.intercept({handler: () => { try { e.scroll(); mark('scrolled'); } catch (x) { mark('scroll threw ' + x.name + ': ' + x.message); } }}); try { e.scroll(); } catch (x) { mark('early scroll threw ' + x.name + ': ' + x.message); } });
  await step('intercept late', () => { const r = res(navigation.navigate('#p')); }, (e) => { setTimeout(() => { try { e.intercept(); mark('late ok'); } catch (x) { mark('late threw ' + x.name + ': ' + x.message); } }, 0); });
  await step('intercept after prevent', () => res(navigation.navigate('#q')), (e) => { e.preventDefault(); try { e.intercept(); } catch (x) { mark('threw ' + x.name + ': ' + x.message); } });
  await step('navigate during ongoing', () => { res(navigation.navigate('#r')); res(navigation.navigate('#s')); }, (e) => { if (e.destination.url.endsWith('#r')) e.intercept({handler: () => new Promise((r) => setTimeout(r, 20))}); });
  await step('javascript url', () => res(navigation.navigate('javascript:1')));
  await step('updateCurrentEntry', () => { navigation.updateCurrentEntry({state: 9}); });
  await step('dispose', () => { const old = navigation.entries().slice(navigation.currentEntry.index + 1); for (const x of old) x.addEventListener('dispose', () => mark('dispose ' + ent(x))); navigation.navigate('#t'); });
  await step('back then push (dispose)', async () => { const fw = navigation.currentEntry; fw.addEventListener('dispose', (ev) => mark('dispose ' + frag(fw.url) + ' trusted=' + ev.isTrusted + ' idx=' + fw.index)); history.back(); });
  await step('push after back', () => { navigation.navigate('#u'); });
  await step('onnavigate attr', () => { navigation.onnavigate = (e) => mark('onnavigate ' + e.navigationType); location.hash = 'v'; navigation.onnavigate = null; });
  await step('NavigateEvent ctor', () => { try { new NavigateEvent('navigate', {}); } catch (x) { mark('threw ' + x.name + ': ' + x.message); } try { mark('dest ctor ' + typeof new NavigationDestination()); } catch (x) { mark('dest threw ' + x.name + ': ' + x.message); } });
  // last: a canceled traversal leaves Chrome's history.length one short until the next traversal commits
  await step('prevent history.back', () => { history.back(); }, (e) => e.preventDefault());
  return out;
})()
