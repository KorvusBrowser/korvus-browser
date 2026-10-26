(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { const v = await f(); R.push([name, 'resolved ' + Object.prototype.toString.call(v)]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  await t('Summarizer.create()', () => Summarizer.create());
  await t('LanguageModel.create()', () => LanguageModel.create());
  await t('LanguageDetector.create()', () => LanguageDetector.create());
  await t('Translator.create(pair)', () => Translator.create({ sourceLanguage: 'en', targetLanguage: 'fr' }));
  await t('Translator.create()', () => Translator.create());
  await t('Summarizer.availability()', () => Summarizer.availability().then((v) => ({ toString: () => v })));
  R.push(['activation', navigator.userActivation.isActive]);
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
