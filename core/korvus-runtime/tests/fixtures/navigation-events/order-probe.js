(async () => {
  // What runs between a traversal's navigate event and its popstate: tasks queued right after history.back() -- a
  // posted message, a 0 ms timer -- and the microtasks of the traversal's own commit.
  const out = [];
  const log = [];
  const mark = (s) => log.push(s);
  let hook = null;
  navigation.addEventListener('navigate', () => { mark('navigate'); if (hook) hook(); });
  navigation.addEventListener('currententrychange', () => mark('cec'));
  navigation.addEventListener('navigatesuccess', () => mark('success'));
  addEventListener('popstate', () => { mark('popstate'); queueMicrotask(() => mark('popstate-microtask')); });
  addEventListener('hashchange', () => mark('hashchange'));
  const settle = () => new Promise((r) => setTimeout(r, 80));
  location.hash = 'a'; await settle(); location.hash = 'b'; await settle();
  const step = async (label, f, h) => { log.length = 0; hook = h || null; f(); await settle(); hook = null; out.push([label, log.slice(), location.hash]); };
  await step('back + message', () => { history.back(); const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); });
  await step('forward + timer', () => { history.forward(); setTimeout(() => mark('timer'), 0); });
  await step('message then back', () => { const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); history.back(); });
  await step('forward + message + timer', () => { history.forward(); const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); setTimeout(() => mark('timer'), 0); });
  await step('back, the navigate listener posts', () => { history.back(); }, () => { const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); setTimeout(() => mark('timer'), 0); });
  await step('navigation.forward + message', () => { navigation.forward().finished.then(() => mark('finished')); const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); });
  // deep in a timer chain (the clamp holds a 0 ms timer back 4 ms): a fragment navigation's hashchange is a DOM
  // manipulation task, not a timer -- it runs before a message posted after it
  await step('nested: hash + message', () => { location.hash = 'c'; const ch = new MessageChannel(); ch.port1.onmessage = () => mark('message'); ch.port2.postMessage(0); setTimeout(() => mark('timer'), 0); });
  // two traversals by a delta: the second, asked from a timer that runs before the first arrives, goes back from where
  // the first arrived (HTML's session history traversal queue resolves a delta when the step runs)
  // from a message task (nesting level 0): the 0 ms timer is not clamped, so it runs before the first traversal arrives
  await step('back, and back again from a 0 ms timer', () => { const ch = new MessageChannel(); ch.port1.onmessage = () => { history.back(); setTimeout(() => { mark('timer'); history.back(); }, 0); }; ch.port2.postMessage(0); });
  return out;
})()
