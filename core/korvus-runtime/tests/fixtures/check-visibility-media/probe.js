(async function () {
  // `Element.checkVisibility()` reads `display` under the DOCUMENT's media, as `getComputedStyle` does (LEAN B2).
  // The breakpoints are made from this window's own `innerWidth`, so the answers do not depend on the window's size:
  // a rule at `min-width: innerWidth - 1` applies here, one at `max-width: innerWidth - 1` does not. korvus read
  // `display` under a 1024x768 screen with no theme, so on a wider window the first rule did not apply and the second
  // did. A row is [case, answer].
  const w = innerWidth;
  const style = document.createElement('style');
  style.textContent =
    `@media (min-width: ${w - 1}px) { #a { display: none } #p { display: none } }\n` +
    `@media (max-width: ${w - 1}px) { #b { display: none } }\n` +
    `@media (width: ${w}px) { #c { display: none } }`;
  document.head.append(style);
  const el = (id, parent) => { const e = document.createElement('div'); e.id = id; e.textContent = id; (parent || document.body).append(e); return e; };
  const a = el('a'), b = el('b'), c = el('c'), p = el('p'), q = el('q', p), d = el('d');
  return [
    ['min-width innerWidth-1 hides #a', a.checkVisibility()],
    ['max-width innerWidth-1 leaves #b', b.checkVisibility()],
    ['width innerWidth hides #c', c.checkVisibility()],
    ['an ancestor hidden by @media hides #q', q.checkVisibility()],
    ['no rule leaves #d', d.checkVisibility()],
    ['getComputedStyle agrees on #a', getComputedStyle(a).display],
    ['getComputedStyle agrees on #b', getComputedStyle(b).display],
    ['matchMedia width innerWidth', matchMedia(`(width: ${w}px)`).matches],
    ['checkVisibility again after a layout read', (b.getBoundingClientRect(), a.checkVisibility())],
  ];
})()
