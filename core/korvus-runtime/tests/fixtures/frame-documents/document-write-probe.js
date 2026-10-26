(async () => {
  // A frame document's document.write/open/close, and a frame's load events, as stock Chrome 150 answers them
  const R = [];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const mk = (where) => { const f = document.createElement('iframe'); (where || document.body).appendChild(f); return f; };
  const props = ['display', 'accentColor', 'marginTop', 'color', 'fontSize', 'width'];
  const style = (cs) => props.map((p) => cs[p]).concat(cs.length > 0);
  const desc = (d) => [d.readyState, d.body ? d.body.innerHTML : null, d.compatMode];
  // 1. write() with no open(): the document open steps run first -- the same Document, emptied, its listeners erased
  {
    const f = mk(); const d = f.contentDocument; const ev = [];
    d.addEventListener('click', () => ev.push('old listener'));
    d.write('<p id=a>one</p>');
    R.push(['write without open', d === f.contentDocument, desc(d)]);
    d.write('<p id=b>two</p>');
    R.push(['a second write', desc(d)]);
    d.dispatchEvent(new Event('click'));
    d.close();
    R.push(['closed', d === f.contentDocument, desc(d), ev]);
    f.remove();
  }
  // close() with no open() is nothing; a write() after close() opens the document again
  {
    const f = mk(); const d = f.contentDocument; const ev = [];
    f.onload = () => ev.push('iframe load');
    d.body.appendChild(d.createElement('span'));
    d.close();
    await wait(30);
    R.push(['close() alone', d === f.contentDocument, d.body.innerHTML, d.readyState, ev]);
    f.remove();
  }
  {
    const f = mk(); const d = f.contentDocument;
    d.write('<p>1</p>'); d.close(); d.write('<p>2</p>');
    R.push(['write after close', d === f.contentDocument, d.body.innerHTML, d.readyState]);
    d.close();
    f.remove();
  }
  // the document open steps take the URL of the document whose script opened it (the entry document)
  {
    const u = (x) => x.replace(location.origin, 'ORIGIN');
    const f = mk(); const d = f.contentDocument;
    const before = [u(d.URL), u(f.contentWindow.location.href)];
    d.open();
    R.push(['open from the parent', before, [u(d.URL), u(f.contentWindow.location.href), u(d.baseURI), d.readyState]]);
    d.close();
    f.remove();
  }
  // 2. a body written that way has a computed style, read from the parent's window and from the frame's own
  {
    const f = mk(); const d = f.contentDocument;
    d.write('<html><head></head><body></body></html>'); d.close();
    R.push(['written body, parent getComputedStyle', style(getComputedStyle(d.body))]);
    R.push(['written body, own getComputedStyle', style(f.contentWindow.getComputedStyle(d.body))]);
    f.remove();
  }
  {
    const outer = mk(); const ow = outer.contentWindow, od = outer.contentDocument;
    const inner = od.createElement('iframe'); od.body.appendChild(inner);
    const idoc = inner.contentDocument;
    idoc.write('<html><head></head><body></body></html>'); idoc.close();
    R.push(['a blank frame in a frame, written', style(ow.getComputedStyle(idoc.body)), style(getComputedStyle(idoc.body))]);
    outer.remove();
  }
  // 3. close(): the end of the document runs inside it -- interactive, DOMContentLoaded, complete, the window's load,
  // then the frame element's
  const ends = async (label, opener) => {
    const f = mk(); const d = f.contentDocument, w = f.contentWindow; const ev = []; let phase = 'before';
    f.addEventListener('load', () => ev.push(phase + ': iframe load ' + d.readyState));
    opener(d);
    d.addEventListener('readystatechange', () => ev.push(phase + ': readystatechange ' + d.readyState));
    d.addEventListener('DOMContentLoaded', () => ev.push(phase + ': DOMContentLoaded'));
    w.addEventListener('load', () => ev.push(phase + ': window load ' + d.readyState));
    ev.push('open: ' + d.readyState);
    const t1 = w.performance.now();
    phase = 'in close'; d.close(); phase = 'after close';
    ev.push('returned: ' + d.readyState, 'the clock went on: ' + (w.performance.now() >= t1));
    await wait(30);
    R.push([label, ev]);
    f.remove();
  };
  await ends('write, close', (d) => d.write('<p>x</p>'));
  await ends('open, write, close', (d) => { d.open(); d.write('<p>x</p>'); });
  await ends('open, close', (d) => d.open());
  // 4. a frame that navigates: its window's load, then its element's (the first navigation of a same-origin frame
  // keeps the initial document's Window, so a listener on it hears the new document's load)
  const loads = async (label, setup) => {
    const f = document.createElement('iframe'); const ev = [];
    f.addEventListener('load', () => ev.push('iframe load'));
    setup(f);
    document.body.appendChild(f);
    f.contentWindow.addEventListener('load', () => ev.push('window load ' + f.contentDocument.URL.replace(location.origin, '')));
    for (let i = 0; i < 60 && !ev.length; i++) await wait(5);
    await wait(60);
    R.push([label, ev]);
    f.remove();
  };
  await loads('srcdoc', (f) => { f.srcdoc = '<p>hi</p>'; });
  await loads('src, same origin', (f) => { f.src = '/frame?x'; });
  return R;
})()
