(function(){var d=document,b=d.body,R=[];
var css=['transform:scale(2);-webkit-transform:scale(3)','-webkit-transform:scale(3);transform:scale(2)','transform:scale(2);-moz-transform:scale(5)','-webkit-transform:rotate(10deg)','transform-origin:0 0;-webkit-transform-origin:10px 10px','box-shadow:1px 1px red;-webkit-box-shadow:2px 2px blue','-webkit-user-select:none','animation-name:x;-webkit-animation-name:y'];
css.forEach(function(c,i){b.insertAdjacentHTML('beforeend','<div id=a'+i+' style="'+c+'">x</div>');var e=d.getElementById('a'+i),cs=getComputedStyle(e);R.push([c,cs.transform,cs.transformOrigin,cs.userSelect,cs.animationName]);});
return R;})()
