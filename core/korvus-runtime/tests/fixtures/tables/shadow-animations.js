new Promise(function(res){var d=document,b=d.body,R=[];
b.innerHTML='<style>@keyframes sp{100%{transform:rotate(360deg)}}#a,#s{animation:sp 5s linear infinite}</style><div id=a>x</div><svg id=s width=30 height=30></svg><div id=h></div>';
var r=d.getElementById('h').attachShadow({mode:'open'});r.innerHTML='<style>@keyframes sp2{100%{transform:rotate(360deg)}}#c,#t{animation:sp2 5s linear infinite}</style><div id=c>y</div><svg id=t width=30 height=30></svg><div id=u style="animation:sp 5s linear infinite">z</div>';
setTimeout(function(){[d.getElementById('a'),d.getElementById('s'),r.getElementById('c'),r.getElementById('t'),r.getElementById('u')].forEach(function(e){var cs=getComputedStyle(e);R.push([e.id,cs.transform!=='none',e.getAnimations().length]);});res(R);},300);})
