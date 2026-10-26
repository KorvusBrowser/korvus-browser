(() => {
  const out = [];
  const vals = ['red', 'transparent', 'violetred', '#ff800080', '#f808', 'rgba(0, 0, 0, 0.503)', 'rgba(0,0,0,0.123)', 'rgba(0,0,0,0.5 )', 'hsla(120, 50%, 50%, 0.503)', 'rgb(10.5, 20, 30)', 'RGB(10.5, 20, 30)',
    'rgb(10.4 20.6 30.5)', 'rgb(50% 20 10)', 'hsl(120.5, 33.3%, 66.6%)', 'hwb(120 20% 30%)', 'hwb(30deg 10% 20% / 0.4)', 'rgb(none 20 30)', 'rgb(255 255 255 / none)',
    'lab(50 40 30)', 'lab(50% 40% -30%)', 'lab(none 40 30)', 'lch(70 30 400)', 'oklab(70% 25% 0)', 'oklch(0.5 0.4 30deg / 50%)', 'color(srgb 0.2 0.4 0.6)', 'color(xyz 0.2 0.4 0.6)',
    'color(display-p3 1 0.25 0.5 / 0.5)', 'color(display-p3 0.123456789 0.25 0.5)', 'color(srgb 1 1e-7 0.5)', 'rgb(calc(10 + 5) 20 30)', 'lab(calc(50 + 1) 40 30)', 'rgb(from red r g b)', 'color-mix(in srgb, red, blue)',
    'color-mix(in oklab, red 30%, blue)', 'light-dark(red, blue)', 'currentcolor', 'canvas', 'highlight', 'buttonborder', 'violet', 'notacolor'];
  for (const v of vals) {
    const d = document.createElement('div'); document.body.appendChild(d);
    d.style.color = v; d.style.backgroundColor = v;
    out.push([v, CSS.supports('color', v), d.style.color, d.style.backgroundColor, getComputedStyle(d).color, getComputedStyle(d).backgroundColor]);
    d.remove();
  }
  return out;
})()
