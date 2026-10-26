(function(){var d=document,b=d.body,R=[];
[0.1,0.01,0.001,0.0001,0.00001,0.000123456,123456.7,1234567,12345678,0.5e-6,100000000,-0.0000775795].forEach(function(v,i){
 b.insertAdjacentHTML('beforeend','<div id=n'+i+' style="transform:scale('+v+');opacity:'+(v<1&&v>0?v:0.5)+';line-height:'+Math.abs(v)+'">x</div>');
 var cs=getComputedStyle(d.getElementById('n'+i));R.push([v,cs.transform,cs.opacity,cs.lineHeight]);});
['2','-1','50%','150%','0.00001','5e-7','0.333333333','.5','1'].forEach(function(v,i){b.insertAdjacentHTML('beforeend','<div id=o'+i+' style="opacity:'+v+';fill-opacity:'+v+';stroke-opacity:'+v+'">x</div>');var cs=getComputedStyle(d.getElementById('o'+i));R.push(['opacity '+v,cs.opacity,cs.fillOpacity,cs.strokeOpacity]);});
b.insertAdjacentHTML('beforeend','<div id=p style="transform:perspective(12890px) translateZ(101.5px)">x</div>');R.push(['matrix3d',getComputedStyle(d.getElementById('p')).transform]);
return R;})()
