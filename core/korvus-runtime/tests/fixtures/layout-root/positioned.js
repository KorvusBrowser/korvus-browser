(function(){var R=[];var d=document,de=d.documentElement,b=d.body;
function r(t,e){var x=e.getBoundingClientRect();R.push([t,x.left,x.top,x.width,x.height,e.offsetLeft,e.offsetTop]);}
b.innerHTML='<div style="height:100px"></div><div id=a1 style="position:absolute;bottom:0;right:0;width:50%;height:50%"></div>'+
 '<div id=a2 style="position:absolute;top:10%;left:10%;width:10px;height:10px;margin:3px"></div>'+
 '<div id=f1 style="position:fixed;bottom:0;right:0;width:50%;height:50%"></div><div id=a3 style="position:absolute"></div>';
['a1','a2','f1','a3'].forEach(function(i){r(i,d.getElementById(i));});
de.style.cssText='margin:20px;padding:3px;border:4px solid';
['a1','a2','f1','a3'].forEach(function(i){r('m-'+i,d.getElementById(i));});
de.style.cssText='position:relative;margin:20px;padding:3px;border:4px solid';
['a1','a2','f1','a3'].forEach(function(i){r('rel-'+i,d.getElementById(i));});
R.push(['vp',innerWidth,innerHeight]);
return R;})()
