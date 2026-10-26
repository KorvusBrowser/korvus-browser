(async () => {
  // How a DOM method converts its arguments, Chrome 150: every argument first and in order, the method's steps
  // after; a string argument's Text node in the method's node document, never through the page's createTextNode;
  // the IDL's null / undefined rules per member.
  const R = [];
  const log = [];
  const take = () => { const l = log.join(','); log.length = 0; return l; };
  const tagged = (name, value) => ({ toString() { log.push(name); return value; } });
  const thrower = { toString() { log.push('throw'); throw new Error('no'); } };
  const row = (name, f) => {
    let effect;
    try { effect = f(); } catch (e) { effect = 'threw ' + e.name + ': ' + e.message; }
    R.push([name, take(), typeof effect === 'string' ? effect : JSON.stringify(effect)]);
  };
  const div = () => document.createElement('div');
  const orig = Document.prototype.createTextNode;
  Document.prototype.createTextNode = function (t) { log.push('createTextNode'); return orig.call(this, t); };

  row('setAttribute converts the name, then the value', () => { const d = div(); d.setAttribute(tagged('name', 'data-a'), tagged('value', 'v')); return d.getAttribute('data-a'); });
  row('setAttribute: a throwing name stops before the value', () => { const d = div(); d.setAttribute(thrower, tagged('value', 'v')); return d.attributes.length; });
  row('setAttributeNS converts ns, name, value in order', () => { const d = div(); d.setAttributeNS(tagged('ns', 'urn:x'), tagged('qname', 'p:q'), tagged('value', 'v')); return d.getAttributeNS('urn:x', 'q'); });
  row('append(string) calls no createTextNode', () => { const d = div(); d.append('x', 'y'); return d.childNodes.length + ':' + d.textContent; });
  row('before() on a parentless node still converts its arguments', () => { const d = div(); d.before(tagged('arg', 'a')); return d.parentNode === null; });
  row('after() on a parentless node still converts its arguments', () => { const d = div(); d.after(tagged('arg', 'a')); return d.parentNode === null; });
  row('replaceWith() on a parentless node still converts its arguments', () => { const d = div(); d.replaceWith(tagged('arg', 'a')); return d.parentNode === null; });
  row('append converts every argument before inserting any', () => { const d = div(); try { d.append('a', thrower, 'b'); } catch (e) {} return d.childNodes.length; });
  row('replaceChildren whose argument throws keeps the children', () => { const d = div(); d.append('a', 'b'); try { d.replaceChildren('c', thrower); } catch (e) {} return d.textContent; });
  row('replaceChildren that fails validity keeps the children', () => { const d = div(); d.append('a'); const p = div(); p.append(d); try { d.replaceChildren(p); } catch (e) { return 'threw ' + e.name + ':' + d.textContent; } return 'no throw:' + d.textContent; });
  row('template content append: the Text node is the template document\'s', () => { const t = document.createElement('template'); t.content.append('x'); return t.content.firstChild.ownerDocument === t.content.ownerDocument && t.content.ownerDocument !== document; });
  row('parsed document append: the Text node is that document\'s', () => { const doc = new DOMParser().parseFromString('<p></p>', 'text/html'); doc.body.append('y'); return doc.body.firstChild.ownerDocument === doc; });
  row('parsed document before(): the Text node is that document\'s', () => { const doc = new DOMParser().parseFromString('<p></p>', 'text/html'); doc.body.firstChild.before('z'); return doc.body.firstChild.nodeName + ':' + (doc.body.firstChild.ownerDocument === doc); });
  row('created document append: the Text node is that document\'s', () => { const doc = document.implementation.createHTMLDocument('t'); doc.body.append('w'); return doc.body.firstChild.ownerDocument === doc; });
  row('innerText = null', () => { const d = div(); d.innerText = null; return JSON.stringify(d.textContent); });
  row('innerText = undefined', () => { const d = div(); d.innerText = undefined; return JSON.stringify(d.textContent); });
  row('textContent = undefined', () => { const d = div(); d.textContent = undefined; return JSON.stringify(d.textContent); });
  row('outerHTML = null', () => { const d = div(); const c = d.appendChild(div()); c.outerHTML = null; return JSON.stringify(d.innerHTML); });
  row('innerHTML = undefined', () => { const d = div(); d.innerHTML = undefined; return JSON.stringify(d.innerHTML); });
  row('iframe.srcdoc = null', () => { const f = document.createElement('iframe'); f.srcdoc = null; return JSON.stringify(f.getAttribute('srcdoc')); });
  row('iframe.srcdoc = undefined', () => { const f = document.createElement('iframe'); f.srcdoc = undefined; return JSON.stringify(f.getAttribute('srcdoc')); });
  row('script.text = null', () => { const s = document.createElement('script'); s.text = null; return JSON.stringify(s.text); });
  row('title.text = null', () => { const t = document.createElement('title'); t.text = null; return JSON.stringify(t.text); });
  row('insertAdjacentHTML(pos, null)', () => { const d = div(); d.insertAdjacentHTML('beforeend', null); return JSON.stringify(d.innerHTML); });
  row('createContextualFragment(null)', () => { const r = document.createRange(); r.selectNodeContents(document.body); return JSON.stringify(r.createContextualFragment(null).textContent); });
  row('frame document.write(TrustedHTML, string, TrustedHTML)', () => {
    const p = trustedTypes.createPolicy('w', { createHTML: (s) => s });
    const f = document.createElement('iframe'); document.body.append(f);
    const TH = TrustedHTML.prototype; const o = TH.toString; TH.toString = function () { log.push('TrustedHTML.toString'); return o.call(this); };
    const d = f.contentDocument; d.open(); d.write(p.createHTML('<b>1</b>'), '<i>2</i>', p.createHTML('<u>3</u>')); d.close();
    const out = d.body ? d.body.innerHTML : '(no body)'; f.remove(); TH.toString = o; return out;
  });
  row('frame document.write(string, TrustedHTML): the DOMString overload', () => {
    const p = trustedTypes.createPolicy('w2', { createHTML: (s) => s });
    const f = document.createElement('iframe'); document.body.append(f);
    const TH = TrustedHTML.prototype; const o = TH.toString; TH.toString = function () { log.push('TrustedHTML.toString'); return o.call(this); };
    const d = f.contentDocument; d.open(); d.write('<i>a</i>', p.createHTML('<u>b</u>')); d.close();
    const out = d.body ? d.body.innerHTML : '(no body)'; f.remove(); TH.toString = o; return out;
  });
  row('title.text set calls no textContent setter', () => {
    const desc = Object.getOwnPropertyDescriptor(Node.prototype, 'textContent');
    Object.defineProperty(Node.prototype, 'textContent', { configurable: true, get: desc.get, set(v) { log.push('Node.textContent set'); desc.set.call(this, v); } });
    const t = document.createElement('title'); t.text = 'tt'; const s = document.createElement('script'); s.text = 'ss';
    Object.defineProperty(Node.prototype, 'textContent', desc);
    return t.text + ':' + s.text;
  });
  Document.prototype.createTextNode = orig;
  return R;
})()
