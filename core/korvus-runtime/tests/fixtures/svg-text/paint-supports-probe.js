(async () => {
  // CSS.supports for the SVG paint and stroke properties, as Blink's parsers take them.
  const R = [];
  const cases = {
    fill: ['none', 'red', 'nonsense', 'url(#a)', 'url(#a) none', 'url(#a) red', 'url(#a) nonsense', 'context-fill', 'context-stroke', 'currentcolor', 'red blue', '#abc', 'transparent'],
    stroke: ['none', 'nonsense', 'url(#x) blue', 'inherit'],
    'stroke-width': ['2', '2px', '-1px', '-1', '0', '10%', 'calc(1px + 2px)', 'auto', '2em'],
    'stroke-dashoffset': ['3', '3px', '-3px', '-3', '5%', 'x'],
    'stroke-dasharray': ['none', '5 10', '5px, 10px', '5px 10px', '-1', '5,', '5% 2em', 'a'],
    'stroke-miterlimit': ['7', '0.5', '-1', '0', '4px'],
    'letter-spacing': ['0', '1', '1px', 'normal', '-1px'],
    'word-spacing': ['0', '4', '4px', 'normal'],
    'baseline-shift': ['2', '2px', 'sub', 'super', 'baseline', '10%'],
    'font-size': ['20', '20px', '-1px'],
  };
  for (const [p, vs] of Object.entries(cases)) for (const v of vs) R.push([p + ': ' + v, CSS.supports(p, v)]);
  return R;
})()
