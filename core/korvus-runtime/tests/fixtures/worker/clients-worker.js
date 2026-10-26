// GOAL-4 W5.4 clients fixture: on activate, message the controlled client (the page) via Client.postMessage.
self.addEventListener("activate", async () => {
  const cs = await clients.matchAll();
  if (cs[0]) cs[0].postMessage("hello-from-sw");
});
