(async () => {
  // SVGTextContentElement's methods called on the prototype with no argument, and on an instance with none.
  const R = [];
  const t = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  const P = SVGTextContentElement.prototype;
  for (const m of ['getNumberOfChars', 'getComputedTextLength', 'getSubStringLength', 'getStartPositionOfChar', 'getEndPositionOfChar', 'getExtentOfChar', 'getRotationOfChar', 'getCharNumAtPosition', 'selectSubString']) {
    let a, b;
    try { a = String(P[m].call(P)); } catch (e) { a = 'throw:' + e.message; }
    try { b = String(P[m].call(t)); } catch (e) { b = 'throw:' + e.message; }
    R.push([m, P[m].length, a, b]);
  }
  return R;
})()
