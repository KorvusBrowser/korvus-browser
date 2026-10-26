(() => {
  // getComputedStyle's answer for every shorthand Chrome 150 serializes, over its initial values and a set of
  // declared ones: [shorthand, declaration, value]
  const R = [];
  const host = document.createElement('div');
  document.body.appendChild(host);
  const read = (decl, props) => {
    const el = document.createElement('div');
    el.setAttribute('style', decl);
    host.appendChild(el);
    const cs = getComputedStyle(el);
    for (const p of props) R.push([p, decl, cs.getPropertyValue(p).split(location.origin).join('ORIGIN')]);
    el.remove();
  };
  const all = ['animation', 'animation-range', 'background', 'background-position', 'border-block', 'border-block-color', 'border-block-end', 'border-block-start', 'border-block-style', 'border-block-width', 'border-image', 'border-inline', 'border-inline-color', 'border-inline-end', 'border-inline-start', 'border-inline-style', 'border-inline-width', 'border-radius', 'border-spacing', 'column-rule', 'columns', 'container', 'corner-shape', 'corner-top-shape', 'corner-left-shape', 'corner-block-start-shape', 'flex', 'flex-flow', 'font', 'font-synthesis', 'grid', 'grid-template', 'inset-block', 'inset-inline', 'interest-delay', 'list-style', 'margin-block', 'margin-inline', 'marker', 'mask', 'offset', 'outline', 'overscroll-behavior', 'padding-block', 'padding-inline', 'page-break-after', 'page-break-before', 'page-break-inside', 'place-content', 'place-items', 'place-self', 'position-try', 'scroll-margin-block', 'scroll-margin-inline', 'scroll-padding-block', 'scroll-padding-inline', 'scroll-timeline', 'text-box', 'text-emphasis', 'text-wrap', 'transition', 'view-timeline', '-webkit-text-stroke', '-webkit-column-break-after', '-webkit-column-break-before', '-webkit-column-break-inside', '-webkit-border-radius', '-webkit-columns', '-webkit-flex', '-webkit-flex-flow', '-webkit-mask', '-webkit-text-emphasis', '-webkit-transition', '-webkit-animation', '-webkit-column-rule', '-webkit-border-before', '-webkit-border-after', '-webkit-border-start', '-webkit-border-end'];
  read('', all);
  const cases = [
    ['animation', ['animation: 2s ease-in 1s infinite reverse both paused spin', 'animation: a 1s, b 2s linear', 'animation-name: x; animation-duration: 3s', 'animation: none']],
    ['animation-range', ['animation-range: entry 10% exit 90%', 'animation-range: contain']],
    ['background', ['background: red url(x.png) no-repeat center / 10px 20px fixed content-box padding-box', 'background: url(a.png), url(b.png) red', 'background-color: blue', 'background: linear-gradient(red, blue)']],
    ['background-position', ['background-position: 10px 20%', 'background-position: right 5px bottom', 'background-image: url(a.png), url(b.png); background-position: 1px 2px, 3px 4px']],
    ['border-block', ['border-block: 2px solid red', 'border-block-start: 1px dotted blue']],
    ['border-block-color', ['border-block-color: red blue', 'border-block-color: green']],
    ['border-block-style', ['border-block-style: solid dashed']],
    ['border-block-width', ['border-block-width: 3px 5px; border-block-style: solid']],
    ['border-inline', ['border-inline: 2px solid red', 'border-inline-end: 4px double green']],
    ['border-image', ['border-image: url(x.png) 10 20 / 5px / 2px round stretch', 'border-image-source: url(y.png)', 'border-image: url(z.png) 30% fill']],
    ['border-radius', ['border-radius: 10px', 'border-radius: 10px 20px 30px 40px / 5px', 'border-top-left-radius: 3px 6px']],
    ['border-spacing', ['border-spacing: 2px', 'border-spacing: 2px 4px']],
    ['column-rule', ['column-rule: 2px dashed red', 'column-rule-style: solid']],
    ['columns', ['columns: 3', 'columns: 100px', 'columns: 100px 3', 'column-count: auto; column-width: 50px']],
    ['container', ['container: card / inline-size', 'container: card', 'container-type: size']],
    ['corner-shape', ['corner-shape: bevel', 'corner-shape: bevel squircle', 'corner-top-left-shape: scoop']],
    ['flex', ['flex: 1', 'flex: 2 3 10px', 'flex: none', 'flex: auto', 'flex-grow: 4']],
    ['flex-flow', ['flex-flow: column wrap', 'flex-direction: row-reverse']],
    ['font', ['font: italic bold 20px/1.5 serif', 'font: small-caps 12px monospace', 'font: 10px sans-serif; font-stretch: condensed', 'font: caption', 'font-size: 30px', 'font-variant-ligatures: none']],
    ['font-synthesis', ['font-synthesis: none', 'font-synthesis: weight', 'font-synthesis: style small-caps']],
    ['grid', ['grid: auto-flow / 1fr 1fr', 'grid: "a b" 10px / 1fr 2fr', 'grid-template-columns: 10px 20px', 'grid: 100px / auto-flow dense 50px']],
    ['grid-template', ['grid-template: 100px / 1fr 1fr', 'grid-template: "a a" 20px "b c" 30px / 1fr 1fr', 'grid-template: none']],
    ['inset-block', ['inset-block: 1px 2px', 'inset-block: 5%']],
    ['inset-inline', ['inset-inline: auto 3px']],
    ['interest-delay', ['interest-delay: 1s', 'interest-delay: 1s 2s']],
    ['list-style', ['list-style: square inside', 'list-style: url(x.png)', 'list-style: none']],
    ['margin-block', ['margin-block: 1px 2px', 'margin-block: 3px']],
    ['margin-inline', ['margin-inline: auto']],
    ['marker', ['marker: url(#m)', 'marker-start: url(#a)']],
    ['mask', ['mask: url(x.png) no-repeat center / contain', 'mask-image: url(a.png), url(b.png)']],
    ['offset', ['offset: path("M 0 0 H 100") 50px auto 30deg', 'offset-distance: 10px', 'offset: 10px 20px']],
    ['outline', ['outline: 2px dotted blue', 'outline-style: solid']],
    ['overscroll-behavior', ['overscroll-behavior: contain', 'overscroll-behavior: contain none']],
    ['padding-block', ['padding-block: 1px 2px']],
    ['padding-inline', ['padding-inline: 4px']],
    ['page-break-after', ['break-after: page', 'break-after: left', 'break-after: column', 'page-break-after: always', 'break-after: avoid']],
    ['page-break-before', ['break-before: right', 'break-before: recto']],
    ['page-break-inside', ['break-inside: avoid', 'break-inside: avoid-column']],
    ['place-content', ['place-content: center', 'place-content: center start', 'align-content: space-between']],
    ['place-items', ['place-items: center', 'place-items: start end']],
    ['place-self', ['place-self: end', 'place-self: auto center']],
    ['position-try', ['position-try: most-height top', 'position-try: --a, flip-block', 'position-try-order: most-width']],
    ['scroll-margin-block', ['scroll-margin-block: 1px 2px']],
    ['scroll-padding-inline', ['scroll-padding-inline: 5px']],
    ['scroll-timeline', ['scroll-timeline: --t inline', 'scroll-timeline-name: --a, --b']],
    ['text-box', ['text-box: trim-both cap alphabetic', 'text-box: trim-start', 'text-box-edge: text', 'text-box: cap']],
    ['text-emphasis', ['text-emphasis: filled circle red', 'text-emphasis-style: "x"']],
    ['text-wrap', ['text-wrap: nowrap', 'text-wrap: balance', 'text-wrap: nowrap balance', 'text-wrap: pretty']],
    ['transition', ['transition: opacity 1s ease-in 2s', 'transition: all 1s, color 2s linear', 'transition-duration: 3s', 'transition: none', 'transition-behavior: allow-discrete']],
    ['view-timeline', ['view-timeline: --v block 10px', 'view-timeline-name: --w']],
    ['-webkit-text-stroke', ['-webkit-text-stroke: 2px red']],
    ['-webkit-column-break-after', ['break-after: column', 'break-after: page']],
    ['-webkit-column-break-inside', ['break-inside: avoid-column', 'break-inside: avoid']],
  ];
  for (const [p, decls] of cases) for (const d of decls) read(d, [p]);
  host.remove();
  return R;
})()
