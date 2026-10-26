(() => {
  const out = [];
  const t = (label, f) => {
    try { const v = f(); out.push([label, 'ok', typeof v === 'number' && isNaN(v) ? 'NaN' : (v === undefined ? 'undefined' : (typeof v === 'object' && v !== null ? Object.prototype.toString.call(v) : v))]); }
    catch (e) { out.push([label, e.name, e.message]); }
  };
  t('usb-in-bogus', () => new USBInTransferResult('bogus'));
  t('usb-in-ok', () => new USBInTransferResult('ok').status);
  t('usb-in-data-null', () => new USBInTransferResult('ok').data);
  t('usb-in-data-wrong', () => new USBInTransferResult('ok', 5));
  t('usb-in-data', () => { const d = new DataView(new ArrayBuffer(4)); return new USBInTransferResult('ok', d).data === d; });
  t('usb-in-readonly', () => { const r = new USBInTransferResult('ok'); r.status = 'stall'; return r.status; });
  t('usb-out', () => new USBOutTransferResult('ok', -1).bytesWritten);
  t('usb-out-str', () => new USBOutTransferResult('ok', '12').bytesWritten);
  t('usb-out-default', () => new USBOutTransferResult('stall').bytesWritten);
  t('usb-iso-in-3', () => new USBIsochronousInTransferResult(3));
  t('usb-iso-in-obj', () => new USBIsochronousInTransferResult({}));
  t('usb-iso-in-wrong-elem', () => new USBIsochronousInTransferResult([1]));
  t('usb-iso-in', () => { const p = new USBIsochronousInTransferPacket('ok'); const r = new USBIsochronousInTransferResult([p]); return [r.packets.length, r.packets[0] === p, Object.isFrozen(r.packets), r.packets === r.packets].join(); });
  t('usb-iso-in-iter', () => { const p = new USBIsochronousInTransferPacket('ok'); return new USBIsochronousInTransferResult(new Set([p])).packets.length; });
  t('usb-iso-out', () => new USBIsochronousOutTransferResult([new USBIsochronousOutTransferPacket('ok', 2)]).packets[0].bytesWritten);
  t('phrase', () => { const p = new SpeechRecognitionPhrase('x'); return p.phrase + ':' + p.boost; });
  t('phrase-boost', () => new SpeechRecognitionPhrase('x', 0.1).boost);
  t('phrase-nan', () => new SpeechRecognitionPhrase('x', NaN));
  t('fontfaces-frozen', () => Object.isFrozen(new FontFaceSetLoadEvent('x').fontfaces));
  t('fontfaces-given', () => { const a = []; const e = new FontFaceSetLoadEvent('x', { fontfaces: a }); return [e.fontfaces === a, Object.isFrozen(a), Object.isFrozen(e.fontfaces)].join(); });
  t('brand', () => Object.getOwnPropertyDescriptor(USBInTransferResult.prototype, 'status').get.call({}));
  return out;
})()
