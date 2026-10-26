(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = ['Summarizer.availability()', 'Summarizer.availability({})', 'LanguageModel.availability()', 'LanguageModel.availability({})', 'Translator.availability()', 'Translator.availability({})', 'LanguageDetector.availability()', 'LanguageDetector.availability({})', 'Translator.availability(langs)', 'Summarizer.create'];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { const order = []; const p = f(); if (!p || typeof p.then !== 'function') { R.push([name, 'not a promise ' + String(p)]); return; } let v; p.then((x) => { v = x; order.push('API'); }, (e) => { v = e.name + ': ' + e.message; order.push('API!'); }); Promise.resolve().then(() => order.push('micro')); setTimeout(() => order.push('t0'), 0); await new Promise((r) => setTimeout(r, 80)); R.push([name, order.join(' ') + ' = ' + v]); } catch (e) { R.push([name, 'sync ' + e.name + ': ' + e.message]); } };
  for (const I of ['Summarizer', 'LanguageModel', 'Translator', 'LanguageDetector', 'Writer', 'Rewriter', 'Proofreader']) {
    R.push([I + ' exists', typeof self[I]]);
    if (typeof self[I] !== 'function') continue;
    R.push([I + ' statics', Object.getOwnPropertyNames(self[I]).filter((k) => !['length', 'name', 'prototype'].includes(k)).join(',')]);
    if (typeof self[I].availability === 'function') {
      await t(I + '.availability()', () => self[I].availability());
      await t(I + '.availability({})', () => self[I].availability({}));
    }
  }
  await t('Translator.availability(langs)', () => Translator.availability({ sourceLanguage: 'en', targetLanguage: 'fr' }));
  await t('Summarizer.availability(bad type)', () => Summarizer.availability({ type: 'nope' }));
  await t('LanguageModel.params', () => LanguageModel.params ? LanguageModel.params() : 'none');
  await t('Summarizer.create', () => Summarizer.create());
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
