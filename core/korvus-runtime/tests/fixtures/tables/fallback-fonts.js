(function(){var d=document,b=d.body,R=[];
[['14px system-ui','확인 중...'],['14px system-ui','abc 가나다'],['16px Arial','한국어'],['14px "Noto Sans CJK KR"','한국어 text'],['14px system-ui','emoji 😀'],['14px system-ui','漢字'],['14px system-ui','plain'],['14px system-ui;line-height:20px','확인']].forEach(function(c,i){
 b.innerHTML='<div id=d style="font:'+c[0]+'"><span id=s>'+c[1]+'</span></div>';var dv=d.getElementById('d').getBoundingClientRect(),s=d.getElementById('s').getBoundingClientRect();R.push([c.join(' | '),dv.height,s.y-dv.y,s.height,s.width]);});
return R;})()
