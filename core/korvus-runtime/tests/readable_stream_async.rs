//! ReadableStream: a reader that calls `read()` BEFORE data is enqueued must still receive it.
//!
//! The engine's original stream model snapshotted the queue at `getReader()` time and had `read()`
//! report an immediate `{done:true}` once that snapshot was exhausted. That "worked" only for a
//! producer that enqueued everything synchronously inside `start(controller)`. An ASYNC producer --
//! data enqueued AFTER `read()` began -- delivered nothing: the reader saw done, the later
//! `enqueue()`s went to a queue no reader was watching. A Next.js RSC flight stream (fed from inline
//! `self.__next_f` pushes) is exactly that shape, so React's flight client read the empty stream to
//! done and threw `Error: Connection closed.` -- observed on overreacted.io and a production site.
//!
//! The fix: a reader reads its stream LIVE, and a `read()` on an empty, still-open stream PARKS a
//! promise resolver that the next `enqueue()` (fulfil directly) or `close()` (settle as done) resolves.
//! This gate pins both halves -- the async-producer path AND the synchronous-buffered path that always
//! worked -- so neither can regress the other.
use engine::Engine;

#[tokio::test]
async fn read_before_enqueue_delivers_the_async_chunks() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("rs", r#"
        // ASYNC PRODUCER -- the "Connection closed." repro: read() is issued while the stream is empty
        // and open; the chunks arrive only afterwards, one onto a parked read and one onto the queue.
        (async () => {
            let ctrl;
            const s = new ReadableStream({ start(c) { ctrl = c; } });
            const rd = s.getReader();
            const p1 = rd.read();          // parks: empty + open
            ctrl.enqueue("A");             // fulfils the parked read directly
            ctrl.enqueue("B");             // queued: no read waiting yet (rd's next read is a microtask)
            ctrl.close();                  // stream done
            const a = await p1;
            const b = await rd.read();     // drains the queued B
            const c = await rd.read();     // empty + closed -> done
            print("async=" + a.value + "," + a.done + "|" + b.value + "," + b.done + "|" + c.value + "," + c.done);
        })();
        // SYNC BUFFERED -- the path that always worked: everything enqueued + closed inside start(),
        // read afterwards. Must still hold after the rewrite.
        (async () => {
            const s = new ReadableStream({ start(c) { c.enqueue("X"); c.enqueue("Y"); c.close(); } });
            const rd = s.getReader();
            const a = await rd.read();
            const b = await rd.read();
            const d = await rd.read();
            print("sync=" + a.value + "," + a.done + "|" + b.value + "," + b.done + "|" + d.value + "," + d.done);
        })();
    "#.into()).unwrap();
    e.run().await.unwrap();
    // The two async IIFEs settle independently, so their print order is not fixed -- sort to compare.
    let mut out = e.take_output();
    out.sort();
    assert_eq!(out, vec![
        "async=A,false|B,false|undefined,true",
        "sync=X,false|Y,false|undefined,true",
    ]);
}
