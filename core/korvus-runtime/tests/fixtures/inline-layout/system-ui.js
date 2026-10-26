(function(){var d=document,b=d.body,R=[];
var stack='-apple-system,system-ui,blinkmacsystemfont,"Segoe UI",roboto,oxygen,ubuntu,"Helvetica Neue",arial,sans-serif';
['-apple-system','system-ui','blinkmacsystemfont','"Segoe UI"','roboto','oxygen','ubuntu','"Helvetica Neue"','arial','sans-serif',stack].forEach(function(f,i){
 b.insertAdjacentHTML('beforeend','<div id=f'+i+' style="font-family:'+f.replace(/"/g,"'")+';font-size:14px;display:inline-block">Ssss tttt</div><br>');
 var e=d.getElementById('f'+i),r=e.getBoundingClientRect();R.push([f.slice(0,20),r.width,r.height]);});
['2.715px','2.89px','2pt','0.5px','1.5px','3.98px','0.9px'].forEach(function(w,i){b.insertAdjacentHTML('beforeend','<div id=b'+i+' style="border:'+w+' solid;padding:'+w+'">x</div>');var e=d.getElementById('b'+i),cs=getComputedStyle(e);R.push(['bw '+w,cs.borderTopWidth,cs.paddingTop,e.getBoundingClientRect().height,e.offsetHeight,e.clientTop]);});
return R;})()
