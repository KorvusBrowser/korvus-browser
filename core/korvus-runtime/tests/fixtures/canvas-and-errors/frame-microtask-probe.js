(async () => {
  let n = 0;
  await new Promise((res) => { queueMicrotask(() => { n++; queueMicrotask(() => { n++; res(); }); }); });
  await new Promise((res) => setTimeout(res, 10));
  return [['frame-microtasks-ran', n]];
})()
