(async () => {
  const r = [];
  window.__ran = 0;
  r.push(['string-timer-returns', setTimeout('window.__ran = 1', 0)]);
  r.push(['string-interval-returns', setInterval('window.__ran = 2', 0)]);
  r.push(['empty-string-returns', setTimeout('', 0)]);
  r.push(['function-timer-returns-an-id', setTimeout(function () {}, 0) > 0]);
  await new Promise((res) => setTimeout(res, 50));
  r.push(['ran', window.__ran]);
  return r;
})()
