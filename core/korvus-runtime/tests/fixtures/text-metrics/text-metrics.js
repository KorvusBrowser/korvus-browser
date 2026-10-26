// canvas measureText as Chrome 150 answers it on Linux (an interactive-widget challenge 2026-10-08): every TextMetrics member for
// generic and named families, fallback CJK/emoji, synthetic italic/bold, fractional sizes, each
// textBaseline and textAlign. Returns [[label, [width, abLeft, abRight, abAscent, abDescent, fbAscent,
// fbDescent, emHeightAscent, emHeightDescent, hanging, alphabetic, ideographic]], ...].
(function(){var R=[];
var F=['16px sans-serif','16px Arial','10px serif','13px monospace','27.77777777777778px aanotafontaa','11.904761904761905px aanotafontaa','bold 20px Arial','italic 15px Times New Roman','12px "Noto Sans CJK KR"'];
var T=['Hello, world!','😀','☀️','mmmWWWiii','gjpqy','中文','ÅÉ'];
var K=['width','actualBoundingBoxLeft','actualBoundingBoxRight','actualBoundingBoxAscent','actualBoundingBoxDescent','fontBoundingBoxAscent','fontBoundingBoxDescent','emHeightAscent','emHeightDescent','hangingBaseline','alphabeticBaseline','ideographicBaseline'];
var c=new OffscreenCanvas(10,10).getContext('2d');
F.forEach(function(f){T.forEach(function(t){c.font=f;var m=c.measureText(t);R.push([f+' | '+t, K.map(function(k){return m[k];})]);});});
['top','hanging','middle','ideographic','bottom'].forEach(function(b){c.font='16px sans-serif';c.textBaseline=b;var m=c.measureText('Hg');R.push(['base '+b,K.map(function(k){return m[k];})]);});
c.textBaseline='alphabetic';['center','right','end'].forEach(function(a){c.textAlign=a;var m=c.measureText('Hg');R.push(['align '+a,K.map(function(k){return m[k];})]);});
return R;})()
