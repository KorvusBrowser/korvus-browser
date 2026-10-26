(async () => {
  const out = [];
  const t = (label, fn) => { try { out.push([label, String(fn())]); } catch (e) { out.push([label, 'threw ' + e.name, e.message]); } };
  t('rot90', () => new DOMMatrix().rotateSelf(90));
  t('rotaa90', () => new DOMMatrix().rotateAxisAngleSelf(0, 0, 1, 90));
  t('rotaa90x', () => new DOMMatrix().rotateAxisAngleSelf(1, 0, 0, 90));
  t('rot180', () => new DOMMatrix().rotateSelf(180));
  t('rot270', () => new DOMMatrix().rotateSelf(-90));
  t('rot45', () => new DOMMatrix().rotateSelf(45));
  t('rot30', () => new DOMMatrix().rotateSelf(30));
  t('rot3', () => new DOMMatrix().rotateSelf(90, 90, 0));
  t('rotvec', () => new DOMMatrix().rotateFromVectorSelf(0, 1));
  t('tiny', () => new DOMMatrix([1e-17, 0, 0, 1, 0, 0]));
  t('big', () => new DOMMatrix([1e21, 0, 0, 1, 0, 0]));
  t('rot90-m11', () => new DOMMatrix().rotateSelf(90).m11);
  t('rotaa-xyz', () => new DOMMatrix().rotateAxisAngleSelf(1, 1, 1, 120));
  t('css-rot', () => new DOMMatrix('rotate(90deg)'));
  t('css-rot3d', () => new DOMMatrix('rotate3d(0, 0, 1, 90deg)'));
  t('skew', () => new DOMMatrix().skewXSelf(45));
  t('style-len', () => { const s = new CSSStyleSheet(); s.replaceSync('a{color:red;width:3px}'); return s.cssRules[0].style.length + '|' + s.cssRules[0].style[1]; });
  t('supports', () => [CSS.supports('width', 'red'), CSS.supports('x', 'x'), CSS.supports('x', '1px'), CSS.supports('height', 'auto'), CSS.supports('width', 'foo'), CSS.supports('min-width', 'red'), CSS.supports('margin-top', 'red'), CSS.supports('top', 'red'), CSS.supports('opacity', 'red'), CSS.supports('display', 'red')].join());
  return out;
})()
