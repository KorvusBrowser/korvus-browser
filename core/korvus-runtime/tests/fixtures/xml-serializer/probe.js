(() => {
  // XMLSerializer and an XML document's markup, as Chrome 150 serializes them (an interactive-widget challenge 2026-10-09)
  const R = [];
  const s = new XMLSerializer();
  const d = document.createElement('div'); d.setAttribute('a', 'x"<>&'); d.innerHTML = '<b>t&amp;&lt;</b><br>text<!--c-->';
  R.push(['html element', s.serializeToString(d)]);
  R.push(['text', s.serializeToString(document.createTextNode('a<b>&"'))]);
  R.push(['comment', s.serializeToString(document.createComment('x'))]);
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.appendChild(document.createElementNS('http://www.w3.org/2000/svg', 'rect'));
  R.push(['svg', s.serializeToString(svg)]);
  const f = document.createDocumentFragment(); f.appendChild(document.createElement('p')); f.appendChild(document.createTextNode('z'));
  R.push(['fragment', s.serializeToString(f)]);
  R.push(['doctype', s.serializeToString(document.implementation.createDocumentType('html', '', ''))]);
  R.push(['doctype ids', s.serializeToString(document.implementation.createDocumentType('svg', '-//W3C//DTD SVG 1.1//EN', 'http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd'))]);
  const x = new DOMParser().parseFromString('<root xmlns:a="urn:a" xmlns="urn:d"><a:c d="1"/><e/></root>', 'application/xml');
  const r = x.documentElement;
  R.push(['xml element names', [r.namespaceURI, r.firstChild.namespaceURI, r.firstChild.prefix, r.firstChild.localName, r.lastChild.namespaceURI]]);
  R.push(['xml attribute names', [...r.attributes].map(a => [a.name, a.namespaceURI, a.prefix, a.localName])]);
  R.push(['xml document', s.serializeToString(x)]);
  R.push(['xml outerHTML', r.outerHTML]);
  R.push(['xml innerHTML', r.innerHTML]);
  const w = document.createElement('div'); w.setAttribute('t', 'a\tb\nc\rd'); w.appendChild(document.createTextNode('x\ty\nz\r>'));
  R.push(['whitespace escapes', s.serializeToString(w)]);
  const t = document.createElement('template'); t.innerHTML = '<i>q</i>';
  R.push(['template', s.serializeToString(t)]);
  const xd = document.implementation.createDocument(null, 'r', null);
  xd.documentElement.appendChild(xd.createProcessingInstruction('pt', 'pd')); xd.documentElement.appendChild(xd.createCDATASection('a]b<&'));
  R.push(['pi and cdata', s.serializeToString(xd)]);
  w.appendChild(document.createElementNS(null, 'nonamespace'));
  R.push(['null namespace child', s.serializeToString(w)]);
  const sv = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); const u = document.createElementNS('http://www.w3.org/2000/svg', 'use'); u.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:href', '#a'); sv.appendChild(u);
  R.push(['xlink attribute', s.serializeToString(sv)]);
  R.push(['html document', s.serializeToString(document.implementation.createHTMLDocument('T'))]);
  const h = new DOMParser().parseFromString('<html xmlns="http://www.w3.org/1999/xhtml" xmlns:foo="urn:f" xml:lang="en"><body><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><use xlink:href="#a"/></svg><p xlink:href="x">t</p></body></html>', 'text/html');
  const he = h.documentElement;
  R.push(['html attributes', [...he.attributes].map(a => [a.name, a.namespaceURI, a.prefix, a.localName])]);
  R.push(['foreign attributes', [...h.querySelector('svg').attributes, ...h.querySelector('use').attributes].map(a => [a.name, a.namespaceURI, a.prefix, a.localName])]);
  R.push(['html p attributes', [...h.querySelector('p').attributes].map(a => [a.name, a.namespaceURI, a.prefix, a.localName])]);
  R.push(['parsed html serialized', s.serializeToString(he)]);
  let threw = 'no';
  try { s.serializeToString({}); } catch (e) { threw = e.name; }
  R.push(['not a node', threw]);
  return R;
})()
