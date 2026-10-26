(async () => {
  const R = [];
  const SKIP_IFACE = new Set(['Window', 'Location', 'History', 'Navigation', 'WorkerGlobalScope', 'DedicatedWorkerGlobalScope', 'Object', 'Function', 'Array', 'String', 'Number', 'Boolean', 'Symbol', 'BigInt', 'Date', 'RegExp', 'Error', 'Map', 'Set', 'WeakMap', 'WeakSet', 'Promise', 'Proxy', 'ArrayBuffer', 'SharedArrayBuffer', 'DataView', 'Intl', 'WeakRef', 'FinalizationRegistry', 'Iterator', 'AsyncIterator', 'AggregateError', 'EvalError', 'RangeError', 'ReferenceError', 'SyntaxError', 'TypeError', 'URIError', 'SuppressedError', 'DisposableStack', 'AsyncDisposableStack', 'Float16Array', 'Float32Array', 'Float64Array', 'Int8Array', 'Int16Array', 'Int32Array', 'Uint8Array', 'Uint8ClampedArray', 'Uint16Array', 'Uint32Array', 'BigInt64Array', 'BigUint64Array']);
  const SKIP_NAME = /^(write|writeln|open|close|submit|requestSubmit|reload|assign|replace|print|go|back|forward|pushState|replaceState|postMessage|alert|confirm|prompt|showModal|show|showPopover|togglePopover|requestFullscreen|exitFullscreen|webkitRequestFullscreen|webkitRequestFullScreen|webkitExitFullscreen|webkitCancelFullScreen|requestPointerLock|exitPointerLock|click|focus|blur|navigate|traverseTo|updateCurrentEntry|execCommand|share|lock|unlock|requestPictureInPicture|exitPictureInPicture|play|load|pause|start|stop|terminate|abort|send|sendBeacon|vibrate|register|unregister|update|getUserMedia|getDisplayMedia|webkitGetUserMedia|requestMIDIAccess|requestMediaKeySystemAccess|requestDevice|requestPort|requestAdapter|requestSession|requestPermission|query|clear|setItem|removeItem|delete|deleteDatabase|cancel|enqueue|transfer|transferFromImageBitmap|transferControlToOffscreen|captureStream|scrollIntoView|scrollIntoViewIfNeeded|scroll|scrollTo|scrollBy|resizeTo|resizeBy|moveTo|moveBy|stopPropagation|preventDefault|dispatchEvent|getDirectory|requestStorageAccess|requestStorageAccessFor|hasStorageAccess|requestVideoFrameCallback|setSinkId|setPointerCapture|releasePointerCapture|requestAnimationFrame|attachShadow|setHTMLUnsafe|setHTML)$/;
  const fmt = (v) => v === undefined ? 'undefined' : v === null ? 'null' : typeof v === 'object' ? Object.prototype.toString.call(v) : typeof v === 'function' ? 'function' : typeof v;
  const pend = [];
  const call = (k, f) => {
    let v;
    try { v = f(); } catch (e) { R.push([k, 'THROW ' + e.name + ': ' + e.message]); return; }
    if (v && typeof v.then === 'function' && Object.prototype.toString.call(v) === '[object Promise]') {
      const i = R.length; R.push([k, 'PROMISE']);
      v.catch(() => {});
      pend.push(Promise.race([v.then((x) => 'RESOLVE ' + fmt(x), (e) => 'REJECT ' + (e && e.name) + ': ' + (e && e.message)), new Promise((r) => setTimeout(() => r('PENDING'), 400))]).then((s) => { R[i] = [k, s]; }));
      return;
    }
    R.push([k, 'OK ' + fmt(v)]);
  };
  const names = Object.getOwnPropertyNames(self).filter((n) => /^[A-Z]/.test(n) && !SKIP_IFACE.has(n)).filter((n) => { try { return typeof self[n] === 'function' && self[n].prototype; } catch (e) { return false; } }).sort();
  for (const n of names) {
    const p = self[n].prototype;
    for (const k of Object.getOwnPropertyNames(p)) {
      if (k === 'constructor') continue;
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (d.get) call(n + '.get ' + k, () => d.get.call({}));
      else if (typeof d.value === 'function' && !SKIP_NAME.test(k)) call(n + '.' + k, () => d.value.call({}));
    }
  }
  // the objects reached without a global name
  const hidden = { MemoryInfo: performance.memory, Quota: navigator.webkitTemporaryStorage };
  for (const [n, o] of Object.entries(hidden)) { const p = Object.getPrototypeOf(o); for (const k of Object.getOwnPropertyNames(p)) { if (k === 'constructor') continue; const d = Object.getOwnPropertyDescriptor(p, k); if (d.get) call(n + '.get ' + k, () => d.get.call({})); else if (typeof d.value === 'function') call(n + '.' + k, () => d.value.call({})); } }
  await Promise.all(pend);
  return R;
})()
