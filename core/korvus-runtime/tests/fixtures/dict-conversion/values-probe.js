(async () => {
  // The answers of dictionary and argument conversions that are not read orders: values a converted member reads
  // back as, listener options, numeric and string arguments, fetch's argument order, non-object dictionaries, and
  // the scroll methods' promise rejections. Rows: [part/label, outcome, detail].
  const out = [];
  for (const r of await (() => {
    const out = [];
    const t = (label, f) => {
      try { const v = f(); out.push([label, 'ok', v === undefined ? 'undefined' : (typeof v === 'number' && isNaN(v) ? 'NaN' : v)]); }
      catch (e) { out.push([label, e.name, e.message]); }
    };
    const fr = document.createElement('iframe'); document.body.appendChild(fr);
    const mc = new MessageChannel();
    t('view-window', () => new MouseEvent('click', {view: window, bubbles: true}).view === window);
    t('view-frame', () => new MouseEvent('click', {view: fr.contentWindow}).view === fr.contentWindow);
    t('view-null', () => new MouseEvent('click', {view: null}).view);
    t('view-object', () => new UIEvent('x', {view: {}}));
    t('view-document', () => new UIEvent('x', {view: document}));
    t('related-body', () => new MouseEvent('x', {relatedTarget: document.body}).relatedTarget === document.body);
    t('related-window', () => new FocusEvent('focus', {relatedTarget: window}).relatedTarget === window);
    t('related-object', () => new MouseEvent('x', {relatedTarget: {}}));
    t('msg-source-window', () => new MessageEvent('message', {source: window, data: 1}).source === window);
    t('msg-source-port', () => new MessageEvent('message', {source: mc.port1}).source === mc.port1);
    t('msg-source-frame', () => new MessageEvent('message', {source: fr.contentWindow}).source === fr.contentWindow);
    t('msg-source-object', () => new MessageEvent('message', {source: {}}));
    t('msg-ports', () => { const e = new MessageEvent('message', {ports: [mc.port1, mc.port2]}); return [e.ports.length, Object.isFrozen(e.ports), e.ports === e.ports].join(); });
    t('msg-ports-bad', () => new MessageEvent('message', {ports: [1]}));
    t('msg-defaults', () => { const e = new MessageEvent('m'); return JSON.stringify([e.data, e.origin, e.lastEventId, e.source, e.ports.length]); });
    t('msg-data-undefined', () => new MessageEvent('m', {data: undefined}).data);
    t('error-defaults', () => { const e = new ErrorEvent('e'); return [String(e.error), e.message, e.filename, e.lineno, e.colno].join('|'); });
    t('error-null', () => String(new ErrorEvent('e', {error: null}).error));
    t('error-lineno', () => new ErrorEvent('e', {lineno: -1, colno: 2.9}).lineno + ':' + new ErrorEvent('e', {colno: 2.9}).colno);
    t('popstate-defaults', () => String(new PopStateEvent('p').state) + ':' + new PopStateEvent('p').hasUAVisualTransition);
    t('custom-detail', () => String(new CustomEvent('c').detail) + ':' + new CustomEvent('c', {detail: 5}).detail);
    t('progress', () => { const e = new ProgressEvent('p', {loaded: 5, total: 10, lengthComputable: true}); return [e.loaded, e.total, e.lengthComputable].join(); });
    t('progress-neg', () => new ProgressEvent('p', {loaded: -1}).loaded);
    t('rejection', () => new PromiseRejectionEvent('x', {promise: Promise.resolve(), reason: 1}).reason);
    t('rejection-no-promise', () => new PromiseRejectionEvent('x', {reason: 1}));
    t('blob-type', () => new Blob(['a'], {type: 'Text/Plain'}).type);
    t('blob-endings', () => new Blob(['a\nb'], {endings: 'native'}).size);
    t('blob-endings-bad', () => new Blob([], {endings: 'x'}));
    t('file-lm', () => new File(['a'], 'f', {lastModified: 5}).lastModified);
    t('file-lm-now', () => Math.abs(new File(['a'], 'f').lastModified - Date.now()) < 1000);
    t('file-type', () => new File(['a'], 'f', {type: 'A/B'}).type);
    t('imagedata', () => new ImageData(1, 1, {colorSpace: 'display-p3'}).colorSpace);
    t('imagedata-bad', () => new ImageData(1, 1, {colorSpace: 'x'}));
    t('throw-getter', () => new Event('e', {get bubbles() { throw new RangeError('b'); }}));
    t('tds-fatal', () => new TextDecoderStream('utf-8', {fatal: true}).fatal);
    t('anim', () => { const e = new AnimationEvent('a', {animationName: 'x', elapsedTime: 1.5, pseudoElement: '::before'}); return [e.animationName, e.elapsedTime, e.pseudoElement].join(); });
    t('wheel', () => { const e = new WheelEvent('w', {deltaY: 3, deltaMode: 1, clientX: 4}); return [e.deltaY, e.deltaMode, e.clientX].join(); });
    t('pointer', () => { const e = new PointerEvent('p', {pointerType: 'pen', isPrimary: true, pointerId: 7, width: 0}); return [e.pointerType, e.isPrimary, e.pointerId, e.width].join(); });
    t('pointer-coalesced', () => new PointerEvent('p').getCoalescedEvents().length);
    t('keyboard', () => { const e = new KeyboardEvent('k', {key: 'Enter', code: 'Enter', keyCode: 13, repeat: true, ctrlKey: true, modifierAltGraph: true}); return [e.key, e.code, e.keyCode, e.repeat, e.ctrlKey, e.getModifierState('AltGraph')].join(); });
    t('mouse', () => { const e = new MouseEvent('m', {button: 65535, buttons: -1, clientX: 1.5, screenY: 2, movementX: 3.7, detail: 2.9, shiftKey: 1}); return [e.button, e.buttons, e.clientX, e.screenY, e.movementX, e.detail, e.shiftKey].join(); });
    t('mouse-nan', () => new MouseEvent('m', {clientX: NaN}));
    t('touch', () => { const tt = new Touch({identifier: 1, target: document.body, clientX: 3}); const e = new TouchEvent('touchstart', {touches: [tt], targetTouches: [], changedTouches: [tt]}); return [e.touches.length, e.touches[0] === tt, e.changedTouches.length].join(); });
    t('touch-bad', () => new TouchEvent('t', {touches: [1]}));
    t('input', () => { const dt = new DataTransfer(); const e = new InputEvent('input', {data: 'x', inputType: 'insertText', dataTransfer: dt, isComposing: true}); return [e.data, e.inputType, e.dataTransfer === dt, e.isComposing, e.getTargetRanges().length].join(); });
    t('input-data-null', () => String(new InputEvent('input').data) + ':' + String(new InputEvent('input').dataTransfer));
    t('focus-related-null', () => String(new FocusEvent('f').relatedTarget));
    t('hashchange', () => { const e = new HashChangeEvent('h', {oldURL: 'a', newURL: 'b'}); return e.oldURL + e.newURL; });
    t('transition', () => { const e = new TransitionEvent('t', {propertyName: 'x', elapsedTime: 2}); return e.propertyName + e.elapsedTime + e.pseudoElement; });
    t('ice', () => String(new RTCPeerConnectionIceEvent('i').candidate));
    t('ice-bad', () => new RTCPeerConnectionIceEvent('i', {candidate: 1}));
    t('uievent-detail', () => new UIEvent('u', {detail: 4294967297}).detail);
    t('cookiechange', () => new CookieChangeEvent('c').changed.length);
    t('composition', () => new CompositionEvent('c', {data: 'x'}).data);
    t('submit', () => String(new SubmitEvent('s').submitter));
    t('submit-bad', () => new SubmitEvent('s', {submitter: document.body}));
    return out;
  })()) out.push(['dvals/' + r[0], r[1], r[2]]);
  for (const r of await (() => {
    const out = [];
    const t = (label, f) => { try { const v = f(); out.push([label, 'ok', v === undefined ? 'undefined' : v]); } catch (e) { out.push([label, e.name, e.message]); } };
    const fire = (target, type, init) => { const ev = new Event(type, Object.assign({cancelable: true}, init)); target.dispatchEvent(ev); return ev.defaultPrevented; };
    t('cb-number', () => addEventListener('x', 5));
    t('cb-string', () => addEventListener('x', 'f'));
    t('cb-symbol-type', () => addEventListener(Symbol('s'), () => {}));
    t('rm-cb-number', () => removeEventListener('x', 5));
    t('opts-number-capture', () => { const d = document.createElement('div'); const p = document.createElement('p'); d.appendChild(p); const seen = []; d.addEventListener('y', () => seen.push('cap'), 1); p.addEventListener('y', () => seen.push('tgt')); p.dispatchEvent(new Event('y', {bubbles: true})); return seen.join(); });
    t('opts-string-capture', () => { const d = document.createElement('div'); const p = document.createElement('p'); d.appendChild(p); const seen = []; d.addEventListener('y', () => seen.push('cap'), 'yes'); p.addEventListener('y', () => seen.push('tgt')); p.dispatchEvent(new Event('y', {bubbles: true})); return seen.join(); });
    t('passive-default-window-touchstart', () => { const f = (e) => e.preventDefault(); addEventListener('touchstart', f); const r = fire(window, 'touchstart'); removeEventListener('touchstart', f); return r; });
    t('passive-default-document-wheel', () => { const f = (e) => e.preventDefault(); document.addEventListener('wheel', f); const r = fire(document, 'wheel'); document.removeEventListener('wheel', f); return r; });
    t('passive-default-body-touchmove', () => { const f = (e) => e.preventDefault(); document.body.addEventListener('touchmove', f); const r = fire(document.body, 'touchmove'); document.body.removeEventListener('touchmove', f); return r; });
    t('passive-default-html-mousewheel', () => { const f = (e) => e.preventDefault(); document.documentElement.addEventListener('mousewheel', f); const r = fire(document.documentElement, 'mousewheel'); document.documentElement.removeEventListener('mousewheel', f); return r; });
    t('passive-default-div-touchstart', () => { const d = document.createElement('div'); document.body.appendChild(d); const f = (e) => e.preventDefault(); d.addEventListener('touchstart', f); const r = fire(d, 'touchstart'); d.remove(); return r; });
    t('passive-default-window-click', () => { const f = (e) => e.preventDefault(); addEventListener('click', f); const r = fire(window, 'click'); removeEventListener('click', f); return r; });
    t('passive-false-window-touchstart', () => { const f = (e) => e.preventDefault(); addEventListener('touchstart', f, {passive: false}); const r = fire(window, 'touchstart'); removeEventListener('touchstart', f); return r; });
    t('passive-true-div-click', () => { const d = document.createElement('div'); const f = (e) => e.preventDefault(); d.addEventListener('click', f, {passive: true}); return fire(d, 'click'); });
    t('signal-removes', () => { const ac = new AbortController(); let n = 0; const d = document.createElement('div'); d.addEventListener('z', () => n++, {signal: ac.signal}); d.dispatchEvent(new Event('z')); ac.abort(); d.dispatchEvent(new Event('z')); return n; });
    t('signal-aborted-adds-nothing', () => { let n = 0; const d = document.createElement('div'); d.addEventListener('z', () => n++, {signal: AbortSignal.abort()}); d.dispatchEvent(new Event('z')); return n; });
    t('signal-dup', () => { const ac = new AbortController(); let n = 0; const f = () => n++; const d = document.createElement('div'); d.addEventListener('z', f); d.addEventListener('z', f, {signal: ac.signal}); ac.abort(); d.dispatchEvent(new Event('z')); return n; });
    t('signal-before-abort-event', () => { const ac = new AbortController(); const seen = []; const d = document.createElement('div'); d.addEventListener('z', () => seen.push('z'), {signal: ac.signal}); ac.signal.addEventListener('abort', () => { d.dispatchEvent(new Event('z')); seen.push('abort'); }); ac.abort(); return seen.join(); });
    t('signal-not-signal', () => addEventListener('z', () => {}, {signal: {}}));
    t('signal-null', () => addEventListener('z', () => {}, {signal: null}));
    t('signal-cap-remove', () => { const ac = new AbortController(); let n = 0; const f = () => n++; const d = document.createElement('div'); d.addEventListener('z', f, {signal: ac.signal, capture: true}); d.addEventListener('z', f); ac.abort(); d.dispatchEvent(new Event('z')); return n; });
    t('null-cb-signal-read', () => { const r = []; addEventListener('z', null, {get signal() { r.push('signal'); return undefined; }}); return r.join(); });
    t('once-passive', () => { let n = 0; const d = document.createElement('div'); d.addEventListener('z', () => n++, {once: true}); d.dispatchEvent(new Event('z')); d.dispatchEvent(new Event('z')); return n; });
    return out;
  })()) out.push(['ael/' + r[0], r[1], r[2]]);
  for (const r of await (() => {
    const out = [];
    const t = (label, f) => { try { const v = f(); out.push([label, 'ok', v === undefined ? 'undefined' : (typeof v === 'number' && isNaN(v) ? 'NaN' : v)]); } catch (e) { out.push([label, e.name, e.message]); } };
    t('point-str', () => DOMPoint.fromPoint({x: '3'}).x);
    t('point-obj', () => DOMPoint.fromPoint({x: {valueOf() { return 4; }}}).x);
    t('point-null', () => DOMPoint.fromPoint({x: null}).x);
    t('point-nan', () => DOMPoint.fromPoint({x: NaN}).x);
    t('point-5', () => DOMPoint.fromPoint(5));
    t('rect-str', () => DOMRect.fromRect({width: '7'}).width);
    t('rect-bool', () => DOMRect.fromRect({height: true}).height);
    t('matrix-str', () => DOMMatrix.fromMatrix({a: '2'}).a);
    t('matrix-is2d-str', () => DOMMatrix.fromMatrix({is2D: 'yes', m33: 2}));
    t('matrix-nan', () => DOMMatrix.fromMatrix({a: NaN}).a);
    t('quad-str', () => DOMQuad.fromRect({x: '1'}).p1.x);
    t('textdecoder-5', () => new TextDecoder('utf-8', 5));
    t('textdecoder-fatal-str', () => new TextDecoder('utf-8', {fatal: 'x'}).fatal);
    t('point-ctor-str', () => new DOMPoint('2').x);
    t('transformpoint-str', () => new DOMMatrix().transformPoint({x: '5'}).x);
    t('imagedata-ctor-str', () => new ImageData(1, 1, {colorSpace: 'srgb'}).colorSpace);
    return out;
  })()) out.push(['serde/' + r[0], r[1], r[2]]);
  for (const r of await (async () => {
    const out = [];
    const spy = (reads) => new Proxy({}, { get(o, k) { reads.push(String(k)); return undefined; }, has(o, k) { reads.push('has:' + String(k)); return false; } });
    const run = async (label, f) => {
      const reads = [];
      let r;
      try { r = f(spy(reads)); } catch (e) { out.push([label, reads.join(','), 'throw ' + e.name + ': ' + e.message]); return; }
      // a promise's answer only when it is already rejected as the call returns (a conversion error); settling later
    // -- the network's answer, or its failure -- is 'resolved': when the server answers is no part of the conversion
    if (r && r.then) {
      const v = await new Promise((res) => {
        let done = false;
        const end = (x) => { if (!done) { done = true; res(x); } };
        r.then(() => end('resolved'), (e) => end('rejected ' + e.name + ': ' + e.message));
        Promise.resolve().then(() => 0).then(() => 0).then(() => end('resolved'));
      });
      out.push([label, reads.join(','), v]);
    }
      else out.push([label, reads.join(','), 'ok']);
    };
    await run('fetch-ftp', (d) => fetch('ftp:', d));
    await run('fetch-bad-url', (d) => fetch('http://[', d));
    await run('fetch-creds', (d) => fetch('http://u:p@127.0.0.1/', d));
    await run('request-ftp', (d) => new Request('ftp:', d));
    await run('request-bad-url', (d) => new Request('http://[', d));
    await run('fetch-mode-bad', (d) => fetch('/x', {mode: 'bogus', get cache() { out.push(['cache-read']); return undefined; }}));
    await run('fetch-signal-bad', (d) => fetch('/x', {signal: {}}));
    await run('fetch-signal-null', (d) => fetch('/x', {signal: null, method: 'HEAD'}));
    await run('fetch-get-body', (d) => fetch('/x', {body: 'a'}));
    await run('request-get-body', (d) => new Request('/x', {body: 'a'}));
    await run('fetch-window-1', (d) => fetch('/x', {window: 1}));
    await run('request-window-null', (d) => new Request('/x', {window: null}).url.slice(-2));
    await run('request-priority-bad', (d) => new Request('/x', {priority: 'x'}));
    await run('request-duplex-bad', (d) => new Request('/x', {duplex: 'x'}));
    await run('request-attr-empty', (d) => new Request('/x', {attributionReporting: {}}));
    return out;
  })()) out.push(['forder/' + r[0], r[1], r[2]]);
  for (const r of await (() => {
    const out = [];
    const t = (label, f) => { try { const v = f(); out.push([label, 'ok', v === undefined ? 'undefined' : v]); } catch (e) { out.push([label, e.name, e.message]); } };
    for (const [n, a] of [['IDBVersionChangeEvent', ['v']], ['SecurityPolicyViolationEvent', ['x']], ['MouseEvent', ['x']], ['Event', ['x']], ['CustomEvent', ['x']], ['BlobEvent', ['x']], ['AudioProcessingEvent', ['x']], ['OfflineAudioCompletionEvent', ['x']], ['GPUUncapturedErrorEvent', ['x']], ['Blob', [[]]], ['File', [[], 'f']], ['Worker', ['data:,']], ['Notification', ['n']], ['Request', ['/']], ['Response', ['']], ['TextDecoder', ['utf-8']], ['IntersectionObserver', [() => {}]], ['AudioContext', []], ['RTCPeerConnection', []], ['FontFace', ['f', 'url(x)']], ['KeyframeEffect', [null, []]], ['ImageData', [1, 1]], ['PerformanceMark', ['m']], ['ReadableStream', []], ['CSSStyleSheet', []], ['DOMMatrix', []]]) {
      t(n + '-5', () => { const o = new window[n](...a, 5); return o && o.close && n === 'AudioContext' ? (o.close(), 'ok') : 'ok'; });
    }
    t('spve-bubbles', () => new SecurityPolicyViolationEvent('x', {bubbles: true}).bubbles);
    t('idb-bubbles', () => new IDBVersionChangeEvent('v', {bubbles: true, oldVersion: 3}).bubbles + ':' + new IDBVersionChangeEvent('v', {oldVersion: 3}).oldVersion);
    t('idb-newversion-null', () => String(new IDBVersionChangeEvent('v', {newVersion: null}).newVersion));
    t('idb-dataloss-msg', () => new IDBVersionChangeEvent('v', {dataLossMessage: 'm'}).dataLossMessage);
    t('spve-disposition', () => new SecurityPolicyViolationEvent('x', {disposition: 'bogus'}));
    t('spve-status', () => new SecurityPolicyViolationEvent('x', {statusCode: 70000, lineNumber: -1}).statusCode + ':' + new SecurityPolicyViolationEvent('x', {lineNumber: -1}).lineNumber);
    return out;
  })()) out.push(['dnotobj/' + r[0], r[1], r[2]]);
  for (const r of await (async () => {
    const out = [];
    const t = async (label, f) => {
      let v;
      try { v = f(); } catch (e) { out.push([label, e.name, e.message]); return; }
      if (v && typeof v.then === 'function') {
        try { await v; out.push([label, 'resolved', '']); } catch (e) { out.push([label, 'rejected ' + e.name, e.message]); }
        return;
      }
      out.push([label, 'ok', v === undefined ? 'undefined' : String(v)]);
    };
    const d = document.createElement('div'); document.body.appendChild(d);
    await t('el-scrollTo-5', () => d.scrollTo(5));
    await t('el-scrollTo-str', () => d.scrollTo('x'));
    await t('el-scrollTo-null', () => d.scrollTo(null));
    await t('el-scrollBy-true', () => d.scrollBy(true));
    await t('win-scrollTo-5', () => scrollTo(5));
    await t('win-scroll-sym', () => scroll(Symbol()));
    await t('el-scrollTo-3args', () => d.scrollTo(1, 2, 3));
    await t('scrollIntoView-5', () => d.scrollIntoView(5));
    await t('scrollIntoView-sym', () => d.scrollIntoView(Symbol()));
    await t('scrollIntoView-bad-block', () => d.scrollIntoView({block: 'x'}));
    await t('scroll-bad-behavior', () => d.scrollTo({behavior: 'x'}));
    await t('focus-5', () => d.focus(5));
    await t('createElement-5', () => document.createElement('div', 5).tagName);
    await t('createElement-sym', () => document.createElement('div', Symbol()));
    await t('define-5', () => customElements.define('x-five-' + Math.random().toString(36).slice(2), class extends HTMLElement {}, 5));
    await t('checkVisibility-5', () => d.checkVisibility(5));
    await t('caret-5', () => document.caretPositionFromPoint(0, 0, 5));
    await t('attachShadow-5', () => document.createElement('div').attachShadow(5));
    await t('attachShadow-null', () => document.createElement('div').attachShadow(null));
    await t('togglePopover-5', () => { d.popover = 'manual'; return d.togglePopover(5); });
    await t('showPopover-5', () => { const e = document.createElement('div'); e.popover = 'manual'; document.body.appendChild(e); return e.showPopover(5); });
    return out;
  })()) out.push(['scroll1/' + r[0], r[1], r[2]]);
  return out;
})()
