(function () {
  /* CSS math functions as Blink 150 simplifies them (css_math_expression_node.cc): each value set on an element's
     style, then read back from the declaration (the specified serialization) and from getComputedStyle. Lengths go on
     `width` of a block in a 1000px-wide box, so a percentage has a fixed basis. Not asked here: `1vw` (the window's
     own width), `1ex` (the font's x-height), a percentage inside `translateX()` (the box it transforms). */
  var box = document.createElement('div');
  box.style.cssText = 'width: 1000px; position: absolute; left: 0; top: 0; font-size: 16px;';
  document.body.appendChild(box);
  var out = [];
  function row(prop, v) {
    var a = document.createElement('div');
    a.style.display = 'block';
    a.style.setProperty(prop, v);
    var r = [prop, v, a.style.getPropertyValue(prop)];
    box.appendChild(a);
    r.push(getComputedStyle(a).getPropertyValue(prop));
    a.remove();
    out.push(r);
  }
  var W = [
    // Fingerprint's s203 and the functions around it
    'calc( 1px * ( sin( 66911823500 * ( 36781 / -0.55 * cos( -30780.497322536891 ) ) ) )', 'calc(1px * sin(0.5))', 'calc(sin(30deg) * 10px)',
    'calc(1px * cos(0))', 'calc(1px + 2px)', 'calc(2px * 3)', 'calc(1px * pi)', 'calc(1px * e)', 'calc(1px * exp(1))', 'calc(1px * sqrt(2))', 'calc(10px * tan(45deg))',
    'calc(1px * pow(2, 3))', 'calc(1px * hypot(3, 4))', 'hypot(3px, 4px)', 'calc(1px * log(10))', 'calc(1px * log(8, 2))', 'calc(1px * atan2(1, 1))', 'calc(1px * asin(1))',
    'round(10.4px)', 'round(10.4px, 1px)', 'round(up, 10.4px, 1px)', 'round(down, 10.6px, 1px)', 'round(to-zero, -10.6px, 1px)', 'round(nearest, 15px, 10px)',
    'calc(1px * abs(-3))', 'abs(-3px)', 'calc(1px * sign(-3))', 'calc(1px * sign(3))', 'mod(10px, 3px)', 'rem(10px, 3px)', 'mod(-10px, 3px)', 'rem(-10px, 3px)',
    'min(1px, 2px)', 'max(1px, 2px)', 'clamp(1px, 2px, 3px)', 'clamp(1px, 5px, 3px)', 'clamp(none, 5px, 3px)', 'clamp(4px, 2px, none)', 'clamp(none, 2px, none)', 'min(1px)',
    'calc(50% - 1px * sin(1))', 'calc(50% + 10px)', 'calc(10px + 50%)', 'calc(50% - 10px + 5px)', 'calc(1em + 1px + 1%)', 'calc(2 * (10px + 5px))', 'calc((10px + 5px) * 2)',
    'calc(100px / 4)', 'calc(100% - (2*10px))', 'calc(10px - 10px)', 'calc(0px)', 'calc(1in + 1cm)', 'calc(1in)', 'calc(1pt + 1pc + 1mm + 1q)', 'calc(10px + 1em - 2px)',
    'min(50%, 100px)', 'max(50%, 100px)', 'clamp(10px, 50%, 600px)', 'min(10em, 100px)', 'min(1em, 2em)', 'calc(min(10px, 20px) + 5px)', 'calc(min(10%, 20px) + 5px)',
    'calc(5px + min(10%, 20px))', 'max(10%, 2em) ', 'calc(1px * (1 + 2))', 'calc(1px * (1 + 2) * 3)', 'calc(3 * 1px * 2)', 'calc(1px / 3)', 'calc(1px * 2 / 4)',
    'calc(-1px)', 'calc(1px - 2px)', 'calc(-1 * 10px)', 'calc(10px * -1 + 50%)', 'calc(50% * 2)', 'calc(50% / 2)', 'calc(100% - 0px)', 'calc(0% + 10px)', 'calc(infinity * 1px)',
    'calc(-infinity * 1px)', 'calc(NaN * 1px)', 'calc(1px * 1px / 1px)', 'calc(1px * 1px)', 'calc(1px / 1px * 10px)', 'calc(10px / 0)', 'calc(1px+2px)', 'calc(1px +2px)',
    'calc(1px + (2px))', 'calc((1px))', 'calc(calc(1px + 2px) * 2)', '-webkit-calc(1px + 2px)', 'CALC(1PX + 2PX)', 'calc(1px * sin(90deg))', 'calc(1px * cos(1turn))',
    'calc(1px * sin(1rad))', 'calc(1px * sin(100grad))', 'calc(1px * acos(0.5) / 1deg)', 'calc(100px * sin(30deg) * cos(60deg))',
    'calc(10px * sign(50% - 10px))', 'calc(1px * pow(2, 0.5))', 'round(50%, 7px)', 'mod(50%, 7px)', 'abs(50% - 600px)', 'calc(min(1px, 2px) * 3)', 'calc(1px * pow(10, 30))',
    'calc(1px * 1e6)', 'calc(1px * 123456789)', 'calc(1px * 0.0000123)', 'calc(1.23456789px)', 'calc(12345.6789px)', 'calc(1px * round(2.5))', 'calc(1px * round(-2.5))',
    'calc(10px * sin(30deg) + 50%)', 'calc(1px * sqrt(4px * 1px / 1px / 1px))', 'calc(1px * sqrt(-1))'
  ];
  W.forEach(function (v) { row('width', v); });
  ['calc(0.5)', 'calc(1 / 3)', 'calc(sin(30deg))', 'min(0.5, 0.8)', 'calc(50% / 2)', 'calc(2 * 25%)', 'clamp(0, 1.5, 1)', 'calc(0.5 + 10%)', 'round(0.66, 0.1)', 'calc(pi / 4)']
    .forEach(function (v) { row('opacity', v); });
  ['calc(1.5)', 'calc(2.5)', 'calc(-1.5)', 'calc(3 / 2)', 'round(up, 1.1, 1)', 'calc(1 + 1)', 'calc(1px)', 'max(1, 3)']
    .forEach(function (v) { row('z-index', v); });
  ['calc(1s + 500ms)', 'calc(500ms)', 'calc(2s * 0.5)', 'max(1s, 2000ms)', 'calc(1s * sin(30deg))', 'calc(1s / 2)', 'calc(-1s)', 'calc(1s + 1px)']
    .forEach(function (v) { row('transition-duration', v); });
  ['calc(1 + 0.5)', 'calc(10px + 2px)', 'calc(150%)', 'calc(1em * 1.5)', 'calc(100% + 2px)', 'calc(1 + 2px)']
    .forEach(function (v) { row('line-height', v); });
  ['rotate(calc(45deg + 45deg))', 'rotate(calc(1turn / 4))', 'rotate(calc(1rad))', 'rotate(atan2(1, 1))', 'scale(calc(2 * 0.5))']
    .forEach(function (v) { row('transform', v); });
  box.remove();
  return out;
})()
