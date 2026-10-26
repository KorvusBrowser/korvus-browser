// strokeText pixels as Chrome 150 rasterizes them on Linux (an interactive-widget challenge 2026-10-08): a fallback family, a
// named one, a generic with Korean-locl punctuation, textBaseline top, textAlign center + middle.
// Returns [[label, [r,g,b,a, ...48x32]], ...].
(function(){var R=[];
function t(name,f){var c=new OffscreenCanvas(48,32),g=c.getContext('2d');f(g);R.push([name,Array.from(g.getImageData(0,0,48,32).data)]);}
t('stroke-fallback',function(g){g.font='11.904761904761905px aanotafontaa';g.strokeStyle='#000';g.strokeText('@H1',3,20);});
t('stroke-arial',function(g){g.font='16px Arial';g.strokeStyle='#123';g.strokeText('Wave',2,22);});
t('stroke-generic',function(g){g.font='14px sans-serif';g.strokeStyle='#000';g.strokeText('Hg(:)',2,20);});
t('stroke-top',function(g){g.font='12px serif';g.textBaseline='top';g.strokeText('Top',2,2);});
t('stroke-center',function(g){g.font='12px Arial';g.textAlign='center';g.textBaseline='middle';g.strokeText('mid',24,16);});
return R;})()
