(function(){var d=document,b=d.body,R=[];b.style.font='14px system-ui';
var H=[
 '<table id=T><tr><td>a</td><td>bb</td></tr><tr><td>ccc</td><td>d</td></tr></table>',
 '<table id=T style="border:2pt solid;margin-left:20px"><caption style="font-size:21px">caption text here</caption><tr><th>head cell</th><td>data cell</td></tr></table>',
 '<table id=T style="width:100%"><tr><td>a</td><td>longer text</td><td>x</td></tr></table>',
 '<table id=T style="width:300px"><tr><td style="width:50px">a</td><td>b</td></tr></table>',
 '<table id=T><tr><td colspan=2>spanning cell</td></tr><tr><td>a</td><td>b</td></tr></table>',
 '<table id=T><tr><td rowspan=2>tall</td><td>r1</td></tr><tr><td>r2</td></tr></table>',
 '<div id=T style="display:table"><div style="display:table-cell">c1</div><div style="display:table-cell;padding:5px">c2</div></div>',
 '<div id=T><div style="display:table"></div>after</div>',
 '<table id=T><tfoot><tr><td>foot</td></tr></tfoot><tbody><tr><td>body</td></tr></tbody><thead><tr><td>head</td></tr></thead></table>',
 '<table id=T border=1 cellpadding=4 cellspacing=0><tr><td>x</td><td>yy</td></tr></table>',
 '<table id=T style="table-layout:fixed;width:200px"><tr><td style="width:30px">a</td><td>b</td><td>c</td></tr></table>',
 '<table id=T><tr style="height:40px"><td style="vertical-align:top">t</td><td>m</td><td style="vertical-align:bottom">b</td></tr></table>',
 '<table id=T style="border-spacing:5px 10px"><tr><td>a</td></tr></table>',
 '<p id=T>text <table style="display:inline-table"><tr><td>it</td></tr></table> more</p>'];
H.forEach(function(h,i){b.innerHTML=h;var t=d.getElementById('T'),out=[i];
 [t].concat([].slice.call(t.querySelectorAll('caption,thead,tbody,tfoot,tr,td,th,div,table'))).forEach(function(e){var r=e.getBoundingClientRect();out.push([e.tagName.toLowerCase(),r.x,r.y,r.width,r.height]);});
 R.push(out);});
return R;})()
