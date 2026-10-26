(function () {
  // The presentational hints of the embedded and replaced elements (Blink's CollectStyleForPresentationAttribute):
  // width / height (AddHTMLLengthToStyle, with the `auto w / h` aspect-ratio of img, video, canvas and input
  // type=image), hspace / vspace, border, align, valign and an iframe's frameborder -- swept over the elements that map
  // them and the ones that do not -- and the table family's lengths. Every case sits alone in a 400px-wide wrapper (a
  // 300px-tall one for the percentage heights), so no row depends on the window's size or on a font. Rows: [case, answer].
  var GOOD = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==';
  var KINDS = {
    img: function () { return document.createElement('img'); },
    iframe: function () { return document.createElement('iframe'); },
    video: function () { return document.createElement('video'); },
    canvas: function () { return document.createElement('canvas'); },
    embed: function () { var e = document.createElement('embed'); e.setAttribute('src', 'data:text/html,e'); e.setAttribute('type', 'text/html'); return e; },
    object: function () { var e = document.createElement('object'); e.setAttribute('data', 'data:text/html,o'); e.setAttribute('type', 'text/html'); return e; },
    'input-image': function () { var e = document.createElement('input'); e.setAttribute('type', 'image'); return e; },
    'input-text': function () { var e = document.createElement('input'); return e; }
  };
  var root = document.createElement('div');
  root.style.cssText = 'position:absolute;left:0;top:0;width:400px';
  document.body.appendChild(root);
  var sheet = document.createElement('style');
  sheet.textContent = '.w10 { width: 10px } .hauto { height: auto } .w100 { width: 100px }';
  document.head.appendChild(sheet);
  var cases = [];
  function add(label, kind, attrs, read, wrapCss) {
    var wrap = document.createElement('div');
    wrap.style.cssText = 'width:400px;' + (wrapCss || '');
    var el = KINDS[kind]();
    attrs.forEach(function (a) { el.setAttribute(a[0], a[1]); });
    wrap.appendChild(el);
    root.appendChild(wrap);
    cases.push([label, el, read]);
    return el;
  }
  function cs(el, p) { return getComputedStyle(el).getPropertyValue(p); }
  var READ = {
    // the box: computed width / height (the used size of a rendered box), its border box, the aspect-ratio
    dims: function (el) { return [cs(el, 'width'), cs(el, 'height'), el.offsetWidth, el.offsetHeight, cs(el, 'aspect-ratio')].join(' '); },
    // a text field's size is its font's: only whether the attribute changed it
    textdims: function (el, base) { return [cs(el, 'width') === cs(base, 'width'), cs(el, 'height') === cs(base, 'height'), cs(el, 'aspect-ratio')].join(' '); },
    space: function (el) { return ['margin-top', 'margin-right', 'margin-bottom', 'margin-left'].map(function (p) { return cs(el, p); }).join(' '); },
    border: function (el) { return ['border-top-width', 'border-top-style', 'border-left-width', 'border-left-style'].map(function (p) { return cs(el, p); }).join(' '); },
    align: function (el) { return ['float', 'vertical-align', 'text-align'].map(function (p) { return cs(el, p); }).join(' '); },
    frame: function (el) { return [cs(el, 'border-top-width'), cs(el, 'border-top-style'), cs(el, 'border-right-width'), el.offsetWidth, el.clientWidth].join(' '); }
  };
  var baseText = add('input-text plain', 'input-text', [], READ.dims);
  cases.pop();
  var kinds = ['img', 'iframe', 'video', 'canvas', 'embed', 'object', 'input-image', 'input-text'];
  var WIDTHS = ['100', '50%', '12.5', '0', '10*', ' 30', '30px', 'abc', '', '5.%', '+5', '-5', '1e2', '40.', '.5', '0%', '100.75%', '7.5.5', '1,5'];
  // the hints themselves, read off a `display: none` element: its computed width / height are the cascaded values
  kinds.forEach(function (k) {
    var none = [['style', 'display:none']];
    var read = function (el) { return [cs(el, 'width'), cs(el, 'height'), cs(el, 'aspect-ratio')].join(' '); };
    add(k + ' computed plain', k, none, read);
    WIDTHS.forEach(function (v) { add(k + ' computed width="' + v + '"', k, none.concat([['width', v]]), read); });
    ['60', '50%', '0', 'abc', '7.5', '10*'].forEach(function (v) { add(k + ' computed height="' + v + '"', k, none.concat([['height', v]]), read); });
    [['40', '30'], ['40', '50%'], ['40.5', '30.25'], ['0', '0'], ['40', '30*'], ['1e2', '3']].forEach(function (p) {
      add(k + ' computed width="' + p[0] + '" height="' + p[1] + '"', k, none.concat([['width', p[0]], ['height', p[1]]]), read);
    });
  });
  kinds.forEach(function (k) {
    var read = k === 'input-text' ? function (el) { return READ.textdims(el, baseText); } : READ.dims;
    // an image input's box is its alt text's when it has no image -- a font's size -- so it is read computed only
    if (k !== 'input-image') {
      add(k + ' plain', k, [], read);
      WIDTHS.forEach(function (v) { add(k + ' width="' + v + '"', k, [['width', v]], read); });
      ['60', '50%', '0', 'abc', '7.5'].forEach(function (v) { add(k + ' height="' + v + '"', k, [['height', v]], read); });
      add(k + ' height="50%" in 300px', k, [['height', '50%']], read, 'height:300px');
      [['40', '30'], ['40', '50%'], ['40', 'abc'], ['40', ''], ['0', '30'], ['40.5', '30'], ['40px', '30'], ['-4', '3'], ['40', '0']].forEach(function (p) {
        add(k + ' width="' + p[0] + '" height="' + p[1] + '"', k, [['width', p[0]], ['height', p[1]]], read);
      });
      add(k + ' width=40 height=30 .w100', k, [['width', '40'], ['height', '30'], ['class', 'w100']], read);
      add(k + ' width=40 height=30 .hauto', k, [['width', '40'], ['height', '30'], ['class', 'hauto']], read);
      add(k + ' width=100 .w10', k, [['width', '100'], ['class', 'w10']], read);
      add(k + ' width=100 style=width:auto', k, [['width', '100'], ['style', 'width:auto']], read);
    }
    ['10', '10%', 'abc', '0', '2.5'].forEach(function (v) {
      add(k + ' hspace="' + v + '"', k, [['hspace', v]], READ.space);
      add(k + ' vspace="' + v + '"', k, [['vspace', v]], READ.space);
    });
    ['3', '0', '', 'abc', '2px', '-1', ' 4'].forEach(function (v) { add(k + ' border="' + v + '"', k, [['border', v]], READ.border); });
    ['left', 'right', 'middle', 'absmiddle', 'abscenter', 'absbottom', 'top', 'center', 'bottom', 'texttop', 'baseline', 'LEFT', 'justify', 'foo', ''].forEach(function (v) {
      add(k + ' align="' + v + '"', k, [['align', v]], READ.align);
    });
    ['top', 'middle', 'foo', 'TEXT-TOP', '10px'].forEach(function (v) { add(k + ' valign="' + v + '"', k, [['valign', v]], READ.align); });
    // (one order only: Blink's presentation-attribute cache keys an element on its SORTED attributes, so a second
    // element with the reverse order would read the first one's style)
    add(k + ' align=left valign=bottom', k, [['align', 'left'], ['valign', 'bottom']], READ.align);
    ['0', '1', 'no'].forEach(function (v) { add(k + ' frameborder="' + v + '"', k, [['frameborder', v]], k === 'iframe' ? READ.frame : READ.border); });
  });
  ['yes', '', ' 0', '-1', '0x', '00', '+0', '1.5', '99999999999', ' 1', '-0', '0.9', ' 0'].forEach(function (v) {
    add('iframe frameborder="' + v + '"', 'iframe', [['frameborder', v]], READ.frame);
  });
  add('iframe frameborder=0 style=border:3px solid', 'iframe', [['frameborder', '0'], ['style', 'border:3px solid']], READ.frame);
  add('iframe frameborder=0 border=5', 'iframe', [['frameborder', '0'], ['border', '5']], READ.frame);
  // `hidden` on an <embed> is a zero size, not `display: none` (HTMLEmbedElement), whichever attribute comes later winning
  var embedRead = function (el) { return [READ.dims(el), cs(el, 'display'), cs(el, 'content-visibility')].join(' '); };
  add('embed hidden', 'embed', [['hidden', '']], embedRead);
  add('embed width=100 hidden', 'embed', [['width', '100'], ['hidden', '']], embedRead);
  add('embed hidden width=120', 'embed', [['hidden', ''], ['width', '120']], embedRead);
  add('embed hidden=until-found', 'embed', [['hidden', 'until-found']], embedRead);
  add('img hidden', 'img', [['hidden', ''], ['width', '30']], function (el) { return [cs(el, 'display'), cs(el, 'width')].join(' '); });
  // the table family's lengths: AddHTMLLengthToStyle with each element's own flags, read off `display: none` parts
  function tablePart(label, html, sel) {
    var wrap = document.createElement('div');
    wrap.innerHTML = html;
    root.appendChild(wrap);
    cases.push([label, wrap.querySelector(sel), function (e) { return [cs(e, 'width'), cs(e, 'height'), cs(e, 'border-spacing')].join(' '); }]);
  }
  // (not a fractional cellspacing: Blink stores a border spacing as a whole number, `border-spacing: 2.5px` itself
  // computing 2px -- a style-system rule, not a hint's)
  ['10', '10%', '0', 'abc', '', '3*'].forEach(function (v) {
    tablePart('table cellspacing="' + v + '"', '<table cellspacing="' + v + '" style="display:none"><tr><td>x</td></tr></table>', 'table');
  });
  ['100', '50%', '0', '10*', '7.5.5', ''].forEach(function (v) {
    tablePart('table width="' + v + '"', '<table width="' + v + '" style="display:none"><tr><td>x</td></tr></table>', 'table');
    tablePart('table height="' + v + '"', '<table height="' + v + '" style="display:none"><tr><td>x</td></tr></table>', 'table');
    tablePart('td width="' + v + '"', '<table><tr><td width="' + v + '" style="display:none">x</td></tr></table>', 'td');
    tablePart('td height="' + v + '"', '<table><tr><td height="' + v + '" style="display:none">x</td></tr></table>', 'td');
    tablePart('tr height="' + v + '"', '<table><tr height="' + v + '" style="display:none"><td>x</td></tr></table>', 'tr');
    tablePart('col width="' + v + '"', '<table><colgroup><col width="' + v + '" style="display:none"></colgroup><tr><td>x</td></tr></table>', 'col');
    tablePart('colgroup width="' + v + '"', '<table><colgroup width="' + v + '" style="display:none"></colgroup><tr><td>x</td></tr></table>', 'colgroup');
    tablePart('col height="' + v + '"', '<table><colgroup><col height="' + v + '" style="display:none"></colgroup><tr><td>x</td></tr></table>', 'col');
    tablePart('tbody height="' + v + '"', '<table><tbody height="' + v + '" style="display:none"><tr><td>x</td></tr></tbody></table>', 'tbody');
  });
  // a device-check script's device-check frame (ct.captcha-delivery.com/i.js): inside it, innerWidth is the page's
  add('device-check frame', 'iframe', [['width', '100%'], ['height', '100%'], ['style', 'height:100vh;'], ['FRAMEBORDER', '0'], ['border', '0'], ['scrolling', 'yes']], function (el) {
    return [el.offsetWidth, el.offsetHeight === innerHeight, el.contentWindow.innerWidth, el.contentWindow.innerHeight === innerHeight].join(' ');
  });
  add('iframe width=50% innerWidth', 'iframe', [['width', '50%']], function (el) { return [el.offsetWidth, el.contentWindow.innerWidth, el.contentWindow.innerHeight].join(' '); });
  add('iframe width=123 height=45 innerWidth', 'iframe', [['width', '123'], ['height', '45']], function (el) { return [el.offsetWidth, el.offsetHeight, el.contentWindow.innerWidth, el.contentWindow.innerHeight].join(' '); });
  // an image that arrives (2x3): its natural ratio beats the hints' `auto w / h`
  var loaded = [];
  [['src only', []], ['width=60', [['width', '60']]], ['height=60', [['height', '60']]], ['width=60 height=60', [['width', '60'], ['height', '60']]],
   ['width=60 height=60 .hauto', [['width', '60'], ['height', '60'], ['class', 'hauto']]], ['width=50%', [['width', '50%']]],
   ['width=60 height=60 .w100', [['width', '60'], ['height', '60'], ['class', 'w100']]]].forEach(function (c) {
    var el = add('loaded img ' + c[0], 'img', c[1], READ.dims);
    loaded.push(new Promise(function (res) { el.onload = el.onerror = res; setTimeout(res, 1500); }));
    el.src = GOOD;
  });
  return Promise.all(loaded).then(function () {
    return new Promise(function (res) { setTimeout(res, 200); });
  }).then(function () {
    var out = cases.map(function (c) { var v; try { v = c[2](c[1]); } catch (e) { v = 'THREW ' + e.name; } return [c[0], v]; });
    root.remove(); sheet.remove();
    return out;
  });
})()
