// PARITY P13a -- geometry in client coordinates after scrolling, and nested documents that are laid out.
//
// Evaluated UNMODIFIED by `tests/geometry_scroll_frames_live.rs` and by stock Chrome over CDP
// (`record.py`), section by section and phase by phase, on a loopback `page.html` whose body is empty.
// A phase is one `Runtime.evaluate` (awaitPromise); state carries across phases on `window.__p13a`.
//
// Nothing here reads a clock or the size of the window: every box is placed at a fixed position in a
// document far larger than either engine's viewport, so each number is the same in both whatever the
// window is. Text is set in Arial, which the recording machine renders in the metric-compatible
// Liberation Sans, so a text box measures the same in both engines and the rows are about WHERE boxes
// are, not about which fonts a machine has.
async (section, phase) => {
  const out = [];
  const log = (k, v) => out.push(k + ' => ' + v);
  const S = (window.__p13a = window.__p13a || {});
  const r2 = (n) => (typeof n === 'number' ? String(Math.round(n * 100) / 100) : String(n));
  const rect = (r) => (r ? [r.x, r.y, r.width, r.height].map(r2).join(',') : String(r));
  const rects = (l) => '[' + Array.from(l).map(rect).join(' | ') + ']';
  const T = (k, f) => { try { log(k, f()); } catch (e) { log(k, 'THROW ' + e.name + ': ' + e.message); } };
  const name = (t) => (t === null ? 'null' : t === undefined ? 'undefined'
    : t.nodeType === 9 ? '#document' : (t.id ? '#' + t.id : t.nodeName.toLowerCase()));
  const el = (doc, tag, id, style, parent, text) => {
    const d = doc.createElement(tag);
    if (id) d.id = id;
    if (style) d.setAttribute('style', style);
    if (text) d.textContent = text;
    (parent || doc.body).appendChild(d);
    return d;
  };
  const css = (doc, text) => { const s = doc.createElement('style'); s.textContent = text; doc.head.appendChild(s); };

  if (section === 'scroll') {
    const d = document;
    if (phase === 0) {
      css(d, 'html,body{margin:0;padding:0} #p{font:16px/20px Arial}');
      el(d, 'div', 'tall', 'width:3000px;height:5000px');
      el(d, 'div', 'a', 'position:absolute;top:1000px;left:10px;width:50px;height:20px;padding:3px;border:2px solid black');
      el(d, 'div', 'fx', 'position:fixed;top:10px;left:20px;width:30px;height:30px');
      const sc = el(d, 'div', 'sc', 'position:absolute;top:1200px;left:100px;width:200px;height:100px;overflow:auto');
      el(d, 'div', 'sc-in', 'width:50px;height:500px', sc);
      el(d, 'div', 'sc-item', 'position:absolute;top:150px;left:10px;width:40px;height:20px', sc);
      // A static scroller whose absolutely positioned child has no positioned ancestor inside it: the
      // child's containing block is the initial one, so the scroller's own scroll does not move it.
      const wrap = el(d, 'div', 'wrap', 'position:absolute;top:1400px;left:0;width:400px;height:300px');
      const scs = el(d, 'div', 'scs', 'width:200px;height:100px;overflow:auto', wrap);
      el(d, 'div', 'scs-in', 'height:400px', scs);
      el(d, 'div', 'scs-flow', 'width:30px;height:10px', scs);
      el(d, 'div', 'scs-abs', 'position:absolute;top:50px;left:250px;width:30px;height:10px', scs);
      const p = el(d, 'div', 'p', 'position:absolute;top:1100px;left:0;width:100px');
      p.append('aaaa ');
      el(d, 'span', 'sp', '', p, 'bbbb cccc dddd');
      p.append(' eeee');
      const stc = el(d, 'div', 'stc', 'position:absolute;top:800px;left:500px;width:100px;height:600px');
      el(d, 'div', 'st', 'position:sticky;top:5px;height:30px', stc);
      el(d, 'div', 'st-after', 'height:30px', stc);
      el(d, 'div', 'cp', 'position:absolute;top:1320px;left:60px;width:300px;font:16px Arial', null, 'hello world');
      scrollTo(50, 900);
      sc.scrollTop = 50;
      scs.scrollTop = 20;
      return 'setup';
    }
    const a = d.getElementById('a');
    T('scroll', () => scrollX + ',' + scrollY + ' de.scrollTop=' + d.documentElement.scrollTop);
    T('a.getBoundingClientRect', () => rect(a.getBoundingClientRect()));
    T('a.getClientRects', () => rects(a.getClientRects()));
    T('a offset/client', () => [a.offsetLeft, a.offsetTop, a.offsetWidth, a.clientWidth, a.clientTop].join(','));
    T('fx.getBoundingClientRect', () => rect(d.getElementById('fx').getBoundingClientRect()));
    T('sc.getBoundingClientRect', () => rect(d.getElementById('sc').getBoundingClientRect()));
    T('sc.scrollTop', () => d.getElementById('sc').scrollTop);
    T('sc-in.getBoundingClientRect', () => rect(d.getElementById('sc-in').getBoundingClientRect()));
    T('sc-item.getBoundingClientRect', () => rect(d.getElementById('sc-item').getBoundingClientRect()));
    T('scs-flow.getBoundingClientRect', () => rect(d.getElementById('scs-flow').getBoundingClientRect()));
    T('scs-abs.getBoundingClientRect', () => rect(d.getElementById('scs-abs').getBoundingClientRect()));
    T('sp.getClientRects', () => rects(d.getElementById('sp').getClientRects()));
    T('sp.getBoundingClientRect', () => rect(d.getElementById('sp').getBoundingClientRect()));
    T('st.getBoundingClientRect', () => rect(d.getElementById('st').getBoundingClientRect()));
    T('st-after.getBoundingClientRect', () => rect(d.getElementById('st-after').getBoundingClientRect()));
    T('range(selectNode a).getBoundingClientRect', () => { const r = new Range(); r.selectNode(a); return rect(r.getBoundingClientRect()); });
    T('range(selectNode a).getClientRects', () => { const r = new Range(); r.selectNode(a); return rects(r.getClientRects()); });
    T('elementFromPoint(10,110) [a]', () => name(d.elementFromPoint(10, 110)));
    T('elementFromPoint(10,500) [nothing moved there]', () => name(d.elementFromPoint(10, 500)));
    T('elementsFromPoint(10,110)', () => d.elementsFromPoint(10, 110).map(name).join(' '));
    T('elementFromPoint(30,20) [fx]', () => name(d.elementFromPoint(30, 20)));
    T('elementFromPoint(80,410) [sc-item]', () => name(d.elementFromPoint(80, 410)));
    T('elementFromPoint(65,550) [scs-in]', () => name(d.elementFromPoint(65, 550)));
    T('elementFromPoint(210,555) [scs-abs]', () => name(d.elementFromPoint(210, 555)));
    T('elementFromPoint(460,10) [st]', () => name(d.elementFromPoint(460, 10)));
    const cp = (x, y) => {
      const c = d.caretPositionFromPoint(x, y);
      if (!c) return 'null';
      const n = c.offsetNode;
      return (n.nodeType === 3 ? name(n.parentNode) + '/#text' : name(n)) + ':' + c.offset + ' ' + Object.prototype.toString.call(c)
        + ' rect=' + rect(c.getClientRect());
    };
    T('caretPositionFromPoint(10,110) [a]', () => cp(10, 110));
    T('caretPositionFromPoint(80,410) [clipped sc-item]', () => cp(80, 410));
    T('caretPositionFromPoint(12,428) [cp h]', () => cp(12, 428));
    T('caretPositionFromPoint(19.5,428) [cp h|e]', () => cp(19.5, 428));
    T('caretPositionFromPoint(36,428) [cp l|o]', () => cp(36, 428));
    T('caretPositionFromPoint(250,428) [cp past the end]', () => cp(250, 428));
    T('caretPositionFromPoint(5,210) [sp line 1]', () => cp(5, 210));
    T('caretPositionFromPoint(-5,5000) [outside]', () => cp(-5, 5000));
    T('caretPositionFromPoint() arity', () => d.caretPositionFromPoint(1));
    T('CaretPosition.prototype.offset on a foreign receiver', () => Object.getOwnPropertyDescriptor(CaretPosition.prototype, 'offset').get.call({}));
    // Back to the origin: the same boxes where the layout put them.
    scrollTo(0, 0);
    T('after scrollTo(0,0) a', () => rect(a.getBoundingClientRect()));
    T('after scrollTo(0,0) st', () => rect(d.getElementById('st').getBoundingClientRect()));
    T('after scrollTo(0,0) elementFromPoint(30,20) [fx]', () => name(d.elementFromPoint(30, 20)));
    T('after scrollTo(0,0) elementFromPoint(510,810) [st]', () => name(d.elementFromPoint(510, 810)));
    return out.join('\n');
  }

  if (section === 'frames') {
    const d = document;
    if (phase === 0) {
      css(d, 'html,body{margin:0;padding:0} iframe{position:absolute}');
      const mk = (id, style, setup) => {
        const f = d.createElement('iframe');
        f.id = id;
        f.setAttribute('style', style);
        const loaded = new Promise((r) => { f.onload = r; });
        setup(f);
        d.body.appendChild(f);
        return loaded;
      };
      await Promise.all([
        // The brief's case, and its own reset: body margin 0 INSIDE the frame.
        mk('f1', 'left:20px;top:30px;width:300px;height:200px;border:0', (f) => {
          f.srcdoc = '<!doctype html><style>html,body{margin:0}</style><div id="in" style="margin:10px;width:50px;height:50px"></div><div id="auto" style="height:5px"></div>';
        }),
        // No style of its own: the PARENT's `body{margin:0}` must not reach it, and the default
        // border + a padding make the viewport the content box, not the border box.
        mk('f2', 'left:400px;top:30px;width:300px;height:200px;padding:5px', (f) => {
          f.srcdoc = '<!doctype html><div id="in" style="width:50px;height:50px"></div><p id="para" style="font:16px Arial">x</p>';
        }),
        // Content taller than the frame: the frame's viewport shows a scrollbar.
        mk('f3', 'left:20px;top:300px;width:300px;height:200px;border:0', (f) => {
          f.srcdoc = '<!doctype html><style>html,body{margin:0}</style><div id="in" style="width:50px;height:50px;margin-top:100px"></div><div style="height:1000px"></div>';
        }),
        // A same-origin `src`.
        mk('f4', 'left:400px;top:300px;width:250px;height:150px;border:0', (f) => { f.src = '/frame.html'; }),
      ]);
      // An initial about:blank frame written into by its parent.
      const f5 = d.createElement('iframe');
      f5.id = 'f5';
      f5.setAttribute('style', 'left:700px;top:30px;width:120px;height:80px;border:0');
      d.body.appendChild(f5);
      const b = f5.contentDocument && f5.contentDocument.body;
      if (b) el(f5.contentDocument, 'div', 'in', 'width:20px;height:20px', b);
      return 'setup';
    }
    const frame = (id) => d.getElementById(id);
    const report = (id) => {
      const f = frame(id);
      const w = f.contentWindow;
      const fd = f.contentDocument;
      T(id + ' contentDocument', () => !!fd);
      if (!fd) return;
      const de = fd.documentElement;
      const inn = fd.getElementById('in');
      T(id + ' inner', () => w.innerWidth + 'x' + w.innerHeight);
      T(id + ' de.client', () => de.clientWidth + 'x' + de.clientHeight);
      T(id + ' de.scroll', () => de.scrollWidth + 'x' + de.scrollHeight);
      T(id + ' de.getBoundingClientRect', () => rect(de.getBoundingClientRect()));
      T(id + ' body.getBoundingClientRect', () => rect(fd.body.getBoundingClientRect()));
      T(id + ' in.getBoundingClientRect', () => rect(inn.getBoundingClientRect()));
      T(id + ' in.getClientRects', () => rects(inn.getClientRects()));
      T(id + ' in offset/client', () => [inn.offsetLeft, inn.offsetTop, inn.offsetWidth, inn.offsetHeight, inn.clientWidth].join(','));
      T(id + ' elementFromPoint(inside in)', () => { const r = inn.getBoundingClientRect(); return name(fd.elementFromPoint(r.x + 5, r.y + 5)); });
      T(id + ' elementsFromPoint(inside in)', () => { const r = inn.getBoundingClientRect(); return fd.elementsFromPoint(r.x + 5, r.y + 5).map(name).join(' '); });
    };
    if (phase === 1) {
      T('f1 frame element rect', () => rect(frame('f1').getBoundingClientRect()));
      T('f2 frame element rect', () => rect(frame('f2').getBoundingClientRect()));
      for (const id of ['f1', 'f2', 'f3', 'f4', 'f5']) report(id);
      T('f2 para rect', () => rect(frame('f2').contentDocument.getElementById('para').getBoundingClientRect()));
      T('parent elementFromPoint over f1', () => name(d.elementFromPoint(40, 50)));
      // Resize one frame and change its document: both are seen.
      frame('f1').style.width = '400px';
      const fd = frame('f1').contentDocument;
      if (fd) fd.getElementById('in').style.width = '70px';
      T('f1 after resize inner', () => frame('f1').contentWindow.innerWidth + 'x' + frame('f1').contentWindow.innerHeight);
      T('f1 after resize de.clientWidth', () => fd.documentElement.clientWidth);
      T('f1 after resize auto', () => rect(fd.getElementById('auto').getBoundingClientRect()));
      T('f1 after resize in', () => rect(fd.getElementById('in').getBoundingClientRect()));
      return out.join('\n');
    }
    // The frame scrolls its own document; the parent does not move.
    const f3 = frame('f3');
    const w = f3.contentWindow;
    T('f3 scrollTo(0,70)', () => { w.scrollTo(0, 70); return w.scrollX + ',' + w.scrollY + ' parent ' + scrollX + ',' + scrollY; });
    T('f3 in after scroll', () => rect(f3.contentDocument.getElementById('in').getBoundingClientRect()));
    T('f1 in unaffected', () => rect(frame('f1').contentDocument.getElementById('in').getBoundingClientRect()));
    T('f3 elementFromPoint(10,40)', () => name(f3.contentDocument.elementFromPoint(10, 40)));
    return out.join('\n');
  }
  return 'no such section';
}
