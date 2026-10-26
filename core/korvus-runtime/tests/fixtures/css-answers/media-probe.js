(function () {
  /* Every media feature Blink 150 knows (media_feature_names.json5), each value its grammar takes and a few it does
     not, plus the query syntax around them: matchMedia(q).matches and the list's serialization .media. Only answers
     that do not depend on the window's exact size are asked. `(fallback: none)` is not: it crashes Chrome 150's renderer
     (`MediaValues` has no anchored fallback outside a container query). */
  var Q = [
    // any-hover / hover / any-pointer / pointer
    '(any-hover)', '(any-hover: none)', '(any-hover: hover)', '(any-hover: fine)',
    '(hover)', '(hover: none)', '(hover: hover)',
    '(any-pointer)', '(any-pointer: none)', '(any-pointer: coarse)', '(any-pointer: fine)',
    '(pointer)', '(pointer: none)', '(pointer: coarse)', '(pointer: fine)', '(pointer: hover)',
    // color / color-index / monochrome
    '(color)', '(color: 8)', '(min-color: 1)', '(max-color: 8)', '(max-color: 7)', '(color: 0)', '(color > 4)', '(color: 8.5)', '(min-color: 10)', '(color: 8px)',
    '(color-index)', '(color-index: 0)', '(min-color-index: 0)', '(max-color-index: 0)', '(min-color-index: 1)',
    '(monochrome)', '(monochrome: 0)', '(min-monochrome: 0)', '(max-monochrome: 0)', '(monochrome: 1)', '(monochrome >= 0)',
    // color-gamut / dynamic-range / video-dynamic-range
    '(color-gamut)', '(color-gamut: srgb)', '(color-gamut: p3)', '(color-gamut: rec2020)', '(color-gamut: foo)',
    '(dynamic-range)', '(dynamic-range: standard)', '(dynamic-range: high)',
    '(video-dynamic-range)', '(video-dynamic-range: standard)', '(video-dynamic-range: high)',
    // forced-colors / inverted-colors / prefers-*
    '(forced-colors)', '(forced-colors: none)', '(forced-colors: active)',
    '(inverted-colors)', '(inverted-colors: none)', '(inverted-colors: inverted)',
    '(prefers-color-scheme)', '(prefers-color-scheme: light)', '(prefers-color-scheme: dark)', '(prefers-color-scheme: no-preference)',
    '(prefers-contrast)', '(prefers-contrast: no-preference)', '(prefers-contrast: more)', '(prefers-contrast: less)', '(prefers-contrast: custom)',
    '(prefers-reduced-motion)', '(prefers-reduced-motion: no-preference)', '(prefers-reduced-motion: reduce)',
    '(prefers-reduced-data)', '(prefers-reduced-data: no-preference)', '(prefers-reduced-data: reduce)',
    '(prefers-reduced-transparency)', '(prefers-reduced-transparency: no-preference)', '(prefers-reduced-transparency: reduce)',
    // grid / scan / update / overflow-* / scripting / display-*
    '(grid)', '(grid: 0)', '(grid: 1)', '(grid: 2)', '(min-grid: 0)', '(grid: 0.0)',
    '(scan)', '(scan: progressive)', '(scan: interlace)',
    '(update)', '(update: none)', '(update: slow)', '(update: fast)',
    '(overflow-inline)', '(overflow-inline: none)', '(overflow-inline: scroll)', '(overflow-inline: paged)',
    '(overflow-block)', '(overflow-block: none)', '(overflow-block: scroll)', '(overflow-block: paged)',
    '(scripting)', '(scripting: enabled)', '(scripting: initial-only)', '(scripting: none)',
    '(display-mode)', '(display-mode: browser)', '(display-mode: fullscreen)', '(display-mode: standalone)', '(display-mode: minimal-ui)',
    '(display-mode: window-controls-overlay)', '(display-mode: tabbed)', '(display-mode: picture-in-picture)', '(display-mode: unframed)', '(display-mode: foo)',
    '(display-state)', '(display-state: normal)', '(resizable)', '(resizable: true)',
    // device-posture / viewport segments / transform-3d / device pixel ratio / resolution
    '(device-posture)', '(device-posture: continuous)', '(device-posture: folded)',
    '(horizontal-viewport-segments)', '(horizontal-viewport-segments: 1)', '(horizontal-viewport-segments: 2)', '(horizontal-viewport-segments >= 1)', '(min-horizontal-viewport-segments: 1)',
    '(vertical-viewport-segments)', '(vertical-viewport-segments: 1)', '(vertical-viewport-segments: 0)', '(vertical-viewport-segments < 2)',
    '(-webkit-transform-3d)', '(-webkit-transform-3d: 1)', '(-webkit-transform-3d: 0)', '(-webkit-transform-3d: 2)', '(-webkit-transform-3d: 1.0)',
    '(-webkit-device-pixel-ratio)', '(-webkit-min-device-pixel-ratio: 0.5)', '(-webkit-max-device-pixel-ratio: 100)', '(-webkit-device-pixel-ratio: 1dppx)', '(-webkit-min-device-pixel-ratio: 1.5)',
    '(resolution)', '(min-resolution: 0.5dppx)', '(max-resolution: 100x)', '(min-resolution: 48dpi)', '(min-resolution: 18.9dpcm)', '(resolution: 1)', '(resolution > 0.1dppx)', '(min-resolution: -1dppx)', '(MIN-RESOLUTION: 2X)', '(min-resolution: 100x)',
    // width / height / device-* / aspect ratios / orientation
    '(width)', '(height)', '(min-width: 1px)', '(max-width: 100000px)', '(width > 0)', '(0 < width < 100000px)', '(width: 100)', '(min-width: 0)', '(width >= 10em)',
    '(min-width: calc(10px + 1em))', '(width >= calc(100px * 2))', '(10px < width)', '(100000px > width > 1px)', '(1px < width > 2px)', '(width = 5px)', '(min-width: 1e3px)', '(min-width: .5em)',
    '(min-height: 1px)', '(max-height: 100000px)', '(height > 0px)',
    '(device-width)', '(device-height)', '(min-device-width: 1px)', '(max-device-width: 100000px)', '(device-height > 1px)', '(min-device-height: 0)',
    '(orientation)', '(orientation: foo)', '(min-orientation: landscape)',
    '(aspect-ratio)', '(min-aspect-ratio: 1/100)', '(max-aspect-ratio: 100/1)', '(min-aspect-ratio: 0.01)', '(max-aspect-ratio: 100)', '(aspect-ratio: 0/0)', '(min-aspect-ratio: 16 / 9 )', '(aspect-ratio > 1/100)', '(min-aspect-ratio: -1/2)', '(min-aspect-ratio: 1/-2)',
    '(device-aspect-ratio)', '(min-device-aspect-ratio: 1/100)', '(max-device-aspect-ratio: 100/1)',
    // container-only and unknown features
    '(inline-size)', '(min-inline-size: 1px)', '(block-size > 0px)', '(stuck: top)', '(snapped: x)', '(scrollable: top)', '(scrolled: top)', '(fallback)',
    '(navigation-controls)', '(navigation-controls: back-button)', '(nav-controls: back)', '(origin-trial-test)',
    '(environment-blending: opaque)', '(shape: rect)', '(foo)', '(foo: bar)', '(  foo  :  bar  )', '(--x)', '(--x: 1)',
    // the syntax around the features
    '', ' ', 'all', 'screen', 'SCREEN', 'print', 'tv', 'speech', 'foo', 'not print', 'not screen', 'not foo', 'only screen', 'only print', 'only',
    'not screen and (color)', 'only screen and (min-width: 1px)', 'Screen And (Color)', 'screen and (color), print', 'print, screen', 'print, speech',
    '(color) and (hover)', '(color) or (hover)', '(color) and (hover) and (pointer: fine)', '(color) or (foo)', '(foo) or (color)', '(foo) and (color)',
    'screen and (color) or (hover)', '(color) and (hover) or (pointer)', 'not (color)', 'not (foo)', 'not (color) and (hover)', '(not (color))', '((color))', '((foo))',
    '(color) and (not (hover))', 'not ((color) or (foo))', 'not all', 'not all and (color)', 'screen,', ',screen', 'screen and', 'and (color)', 'screen and(color)',
    'screen and (color) and', 'screen (color)', 'screen foo', 'screen, print foo, all', 'not only screen', 'screen or print', 'only (color)', 'not not (color)',
    '(min-width:1px)', '( min-width : 1PX )', '(MIN-WIDTH: 1PX)', '(width>=1px)', '(1px<=width)', '(width >= 1px) and (width <= 99999px)', '(1px < width <= 99999px)',
    '(99999px > width >= 1px)', '(hover: hover) and (pointer: fine)', 'foo(bar)', 'screen and foo(bar)', '(color) and foo(bar)', 'not foo(bar)', '(color:)', '(:8)',
    '(color: 8) (hover)', 'screen and (color) , (hover)', '(width < 1e9px)', '(color /* c */)', '/* c */ screen', '(width: calc(1px * sin(0.5)))', '(min-width: calc(1px * sin(0.5)))',
    '(min-width: calc(50% + 1px))', '(min-width: 1vw)', '(max-width: 100vmax)', '(min-height: 1vh)', '(min-width: 0.5rem)', '(min-width: 1Q)', '(min-width: 1in)', '(color: calc(4 + 4))'
  ];
  var out = Q.map(function (q) { try { var m = matchMedia(q); return [q, m.matches, m.media]; } catch (e) { return [q, 'THROW', String(e)]; } });
  // the same parser behind an @media rule
  var style = document.createElement('style');
  style.textContent = '@media screen and (MIN-WIDTH:1px) { a { color: red } }\n@media (color) or (foo:bar) { a { color: red } }\n'
    + '@media not all and (monochrome) { a { color: red } }\n@media (min-aspect-ratio: 16/9), print { a { color: red } }\n@media garbage ( { a { color: red } }\n'
    + '@media (inverted-colors: none) { a { color: red } }\n@media (-webkit-transform-3d) { a { color: red } }\n';
  document.head.appendChild(style);
  Array.prototype.forEach.call(style.sheet.cssRules, function (r, i) { out.push(['@media #' + i, r.conditionText, r.media.mediaText, r.media.length, r.cssText]); });
  style.remove();
  return out;
})()
