// GOAL-4 W5.3 Cache API fixture: open a cache, put a Response, match it back, and post the matched body.
(async () => {
  const cache = await caches.open("v1");
  await cache.put("/hello", new Response("cached-hello"));
  const m = await cache.match("/hello");
  const body = m ? await m.text() : "MISS";
  self.postMessage("cache:" + body);
})();
