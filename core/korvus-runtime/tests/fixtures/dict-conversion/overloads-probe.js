(() => {
  // What a constructor answers before any argument is converted: WebIDL's overload resolution on the argument count
  // ("N argument(s) required, but only M present.", "Overload resolution failed."), the overload one argument picks
  // (KeyframeEffect's copy), and a [LegacyWindowAlias] being the same interface object. Rows: [label, outcome, detail].
  const out = [];
  const val = (v) => (v !== null && typeof v === 'object' ? Object.prototype.toString.call(v) : v === undefined ? 'undefined' : v);
  const t = (label, f) => { try { out.push([label, 'ok', val(f())]); } catch (e) { out.push([label, e.name, e.message]); } };
  const cb = () => {};
  t('AudioBuffer/0', () => new AudioBuffer());
  t('AudioBuffer/undefined', () => new AudioBuffer(undefined));
  t('OfflineAudioContext/0', () => new OfflineAudioContext());
  t('OfflineAudioContext/1-undefined', () => new OfflineAudioContext(undefined));
  t('OfflineAudioContext/2', () => new OfflineAudioContext(1, 2));
  t('OfflineAudioContext/2-undefined', () => new OfflineAudioContext(undefined, undefined));
  t('OfflineAudioContext/3', () => new OfflineAudioContext(1, 128, 44100).length);
  t('IntersectionObserver/0', () => new IntersectionObserver());
  t('IntersectionObserver/1-number', () => new IntersectionObserver(1));
  t('KeyframeEffect/0', () => new KeyframeEffect());
  t('KeyframeEffect/1-null', () => new KeyframeEffect(null));
  t('KeyframeEffect/1-element', () => new KeyframeEffect(document.body));
  t('KeyframeEffect/2', () => new KeyframeEffect(null, null).getKeyframes().length);
  t('KeyframeEffect/1-copy', () => {
    const e = new KeyframeEffect(document.body, [{opacity: 0}, {opacity: 1}], {duration: 5, delay: 2});
    e.composite = 'add';
    const c = new KeyframeEffect(e);
    return [c !== e, c.target === document.body, c.getTiming().duration, c.getTiming().delay, c.getKeyframes().length, c.composite, c.pseudoElement].join();
  });
  t('PerformanceObserver/0', () => new PerformanceObserver());
  t('ResizeObserver/0', () => new ResizeObserver());
  t('VTTCue/0', () => new VTTCue());
  t('VTTCue/1', () => new VTTCue(1));
  t('VTTCue/2', () => new VTTCue(1, 2));
  t('VTTCue/3', () => new VTTCue(1, 2, 'x').text);
  t('MutationObserver/0', () => new MutationObserver());
  t('WebKitMutationObserver/0', () => new WebKitMutationObserver());
  t('WebKitMutationObserver/1', () => new WebKitMutationObserver(cb));
  t('alias/WebKitMutationObserver', () => [WebKitMutationObserver === MutationObserver, WebKitMutationObserver.name, new WebKitMutationObserver(cb) instanceof MutationObserver].join());
  t('alias/WebKitCSSMatrix', () => [WebKitCSSMatrix === DOMMatrix, WebKitCSSMatrix.name].join());
  t('alias/webkitURL', () => [webkitURL === URL, webkitURL.name].join());
  return out;
})()
