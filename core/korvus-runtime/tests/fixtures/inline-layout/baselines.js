(function(){var d=document,b=d.body,R=[];b.style.font='14px system-ui';
['<button>go</button>','<button></button>','<span style="display:inline-flex;width:10px;height:10px;padding:2px;border:1px solid"></span>','<span style="display:inline-grid;width:10px;height:10px"></span>','<span style="display:inline-flex"><i>a</i></span>','<span style="display:inline-block;width:10px;height:10px;padding:2px"></span>','<button style="height:30px">x</button>','<button style="padding:5px 10px">yy<br>zz</button>'].forEach(function(h,i){
 b.innerHTML='<div id=x>a '+h+' b</div>';var x=d.getElementById('x'),r=x.getBoundingClientRect(),c=x.firstElementChild.getBoundingClientRect();R.push([h.slice(0,50),r.height,c.y-r.y,c.height]);});
return R;})()
