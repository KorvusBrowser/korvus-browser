(function () {
  var rows = [];
  var host = document.createElement('div'); host.id = 'h'; document.body.appendChild(host);
  var inner = document.createElement('span'); inner.id = 'in'; host.appendChild(inner);
  var nm = function (x) { return x == null ? null : (x === document ? '#document' : x === window ? '#window' : x.nodeName + (x.id ? '#' + x.id : '')); };
  ['mousemove', 'mouseover', 'mouseout', 'mouseleave', 'mouseenter', 'click', 'mousedown', 'pointermove'].forEach(function (ty) {
    var C = ty.indexOf('pointer') === 0 ? PointerEvent : MouseEvent;
    var seen = null;
    var l = function (e) { seen = { to: nm(e.toElement), from: nm(e.fromElement), rel: nm(e.relatedTarget), target: nm(e.target) }; };
    document.addEventListener(ty, l, true);
    inner.dispatchEvent(new C(ty, { bubbles: true, relatedTarget: host }));
    document.removeEventListener(ty, l, true);
    rows.push([ty + ' relatedTarget=host', seen]);
    var seen2 = null; var l2 = function (e) { seen2 = { to: nm(e.toElement), from: nm(e.fromElement) }; };
    document.addEventListener(ty, l2, true);
    inner.dispatchEvent(new C(ty, { bubbles: true }));
    document.removeEventListener(ty, l2, true);
    rows.push([ty + ' no relatedTarget', seen2]);
  });
  var ev = new MouseEvent('mousemove', { bubbles: true }); inner.dispatchEvent(ev);
  rows.push(['after dispatch: toElement', nm(ev.toElement)]);
  rows.push(['never dispatched: toElement', nm(new MouseEvent('mousemove').toElement)]);
  rows.push(['never dispatched mouseout relatedTarget=host: fromElement', nm(new MouseEvent('mouseout', { relatedTarget: host }).fromElement)]);
  return rows;
})()
