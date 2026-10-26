(function () {
  /* An element's inline declaration block as Blink 150 serializes it (StylePropertySerializer): the style attribute
     set as written, then cssText, length and the longhands it holds, a shorthand and a longhand read back, and the
     attribute Blink writes after one CSSOM change. a bot-detection script's challenge frame was `border: 0`. Not asked here:
     `text-decoration`, which this engine's cascade keeps as one property where Blink has four longhands. */
  var out = [];
  var T = [
    ['display: none; width: 100%; height: 52px; border: 0;', ['border', 'border-top-width', 'border-top-style', 'border-style', 'border-width']],
    ['border: 0', ['border', 'border-top', 'border-color', 'border-image']],
    ['border: 1px solid red', ['border', 'border-left', 'border-top-color', 'border-image-source']],
    ['border: none', ['border', 'border-top-style', 'border-top-width']],
    ['border: 2px dashed', ['border', 'border-bottom-color']],
    ['border-top: 0', ['border-top', 'border', 'border-top-width']],
    ['border-width: 1px 2px', ['border-width', 'border-left-width', 'border']],
    ['margin: 0', ['margin', 'margin-top']],
    ['margin: 0 auto', ['margin', 'margin-left', 'margin-right']],
    ['padding: 1px 2px 3px 4px', ['padding', 'padding-left']],
    ['margin-top: 1px; margin-right: 1px; margin-bottom: 1px; margin-left: 1px', ['margin']],
    ['margin-top: 1px; margin-right: 2px', ['margin', 'margin-right']],
    ['outline: 0', ['outline', 'outline-width', 'outline-style']],
    ['outline: none', ['outline', 'outline-style']],
    ['background: red', ['background', 'background-color', 'background-image']],
    ['background: url(x.png) no-repeat', ['background', 'background-repeat']],
    ['font: 12px Arial', ['font', 'font-size', 'font-family', 'line-height', 'font-weight']],
    ['font: bold 12px/1.5 serif', ['font', 'line-height']],
    ['border-radius: 5px', ['border-radius', 'border-top-left-radius']],
    ['border-radius: 50%', ['border-radius']],
    ['inset: 0', ['inset', 'top']],
    ['flex: 1', ['flex', 'flex-grow', 'flex-basis']],
    ['overflow: hidden', ['overflow', 'overflow-x']],
    ['transition: opacity .3s', ['transition', 'transition-duration', 'transition-property']],
    ['color: red !important; width: 0', ['color', 'width']],
    ['COLOR: RED; Width: 0PX', ['color', 'width']],
    ['color: red; color: blue', ['color']],
    ['--x: 1px; color: red', ['--x', 'color']],
    ['color: red; --x:  a  b ', ['--x', 'color']],
    ['opacity: .5; z-index: 0010', ['opacity', 'z-index']],
    ['width: calc(1px + 2px)', ['width']],
    ['width: 10px; bogus: 1; height: x', ['width', 'height']],
    ['grid-area: 1 / 2', ['grid-area', 'grid-row-start', 'grid-column-start', 'grid-row-end']],
    ['list-style: none', ['list-style', 'list-style-type']],
    ['gap: 10px', ['gap', 'row-gap', 'column-gap']],
    ['place-items: center', ['place-items', 'align-items']],
    ['', []]
  ];
  T.forEach(function (t) {
    var d = document.createElement('div');
    d.setAttribute('style', t[0]);
    var s = d.style;
    var names = [];
    for (var i = 0; i < s.length; i++) names.push(s[i]);
    out.push(['attr', t[0], s.cssText, s.length, names.join(','), t[1].map(function (p) { return p + '=' + s.getPropertyValue(p); }).join(' | ')]);
    // one CSSOM write makes Blink serialize the block back into the attribute
    s.setProperty('z-index', '3');
    out.push(['after setProperty', t[0], d.getAttribute('style'), s.cssText]);
    var e = document.createElement('div');
    e.style.cssText = t[0];
    out.push(['cssText=', t[0], e.style.cssText, e.getAttribute('style'), e.style.length]);
  });
  // a setter on the block: shorthand then longhand, longhand then shorthand, removeProperty of a shorthand
  var d = document.createElement('div');
  d.style.border = '0'; out.push(['set border', d.style.cssText, d.getAttribute('style'), d.style.borderTopWidth, d.style.length]);
  d.style.borderTopWidth = '5px'; out.push(['then border-top-width', d.style.cssText, d.style.border, d.style.borderWidth]);
  d.style.removeProperty('border'); out.push(['removeProperty border', d.style.cssText, d.style.length]);
  d.style.margin = '1px 2px'; d.style.marginTop = '3px'; out.push(['margin then margin-top', d.style.cssText, d.style.margin]);
  d.style.setProperty('padding', '4px', 'important'); out.push(['padding important', d.style.cssText, d.style.getPropertyPriority('padding'), d.style.getPropertyPriority('padding-top')]);
  d.style.cssText = 'border: 0'; d.style.display = 'block'; out.push(['border then display', d.style.cssText, d.getAttribute('style')]);
  return out;
})()
