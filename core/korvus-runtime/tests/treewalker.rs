//! TreeWalker / NodeIterator / NodeFilter traversal.
use engine::Engine;
#[tokio::test]
async fn tree_walker() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li>one</li><li>two</li><li>three</li></ul></body></html>"#).unwrap();
    e.eval("a", r#"
        const ul = document.getElementById("l");
        const w = document.createTreeWalker(ul, NodeFilter.SHOW_ELEMENT);
        const tags = [];
        let n; while ((n = w.nextNode())) tags.push(n.tagName);
        print("walk=" + tags.join(","));
        const it = document.createNodeIterator(ul, NodeFilter.SHOW_TEXT);
        const texts = [];
        let t; while ((t = it.nextNode())) texts.push(t.data);
        print("text=" + texts.join(","));
        // acceptNode filter: only <li> whose text is "two"
        const filt = { acceptNode(node) { return node.textContent === "two" ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP; } };
        const w2 = document.createTreeWalker(ul, NodeFilter.SHOW_ELEMENT, filt);
        const picked = [];
        let m; while ((m = w2.nextNode())) picked.push(m.textContent);
        print("filtered=" + picked.join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "walk=LI,LI,LI",
        "text=one,two,three",
        "filtered=two",
    ]);
}

/// The live-walker semantics WPT dom/traversal grades: firstChild/lastChild/nextSibling/
/// previousSibling/parentNode, REJECT vs SKIP, a throwing filter propagating, currentNode
/// escaping the root, whatToShow gating, and the recursion guard.
#[tokio::test]
async fn tree_walker_directional_and_filter_semantics() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="root"><div id="A1"><div id="B1"><div id="C1"></div></div><div id="B2"></div><div id="B3"></div></div></div></body></html>"#).unwrap();
    e.eval("b", r#"
        const out = [];
        const el = (id) => document.getElementById(id);
        // Directional walk: firstChild descends, nextSibling crosses siblings, parentNode climbs,
        // lastChild takes the far end. (A1 is root's only child, so nextSibling from A1 is null.)
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT);
            out.push("fc=" + w.firstChild().id);
            out.push("cn=" + w.currentNode.id);
            out.push("ns=" + (w.nextSibling() === null));
            out.push("fc2=" + w.firstChild().id);
            out.push("ns2=" + w.nextSibling().id);
            out.push("ns3=" + w.nextSibling().id);
            out.push("pn=" + w.parentNode().id);
            out.push("lc-of-A1=" + w.lastChild().id);
        }
        // REJECT prunes B1's subtree (C1 unreachable), SKIP would keep it.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, {
                acceptNode(n) { return n.id === "B1" ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT; }
            });
            const seen = [];
            let n; while ((n = w.nextNode())) seen.push(n.id);
            out.push("reject=" + seen.join(","));
        }
        // SKIP descends: B1 skipped but C1 still visited.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, {
                acceptNode(n) { return n.id === "B1" ? NodeFilter.FILTER_SKIP : NodeFilter.FILTER_ACCEPT; }
            });
            const seen = [];
            let n; while ((n = w.nextNode())) seen.push(n.id);
            out.push("skip=" + seen.join(","));
        }
        // previousSibling respects the filter: from B3 with B2 skipped, lands on B1.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, {
                acceptNode(n) { return n.id === "B2" ? NodeFilter.FILTER_SKIP : NodeFilter.FILTER_ACCEPT; }
            });
            w.currentNode = el("B3");
            out.push("ps=" + w.previousSibling().id);
        }
        // A throwing filter propagates verbatim and does not move currentNode.
        {
            const err = { name: "boom" };
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, function () { throw err; });
            let threw = null;
            try { w.firstChild(); } catch (x) { threw = x; }
            out.push("rethrow=" + (threw === err) + ",cn=" + w.currentNode.id);
        }
        // Object filter: this === filter, node argument is the candidate.
        {
            let self2 = null, arg = null;
            const filter = { acceptNode(n) { self2 = this; arg = n.id; return NodeFilter.FILTER_ACCEPT; } };
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, filter);
            w.nextNode();
            out.push("this=" + (self2 === filter) + ",arg=" + arg);
        }
        // A non-callable acceptNode throws TypeError at traverse time.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT, { acceptNode: "foo" });
            let name = "none";
            try { w.firstChild(); } catch (x) { name = x.name; }
            out.push("badfilter=" + name);
        }
        // Recursion guard: a filter that re-enters the walker throws InvalidStateError.
        {
            let depth = 0;
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ALL, function () {
                if (depth === 0) { depth++; w.firstChild(); }
                return NodeFilter.FILTER_ACCEPT;
            });
            w.currentNode = el("A1");
            let name = "none";
            try { w.parentNode(); } catch (x) { name = x.name; }
            out.push("recursive=" + name);
        }
        // currentNode may leave the root: parentNode() from outside still climbs to the nearest
        // accepted ancestor (body -> html), exactly as the spec's unbounded parent walk does.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT);
            w.currentNode = document.body;
            out.push("outside-pn=" + w.parentNode().tagName + ",cn=" + w.currentNode.tagName);
        }
        // Setting currentNode to a non-Node throws TypeError.
        {
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_ELEMENT);
            let name = "none";
            try { w.currentNode = null; } catch (x) { name = x.name; }
            out.push("setnull=" + name);
        }
        // whatToShow gates before the filter: SHOW_TEXT sees nothing in an element-only tree,
        // and the raw filter is never called.
        {
            let calls = 0;
            const w = document.createTreeWalker(el("root"), NodeFilter.SHOW_TEXT, function () { calls++; return NodeFilter.FILTER_ACCEPT; });
            out.push("wts=" + (w.nextNode() === null) + ",calls=" + calls);
        }
        // A detached subtree walks by itself (nothing escapes into the document).
        {
            const detached = document.createElement("div");
            const span = document.createElement("span");
            detached.appendChild(span);
            const w = document.createTreeWalker(detached, NodeFilter.SHOW_ELEMENT);
            out.push("detached=" + w.firstChild().tagName + "," + (w.nextSibling() === null));
        }
        for (const line of out) print(line);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "fc=A1",
        "cn=A1",
        "ns=true",
        "fc2=B1",
        "ns2=B2",
        "ns3=B3",
        "pn=A1",
        "lc-of-A1=B3",
        "reject=A1,B2,B3",
        "skip=A1,C1,B2,B3",
        "ps=B1",
        "rethrow=true,cn=root",
        "this=true,arg=A1",
        "badfilter=TypeError",
        "recursive=InvalidStateError",
        "outside-pn=HTML,cn=HTML",
        "setnull=TypeError",
        "wts=true,calls=0",
        "detached=SPAN,true",
    ]);
}
