(function(){var d=document,b=d.body,R=[];
var cases=['top:1px;right:2px;bottom:3px;left:4px','left:10px','right:10px','top:5%','bottom:7%;left:auto','','left:calc(10% + 3px);right:auto'];
cases.forEach(function(c,i){b.insertAdjacentHTML('beforeend','<div id=r'+i+' style="position:relative;'+c+'">x</div>');var cs=getComputedStyle(d.getElementById('r'+i));R.push(['rel '+c,cs.top,cs.right,cs.bottom,cs.left]);});
cases.forEach(function(c,i){b.insertAdjacentHTML('beforeend','<div id=s'+i+' style="position:sticky;'+c+'">x</div>');var cs=getComputedStyle(d.getElementById('s'+i));R.push(['sticky '+c,cs.top,cs.right,cs.bottom,cs.left]);});
return R;})()
