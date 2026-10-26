// hello -- the smallest korvus plugin written in Node over the remote-plugin bridge.
//
// It observes every event korvus forwards (printed to stderr) and refuses any request hop whose URL contains a
// chosen substring. Run it, then point the `remote-bridge` plugin at it:
//
//   node hello.mjs --port 9099 --block-substr ads.
//   korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
//
// With `--port 0` it binds an ephemeral port and prints `PORT <n>` on stdout, so a parent process can learn the
// port (the SDK probe drives it this way). Events go to stderr, keeping stdout for that one line.

import { BLOCK, CONTINUE, KorvusPlugin } from "./korvus-plugin.mjs";

function arg(flag, fallback) {
  const i = process.argv.indexOf(flag);
  return i !== -1 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
}

const host = arg("--host", "127.0.0.1");
const port = Number(arg("--port", "9099"));
const blockSubstr = arg("--block-substr", "");

class Hello extends KorvusPlugin {
  onEvent(ev) {
    console.error(`event: ${ev.kind} (pid=${ev.pid} tag=${ev.tag})`);
  }
  onRequest(req) {
    if (blockSubstr && req.url.includes(blockSubstr)) {
      console.error(`block: ${req.method} ${req.url}`);
      return BLOCK;
    }
    return CONTINUE;
  }
}

const plugin = new Hello({ name: "hello-node" });
const bound = await plugin.listen(port, host);
console.log(`PORT ${bound}`); // announce the bound port, then serve until killed
