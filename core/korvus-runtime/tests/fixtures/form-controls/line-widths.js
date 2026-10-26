(function(){var d=document,b=d.body,R=[];
['2.715px','2.89px','2pt','0.5px','1.5px','3.98px','0.9px','thin','medium','thick','0px','1e-3px'].forEach(function(w,i){
 b.insertAdjacentHTML('beforeend','<div id=b'+i+' style="border:'+w+' solid;padding:'+w+';margin:'+w+';outline:'+w+' solid;column-rule:'+w+' solid">x</div>');
 var e=d.getElementById('b'+i),cs=getComputedStyle(e);
 R.push([w,cs.borderTopWidth,cs.borderLeftWidth,cs.outlineWidth,cs.columnRuleWidth,cs.paddingTop,cs.marginTop,e.getBoundingClientRect().height,e.offsetHeight,e.clientTop]);});
b.insertAdjacentHTML('beforeend','<div id=p style="width:200px"><div id=q style="padding:5%;margin:3%">y</div></div>');var cs=getComputedStyle(d.getElementById('q'));R.push(['percent',cs.paddingTop,cs.marginTop]);
return R;})()
