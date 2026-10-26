(async () => {
  // a bot challenge's SVG text probe, replayed with the strings stock Chrome 150 measured (live trace,
  // rateyourmusic.com, 2026-10-09 03:53): an about:blank frame in a closed shadow root, a <g> of 19 texts at (32, 32)
  // under scale(1.000998) -- four emoji whose `150px !important` is no value, fifteen hex strings in italic 150px
  // serif -- imported from a template; the group's box, each text's length, the first character's extent, and a
  // length once the frame is gone.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const host = document.createElement('div');
  document.body.append(host);
  const root = host.attachShadow({ mode: 'closed' });
  const outer = document.createElement('div');
  outer.style.height = '100vh'; outer.style.width = '100vw'; outer.style.position = 'absolute'; outer.style.left = '-10000px'; outer.style.visibility = 'hidden';
  const iframe = document.createElement('iframe');
  outer.appendChild(iframe);
  root.appendChild(outer);
  const idoc = iframe.contentDocument;
  const placeholder = document.createElement('div');
  idoc.body.appendChild(placeholder);
  const wrap = document.createElement('div');
  wrap.style.position = 'absolute'; wrap.style.left = '-9999px'; wrap.style.height = 'auto';
  const svg = document.createElementNS(NS, 'svg');
  const g = document.createElementNS(NS, 'g');
  g.id = 'dgjcj0';
  const strings = ["😀", "👩‍❤️‍💋‍👨", "👨‍👩‍👧‍👦", "👨‍👩‍👦", "dc7c811b9561739d9b75bb3e9e1715970a868834e62251b0b9ca02e74d0f42c9", "cab2644828bcc1f220612fde559864da2c5bf0c6eebdcadbb4a4c88c9fcf3927", "37290d74ac4d186e3a8e5785d259d2ec04fac91ae28092e7620ec8bc99e830aa", "6201111b83a0cb5b0922cb37cc442b9a40e24e3b1ce100a4bb204f4c63fd2ac0", "e4d14bef7aa0668546bc69bf39adce5193e7e0006b24bfc81feed38693f38ae7", "10b8ecc0d7ffa69355fc2ad96eae450ee66556bf2f10dba7dc85a2fbcddcdea1", "bb8c8605c55dcc2d650e87ecb9fb01b1c2a95e0fd8e03e2c87c2cf5ebeaf3b0b", "935f68319d4f227e02bfd54a0ddf85b8a242e42a4277aa5ef5eaab691710924e", "bb0347a468d97e98a9c00e37cebec1ab930f6f1221cae0f1fbb92b07e1900ba2", "aa9e9b5c907d50feb410f2a84e81ab72c5ae6724d57ccc53650f9361d33dc734", "1eb79602411ef02cf6fe117897015fff89f80face4eccd50425c45149b148408", "9c19636ddea25f0a13357d01c0aef27a4eb4c7f8cbaf2be35f272273572ce2e5", "00e13ed7af55b27622f1d6eab5bec0147e68efe28dc2b12461117afa1a5ed40e", "10c22bcf4c768b515be4e94bcafc71bf3e8fb5f70b2584bcc8c7533217f2e7f9", "79adb2a2fce5c6ba215fe5f27f532d4e7edbac4b6a5e09e1ef3a08084a904621"];
  strings.forEach((s, i) => {
    const t = document.createElementNS(NS, 'text');
    t.setAttribute('x', '32'); t.setAttribute('y', '32');
    t.style.fontSize = '150px !important'; t.style.height = 'auto'; t.style.position = 'absolute !important'; t.style.transform = 'scale(1.000998)';
    if (i >= 4) t.style.font = 'italic 150px serif';
    t.setAttribute('class', 'yGqF7');
    t.textContent = s;
    g.appendChild(t);
  });
  svg.appendChild(g);
  wrap.appendChild(svg);
  const tpl = document.createElement('template');
  tpl.content.appendChild(wrap);
  const frag = document.importNode(tpl.content, true);
  placeholder.parentNode.replaceChild(frag, placeholder);
  const gg = idoc.getElementById('dgjcj0');
  const b = gg.getBBox();
  R.push(['group box', [b.x, b.y, b.width, b.height]]);
  const list = gg.getElementsByClassName('yGqF7');
  R.push(['lengths', Array.from(list, (t) => t.getComputedTextLength())]);
  const e = list[0].getExtentOfChar(list[0].innerHTML);
  R.push(['first extent', [e.x, e.y, e.width, e.height]]);
  R.push(['styles', list[0].style.fontSize, list[0].style.position, getComputedStyle(list[0]).fontSize]);
  outer.remove();
  R.push(['after removal', list[4].getComputedTextLength()]);
  return R;
})()
