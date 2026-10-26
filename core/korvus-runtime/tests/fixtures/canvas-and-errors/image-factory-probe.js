(() => {
  let made = 0, widths = 0, heights = 0;
  const create = Document.prototype.createElement;
  document.createElement = function () { made++; return create.apply(this, arguments); };
  for (const [k, inc] of [['width', () => widths++], ['height', () => heights++]]) {
    const d = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, k);
    Object.defineProperty(HTMLImageElement.prototype, k, {get: d.get, set(v) { inc(); d.set.call(this, v); }, configurable: true});
  }
  const i = new Image(3, 4);
  const j = new Image();
  return [['page-createElement-calls', made], ['page-width-setter-calls', widths], ['page-height-setter-calls', heights],
          ['attributes', i.getAttribute('width'), i.getAttribute('height'), j.hasAttribute('width')], ['idl', i.width, i.height],
          ['tag', i.tagName, Object.getPrototypeOf(i) === HTMLImageElement.prototype]];
})()
