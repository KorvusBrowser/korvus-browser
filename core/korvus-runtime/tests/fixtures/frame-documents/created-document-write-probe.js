(() => {
  // open() / write() / close() on a Document no browsing context shows (createHTMLDocument, a DOMParser result): a
  // script-created parser, as stock Chrome 150 answers. One answer per line: what the document holds afterwards.
  const out = [];
  const t = (label, f) => { try { out.push(label + ': ' + f()); } catch (e) { out.push(label + ': threw ' + e.name + ': ' + e.message); } };
  const html = (d) => d.documentElement ? d.documentElement.outerHTML : '(no documentElement)';
  t('open, write, close', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<p>w</p>'); d.close(); return d.body.innerHTML; });
  t('open() answers the document', () => { const d = document.implementation.createHTMLDocument(''); const r = d.open(); d.close(); return r === d; });
  t('write without open', () => { const d = document.implementation.createHTMLDocument('t'); d.write('<p>x</p>'); return html(d); });
  t('two writes, one tag across them', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<p>a'); d.write('b</p>'); d.close(); return d.body.innerHTML; });
  t('writeln', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.writeln('<p>l</p>'); d.close(); return JSON.stringify(d.body.innerHTML); });
  t('write after close', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<p>1</p>'); d.close(); d.write('<p>2</p>'); return d.body.innerHTML; });
  t('body during writing', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<p>m</p>'); const b = d.body ? d.body.innerHTML : 'null'; d.close(); return b; });
  t('a doctype decides the mode', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<!DOCTYPE html><p>s</p>'); d.close(); return d.compatMode; });
  t('no doctype', () => { const d = document.implementation.createHTMLDocument(''); d.open(); d.write('<p>q</p>'); d.close(); return d.compatMode; });
  t('DOMParser document', () => { const d = new DOMParser().parseFromString('<p>old</p>', 'text/html'); d.open(); d.write('<p>new</p>'); d.close(); return d.body.innerHTML; });
  t('open(url, name, features)', () => { const d = document.implementation.createHTMLDocument(''); return String(d.open('about:blank', '_blank', '')); });
  t('an XML document', () => { const d = document.implementation.createDocument(null, 'r'); d.open(); return 'opened'; });
  return out;
})()
