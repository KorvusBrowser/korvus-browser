(() => {
  const out = [];
  const sys = ['activeborder','activecaption','activetext','appworkspace','background','buttonborder','buttonface','buttonhighlight','buttonshadow','buttontext','canvas','canvastext','captiontext','field','fieldtext','graytext','highlight','highlighttext','inactiveborder','inactivecaption','inactivecaptiontext','infobackground','infotext','linktext','mark','marktext','menu','menutext','selecteditem','selecteditemtext','scrollbar','threeddarkshadow','threedface','threedhighlight','threedlightshadow','threedshadow','visitedtext','window','windowframe','windowtext','accentcolor','accentcolortext','text','-webkit-link','-webkit-activelink','-webkit-focus-ring-color'];
  for (const s of sys) {
    const d = document.createElement('div'); document.body.appendChild(d);
    d.style.color = s;
    const c = document.createElement('canvas').getContext('2d'); document.body.appendChild(c.canvas); c.fillStyle = '#123456'; c.fillStyle = s;
    out.push([s, CSS.supports('color', s), d.style.color, getComputedStyle(d).color, c.fillStyle]);
    d.remove(); c.canvas.remove();
  }
  return out;
})()
