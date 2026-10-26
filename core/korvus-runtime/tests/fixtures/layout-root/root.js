(function(){var R=[];var d=document,de=d.documentElement,b=d.body;
function g(t){R.push([t,de.scrollHeight,de.scrollWidth,de.clientHeight,de.clientWidth,b.scrollHeight,b.offsetHeight,b.offsetTop,de.offsetHeight,de.getBoundingClientRect().top,b.getBoundingClientRect().top]);}
g('default');
var x=d.createElement('div');x.style.height='3000px';b.appendChild(x);g('tall');
de.style.margin='20px';g('html-margin');
de.style.marginBottom='50px';g('html-mb');
b.style.margin='0';g('body-0');
x.style.height='10px';g('short');
x.style.width='5000px';g('wide');
return R;})()
