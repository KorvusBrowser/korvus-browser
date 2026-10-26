(function () {
  /* The dictionaries Web Animations hands out, member by member in order: Blink's bindings write an IDL dictionary's
     members inherited dictionary first, each dictionary's own members in lexicographic order (EffectTiming, then
     ComputedEffectTiming's); getKeyframes() builds each keyframe by hand: offset, easing, composite, the properties,
     computedOffset. */
  var out = [];
  var keys = function (o) { return Object.keys(o).join(','); };
  var d = document.createElement('div');
  document.body.appendChild(d);
  var an = d.animate([{ opacity: 0 }, { opacity: 1 }], 1000);
  out.push(['getTiming', keys(an.effect.getTiming())]);
  out.push(['getComputedTiming', keys(an.effect.getComputedTiming())]);
  out.push(['getKeyframes[0]', keys(an.effect.getKeyframes()[0])]);
  out.push(['getKeyframes[1]', keys(an.effect.getKeyframes()[1])]);
  an.pause(); an.currentTime = 500;
  out.push(['getComputedTiming paused', JSON.stringify(an.effect.getComputedTiming())]);
  an.cancel();
  out.push(['getComputedTiming idle', JSON.stringify(an.effect.getComputedTiming())]);
  var an2 = d.animate({ opacity: [0, 1], transform: ['none', 'scale(2)'] }, { duration: 2000, delay: 100, easing: 'ease-in', iterations: 2, fill: 'both' });
  out.push(['getTiming full', JSON.stringify(an2.effect.getTiming())]);
  // (a keyframe's own properties come out in the order of Blink's HashMap of them -- `transform,opacity` for
  // `{opacity, transform}` -- which is not asked here)
  an2.cancel();
  var k = new KeyframeEffect(null, [{ offset: 0, color: 'red', composite: 'add' }, { color: 'blue', easing: 'steps(2)' }], 500);
  out.push(['KeyframeEffect getKeyframes', JSON.stringify(k.getKeyframes())]);
  out.push(['KeyframeEffect getComputedTiming', JSON.stringify(k.getComputedTiming())]);
  // a CSS animation and a CSS transition carry the same dictionaries
  var s = document.createElement('style');
  s.textContent = '@keyframes f4fade { from { opacity: 0 } to { opacity: 1 } } .f4a { animation: f4fade 3s ease 1s infinite alternate both }';
  document.head.appendChild(s);
  var c = document.createElement('div'); c.className = 'f4a'; document.body.appendChild(c);
  var ca = c.getAnimations()[0];
  out.push(['CSSAnimation getTiming', ca ? JSON.stringify(ca.effect.getTiming()) : 'none']);
  out.push(['CSSAnimation getKeyframes[0]', ca ? keys(ca.effect.getKeyframes()[0]) : 'none']);
  c.remove(); s.remove(); d.remove();
  return out;
})()
