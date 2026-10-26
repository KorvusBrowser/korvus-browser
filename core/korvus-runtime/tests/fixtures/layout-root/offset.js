(function(){var R=[];var d=document,de=d.documentElement,b=d.body;
function o(t,e){var p=e.offsetParent;R.push([t,e.offsetLeft,e.offsetTop,e.offsetWidth,e.offsetHeight,p?(p.id||p.tagName):null]);}
b.innerHTML='<div id=a style="margin:10px;padding:5px;border:3px solid"><div id=b style="margin:7px">x</div></div>'+
 '<div id=rel style="position:relative;left:4px;top:6px;margin:11px;border:2px solid;padding:9px"><span id=s>t</span><div id=c style="margin:3px">y</div></div>'+
 '<div id=fx style="position:fixed;top:30px;left:40px">f</div><div id=ab style="position:absolute;top:50px;left:60px;margin:2px">a</div>';
['a','b','rel','s','c','fx','ab'].forEach(function(i){o(i,d.getElementById(i));});
o('body',b);o('html',de);
de.style.cssText='margin:20px;padding:3px;border:4px solid';b.style.cssText='margin:6px;border:1px solid;padding:2px';
['a','b','rel','c','ab'].forEach(function(i){o('m-'+i,d.getElementById(i));});
o('m-body',b);o('m-html',de);
R.push(['m-rects',de.getBoundingClientRect().top,de.getBoundingClientRect().left,de.getBoundingClientRect().width,b.getBoundingClientRect().top,b.getBoundingClientRect().left,de.scrollHeight,de.offsetHeight,de.clientTop,b.clientTop]);
b.style.position='relative';['a','ab'].forEach(function(i){o('br-'+i,d.getElementById(i));});o('br-body',b);
return R;})()
