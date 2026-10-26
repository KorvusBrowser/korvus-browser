(() => {
  const out = [];
  const cols = [
    '#ff8000', '#F80', '#ff800080', '#f808', '#abc', '#ABCDEF', '#12', '#1234567', 'red', 'RED', 'Red', 'transparent', 'violetred', 'lightslateblue', 'rebeccapurple',
    'currentcolor', 'currentColor', 'canvas', 'Canvas', 'highlight', 'accentcolor', 'menu', 'buttonface', '-webkit-link', 'text',
    ' red ', '\tred\n', 'red/**/', '/**/red', ' #ff0000 ',
    'rgb(10, 200, 30)', 'rgb(10.5, 200.4, 30.6)', 'rgb(10.5,200.5,30.5)', 'rgb(1e1, 2e2, 3e1)', 'RGB(10.5, 200.4, 30.6)', 'rgb(10 200 30)', 'rgb(10 200 30 / 0.5)',
    'rgba(0, 0, 0, 0.503)', 'rgba(0, 0, 0, 0.5)', 'rgba(0,0,0,0.25)', 'rgba(0,0,0,0.123)', 'rgba(0,0,0,0.1234567)', 'rgba(0,0,0,50%)', 'rgb(0 0 0 / 50%)', 'rgba(0, 0, 0, 0.5 )',
    'rgba(0,0,0,.5)', 'rgba(0,0,0,1)', 'rgba(0,0,0,2)', 'rgba(0,0,0,-1)', 'rgba(0,0,0,0)', 'rgb(300, -20, 0)', 'rgb(50%, 20%, 100%)', 'rgb(33.3%, 66.7%, 99.9%)', 'rgb(50% 20 10)',
    'rgb(10, 20 30)', 'rgb(none 20 30)', 'rgb(10, 20, 30, 0.5)', 'rgb(255 255 255 / none)', 'rgb(calc(10 + 5) 20 30)', 'rgb(from red r g b)',
    'hsl(120, 50%, 50%)', 'hsl(120 50% 50%)', 'hsl(120deg 50% 50% / 0.25)', 'hsl(2rad, 50%, 50%)', 'hsl(0.5turn 100% 25%)', 'hsl(-60, 100%, 50%)', 'hsl(400, 100%, 50%)', 'hsla(210, 60%, 40%, 0.7)', 'hsl(210 60 40)', 'hsl(210, 60, 40)', 'HSL(120, 50%, 50%)', 'hsl(120.5, 33.3%, 66.6%)', 'hsl(none 50% 50%)',
    'hwb(120 20% 30%)', 'hwb(120 60% 60%)', 'hwb(30deg 10% 20% / 0.4)', 'hwb(120, 20%, 30%)',
    'lab(50 40 30)', 'lab(50% 40% -30%)', 'lab(150 0 0)', 'lab(50 40 30 / 0.3)', 'lab(none 40 30)', 'lch(50 40 30)', 'lch(70 30 400)', 'lch(50 -10 30)', 'oklab(0.7 0.1 -0.1)', 'oklab(70% 25% 0)', 'oklch(0.7 0.2 150)', 'oklch(0.5 0.4 30deg / 50%)',
    'color(srgb 0.2 0.4 0.6)', 'color(srgb 20% 40% 60%)', 'color(srgb 1.2 -0.1 0.5)', 'color(srgb-linear 0.2 0.4 0.6)', 'color(display-p3 1 0.25 0.5)', 'color(display-p3 0 1 0)', 'color(display-p3-linear 0.5 0.5 0.5)',
    'color(a98-rgb 0.2 0.4 0.6)', 'color(prophoto-rgb 0.2 0.4 0.6)', 'color(rec2020 0.2 0.4 0.6)', 'color(rec2100-linear 0.2 0.4 0.6)', 'color(xyz 0.2 0.4 0.6)', 'color(xyz-d50 0.2 0.4 0.6)', 'color(xyz-d65 0.2 0.4 0.6)',
    'color(display-p3 1 0.25 0.5 / 0.5)', 'color(display-p3 none 0.25 0.5)', 'color(display-p3 0.123456789 0.25 0.5)', 'color(display-p3 1, 0.25, 0.5)', 'color(srgb 1 0.000001 0.5)', 'color(srgb 1 1e-7 0.5)',
    'color-mix(in srgb, red, blue)', 'light-dark(red, blue)', 'notacolor', '', 'rgb(', '#zzzzzz',
  ];
  const px = (cs, ct, col, rs, fmt) => {
    const c = new OffscreenCanvas(1, 1); const x = c.getContext('2d', { colorSpace: cs, colorType: ct });
    x.fillStyle = '#123456'; x.fillStyle = col; x.fillRect(0, 0, 1, 1);
    const s = {}; if (rs) s.colorSpace = rs; if (fmt) s.pixelFormat = fmt;
    return [x.fillStyle, Array.from(x.getImageData(0, 0, 1, 1, s).data)];
  };
  for (const col of cols) {
    try {
      const a = px('srgb', 'unorm8', col);
      const b = px('display-p3', 'unorm8', col);
      const f = px('srgb', 'float16', col, 'srgb', 'rgba-float16');
      const g = px('display-p3', 'float16', col, 'display-p3', 'rgba-float16');
      out.push([JSON.stringify(col), a[0], a[1], b[1], f[1], g[1]]);
    } catch (e) { out.push([JSON.stringify(col), 'THROW ' + e.name + ': ' + e.message]); }
  }
  // shadows and gradients in a wide-gamut colour
  for (const cs of ['srgb', 'display-p3']) {
    const c = new OffscreenCanvas(4, 1); const x = c.getContext('2d', { colorSpace: cs });
    const gr = x.createLinearGradient(0, 0, 4, 0); gr.addColorStop(0, 'color(display-p3 1 0 0)'); gr.addColorStop(1, 'lab(50 40 30)');
    x.fillStyle = gr; x.fillRect(0, 0, 4, 1);
    out.push(['gradient ' + cs, Array.from(x.getImageData(0, 0, 4, 1).data)]);
    const c2 = new OffscreenCanvas(8, 8); const y = c2.getContext('2d', { colorSpace: cs });
    y.shadowColor = 'color(display-p3 0 1 0 / 0.5)'; y.shadowOffsetX = 4; y.fillStyle = 'oklch(0.7 0.2 150)'; y.fillRect(0, 0, 4, 8);
    out.push(['shadow ' + cs, y.shadowColor, Array.from(y.getImageData(0, 4, 8, 1).data)]);
  }
  return out;
})()
