(function(){var d=document,b=d.body;b.style.cssText='margin:0;font:14px sans-serif';
b.innerHTML='<div id=host style="display:flex;gap:8px;padding:10px"></div><p>a <input type=checkbox id=c2> b <input type=radio id=r2> <input id=t2> <button id=b2>go</button></p><input type=checkbox id=c3 style="display:block">';
var h=d.getElementById('host'),r=h.attachShadow({mode:'open'});r.innerHTML='<style>label{display:inline-block;width:120px;border:1px solid red}</style><label id=l>check <input type=checkbox id=c></label><span id=s>x</span>';
function q(t,e){var x=e.getBoundingClientRect();R.push([t,x.x,x.y,x.width,x.height,e.offsetWidth,e.offsetHeight]);}
var R=[];q('label',r.getElementById('l'));q('cb-in-label',r.getElementById('c'));q('span',r.getElementById('s'));['c2','r2','t2','b2','c3'].forEach(function(i){q(i,d.getElementById(i));});
var cs=getComputedStyle(d.getElementById('c2'));R.push(['cs',cs.width,cs.height,cs.margin,cs.display,cs.boxSizing]);
return R;})()
