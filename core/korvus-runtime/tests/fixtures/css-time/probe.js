(() => {
  const rows = [];
  const cases = [
    ['transition-duration', '80ms'], ['transition-duration', '0.25s, 1s'], ['transition-duration', '250ms, 1500ms, 0ms'],
    ['transition-duration', '0.1234567s'], ['transition-duration', '1e7ms'], ['transition-duration', '0.0000001s'],
    ['transition-duration', '2.5e-3s'], ['transition-duration', 'calc(100ms + 0.2s)'], ['transition-duration', '1000000s'],
    ['transition-delay', '-500ms'], ['transition-delay', '1500ms, -0.25s'], ['transition-delay', '0.0000001s'], ['transition-delay', '1.23456789s'],
    ['animation-duration', '250ms'], ['animation-duration', '3s, 40ms'], ['animation-delay', '-1200ms'], ['animation-delay', '33ms'],
  ];
  for (const [prop, value] of cases) {
    const el = document.createElement('div');
    el.style.setProperty(prop, value);
    document.body.append(el);
    rows.push([prop + ': ' + value, getComputedStyle(el).getPropertyValue(prop), el.style.getPropertyValue(prop)]);
    el.remove();
  }
  const t = document.createElement('div');
  t.style.transition = 'color 80ms ease-in 1200ms, opacity 2s';
  t.style.animation = 'kf 250ms linear -100ms 2';
  document.body.append(t);
  const cs = getComputedStyle(t);
  rows.push(['transition shorthand', cs.transition, t.style.transition]);
  rows.push(['animation shorthand', cs.animation, t.style.animation]);
  rows.push(['animation-delay', cs.animationDelay]);
  return rows;
})()
