(function(){var d=document,b=d.body,R=[];
var T=['skewY(23.1753218deg)','scale(2,1.89)','matrix(1.11,2.0001,-1.0001,1.009,150,94.4)','perspective(12890px) translateZ(101.5px)','rotate(30deg)','translate(10px,5%)','scale(100000000000000000000009999999999999.99,1.89)','rotateX(40deg)','perspective(200px) rotateY(30deg)','skewX(-12.5deg) scale(0.7)','rotate(1turn) translateX(3.3px)','scale(-1,1)','none'];
T.forEach(function(t,i){
 b.insertAdjacentHTML('beforeend','<div id=t'+i+' style="width:100px;height:40px;margin:5px;border:2px solid;transform:'+t+'"><span id=s'+i+'>xy</span></div>');
 var e=d.getElementById('t'+i),r=e.getBoundingClientRect(),row=[t,[r.x,r.y,r.width,r.height]];
 // a descendant through a 2D transform (a 3D one's projected descendant is a float ulp off, not recorded)
 if (!/perspective|rotateX|rotateY/.test(t)) { var s=d.getElementById('s'+i).getBoundingClientRect(); row.push([s.x,s.y,s.width,s.height]); }
 R.push(row);});
b.insertAdjacentHTML('beforeend','<div id=o style="transform:rotate(10deg);transform-origin:0 0;margin:20px"><div id=i style="transform:scale(.5);width:50px;height:50px">q</div></div>');
var r=d.getElementById('i').getBoundingClientRect();R.push(['nested',[r.x,r.y,r.width,r.height]]);
var cr=d.getElementById('t4').getClientRects()[0];R.push(['clientRects',[cr.x,cr.y,cr.width,cr.height]]);
b.insertAdjacentHTML('beforeend','<span id=inl style="transform:scale(3)">inline</span>');var ir=d.getElementById('inl').getBoundingClientRect();R.push(['inline ignores transform',[ir.x,ir.y,ir.width,ir.height]]);
return R;})()
