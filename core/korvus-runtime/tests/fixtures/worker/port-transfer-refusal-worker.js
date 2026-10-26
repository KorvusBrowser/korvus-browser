// PARITY MP1: the CROSS-ISOLATE limit of `MessagePort.postMessage`'s transfer list, stated by the
// engine rather than left to be discovered.
//
// This worker holds a port whose other end is in the page's isolate. Same-agent transfer works
// there and here; a port put in THIS port's transfer list would have to ride the cross-isolate
// frame (`[u32 channel_id][bytes]`), which has no slot for one and no allocator for a new channel
// id on the worker side. So it is refused with a DataCloneError, loudly, and the refusal is what
// this fixture reports. An ArrayBuffer in the same list is NOT refused: it rides fine, and the
// second reading proves the refusal is about ports and not about transfer lists.
self.onmessage = (e) => {
  const port = e.ports && e.ports[0];
  if (!port) { self.postMessage("NO-PORT"); return; }
  const out = [];
  const ch = new MessageChannel();
  try { port.postMessage("x", [ch.port1]); out.push("port: NO THROW"); }
  catch (err) { out.push("port: " + err.name + ": " + err.message); }
  // The refusal must not have neutered the port it refused.
  try { ch.port1.postMessage("alive"); out.push("refused port still usable: yes"); }
  catch (err) { out.push("refused port still usable: " + err.name); }
  const ab = new ArrayBuffer(8);
  try { port.postMessage(ab, [ab]); out.push("buffer: byteLength " + ab.byteLength); }
  catch (err) { out.push("buffer: " + err.name + ": " + err.message); }
  self.postMessage(out.join(" | "));
};
