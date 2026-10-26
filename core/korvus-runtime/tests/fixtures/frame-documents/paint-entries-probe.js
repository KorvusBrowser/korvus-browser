(async () => {
  // A frame's paint entries: held while the page holds its paint (its body draws nothing yet), and drawn by what
  // the frame's flat tree shows -- a shadow tree's content, a host's text a slot shows
  const R = [];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const paints = (f) => f.contentWindow.performance.getEntriesByType('paint').map((e) => e.name);
  const mk = (html) => new Promise((res) => {
    const f = document.createElement('iframe');
    f.style.cssText = 'width:300px;height:80px;border:0';
    f.srcdoc = html;
    f.onload = () => res(f);
    document.body.appendChild(f);
  });
  {
    const f = await mk('<p>hello</p>');
    await wait(250);
    R.push(['a frame drawing text while the page holds its paint', paints(f)]);
    await wait(1200);
    R.push(['the same frame once the hold has ended', paints(f)]);
    f.remove();
  }
  const frame = async (label, html) => {
    const f = await mk(html);
    await wait(400);
    R.push([label, paints(f)]);
    f.remove();
  };
  await frame('text in the body', '<p>hello</p>');
  await frame('text in a closed shadow root', '<div id=h></div><script>h.attachShadow({mode:"closed"}).innerHTML="<p>hello</p>"<\/script>');
  await frame('text in an open shadow root', '<div id=h></div><script>h.attachShadow({mode:"open"}).innerHTML="<p>hello</p>"<\/script>');
  await frame('an svg in a closed shadow root', '<div id=h></div><script>h.attachShadow({mode:"closed"}).innerHTML="<svg width=20 height=20><circle r=5 cx=10 cy=10></circle></svg>"<\/script>');
  await frame('a host\'s text no slot shows', '<div id=h>light</div><script>h.attachShadow({mode:"closed"}).innerHTML="<b></b>"<\/script>');
  await frame('a host\'s text a slot shows', '<div id=h>light</div><script>h.attachShadow({mode:"closed"}).innerHTML="<slot></slot>"<\/script>');
  await frame('an empty shadow root', '<div id=h></div><script>h.attachShadow({mode:"closed"})<\/script>');
  return R;
})()
