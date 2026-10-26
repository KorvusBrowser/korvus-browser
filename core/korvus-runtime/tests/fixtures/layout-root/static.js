(function(){var R=[];var d=document,b=d.body;
function r(t,e){var x=e.getBoundingClientRect();R.push([t,x.left,x.top,x.width,x.height,e.offsetLeft,e.offsetTop]);}
b.innerHTML='<div id=p1 style="padding:7px;border:3px solid;margin:5px"><div style="height:20px;margin-bottom:13px"></div><div id=s1 style="position:absolute;margin:4px 6px;width:5px;height:5px"></div><div id=s1b style="position:absolute;top:2px;width:5px;height:5px"></div><div id=s1c style="position:absolute;left:3px;width:5px;height:5px"></div><p>para</p></div>'+
 '<div id=p2 style="position:relative;padding:9px">text<div id=s3 style="position:absolute;width:3px;height:3px"></div></div>'+
 '<div id=p3><div id=s4 style="position:fixed;width:4px;height:4px"></div><div style="height:40px"></div><div id=s5 style="position:absolute;width:4px;height:4px;margin-top:10px"></div></div>'+
 '<div style="display:flex;padding:4px"><div style="width:30px;height:30px"></div><div id=s7 style="position:absolute;width:2px;height:2px"></div></div>';
['s1','s1b','s1c','s3','s4','s5','s7'].forEach(function(i){r(i,d.getElementById(i));});
return R;})()
