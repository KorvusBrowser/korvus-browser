//! LIVE gate for the `DOMMatrix` members measured against Chrome and corrected.
//!
//! `DOMMatrix` is the ONE of that lane's five shapes that did not become a table -- see the lane's
//! report and `js/dom/08c-collections.js`'s remaining block for why (a JS `Proxy` interface object, a
//! `KB.__*` bridge behind `fromMatrix`, and a worker realm whose `DOMMatrix` is a registry shell with
//! a DIFFERENT member list from the window's). What this file pins is the part that was measurably
//! wrong and is now right, so the next lane starts from a gated position rather than a claim.
//!
//! Measured on headless Chrome 151 (`--dump-dom`):
//!
//! ```text
//! new DOMMatrix().setMatrixValue()                     -> TypeError: 1 argument required, but only 0 present.
//! typeof new DOMMatrix().setMatrixValue("translate(1px,2px)")  -> "object"  (NOT undefined)
//! m.m11 = "abc"  -> m.m11 is NaN, no throw            m.a = "2" -> 2
//! ```
//!
//! and webref `geometry.idl:260` declares `DOMMatrix setMatrixValue(DOMString transformList)` --
//! the return type is the MATRIX. The engine answered `undefined`: the Rust body returned `()`, op2
//! rendered that as null, and the ESM arity guard rewrote the null to `undefined` under an `isVoid`
//! flag. Two layers agreeing on the same mistake, and no test looked at the value.

use engine::Engine;

/// Spawn a worker that probes DOMMatrix and posts a pipe-joined summary back; pump until RESULT:.
/// Mirrors `dom_rect_worker_live.rs::worker_probe`.
async fn worker_probe(worker_body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(worker_body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

/// THE WORKER HALF, LANDED — this test was a PIN and is now the assertion it asked to become.
///
/// L21 made `DOMMatrix`/`DOMMatrixReadOnly` hand tables, which (correctly, under I1) removed them
/// from the ONE registry both realms draw from (W6.1), so the worker went from a wrong-shaped stub to
/// ABSENT. L21 pinned that here rather than hide it, with the instruction "if this now reads anything
/// else, that unit landed -- rewrite this test to the new expectation instead of deleting it".
///
/// It landed at merge, on W6.2's measured recipe: `worker-host.js` assigned the two native classes and
/// `worker.rs` verified them against the tables, parent before child (a worker-side reconcile that
/// Deleted: the classes are own classes now, built from the tables in the bootstrap's
/// class slot, `install::live::geometry`). Two further facts this now guards, both measured rather
/// than assumed:
///
///   * The worker's classes are NATIVE ones, not shells — so `[native code]`, not JS source.
///   * The two WINDOW-ONLY rows are absent here. The bundle says so: `surface.json` has
///     `DOMMatrixReadOnly` 42 / `DOMMatrix` 34 and `worker.surface.json` has 41 / 33, and the set
///     difference is exactly `toString` and `setMatrixValue`. The worker capture's member list keeps
///     the installer from mounting them. (While the deno_core class TEMPLATE realised them, a delete had
///     to take them off the prototype too -- the step L20 named as the reason this could not be done in
///     one unit.)
///
/// `WebKitCSSMatrix` stays `undefined`: L21 left its generated alias entry alone, deliberately.
#[tokio::test]
async fn dommatrix_reaches_the_worker_as_the_native_class_minus_the_window_only_rows() {
    let body = r#"
        const nat = (f) => /\[native code\]/.test(Function.prototype.toString.call(f));
        self.postMessage([
            typeof DOMMatrix, typeof DOMMatrixReadOnly, typeof WebKitCSSMatrix,
            nat(DOMMatrix) && nat(DOMMatrixReadOnly),
            // OWN, not `in`: `toString` exists on Object.prototype, so `in` walks the chain and
            // finds it there. "the row is absent" means the prototype does not OWN it.
            Object.prototype.hasOwnProperty.call(DOMMatrix.prototype, "setMatrixValue"),
            Object.prototype.hasOwnProperty.call(DOMMatrixReadOnly.prototype, "toString"),
            new DOMMatrix().a,
        ].join("|"));
    "#;
    let got = worker_probe(body).await;
    assert_eq!(
        got, "function|function|undefined|true|false|false|1",
        "the worker must have the NATIVE DOMMatrix pair, without the two rows only the window capture \
         records, and a constructed matrix must answer its own `a`"
    );
}

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

#[tokio::test]
async fn set_matrix_value_returns_the_matrix_and_still_guards_its_required_argument() {
    let mut e = page().await;
    let src = r#"(() => {
        // `.name`, not `constructor.name`: an unparsable transform list is a DOMException whose name
        // is "SyntaxError" -- which is exactly what the bundle's oracle records for this expression.
        const T = (f) => { try { return String(f()); } catch (e) { return e.name; } };
        const m = new DOMMatrix();
        const r = m.setMatrixValue("translate(1px, 2px)");
        return [
            T(() => new DOMMatrix().setMatrixValue()),          // TypeError -- the WebIDL arity guard
            T(() => new DOMMatrix().setMatrixValue("x")),       // SyntaxError -- an unparsable list
            typeof r,                                            // object, not undefined
            r === m,                                             // the receiver itself
            Object.prototype.toString.call(r),                   // [object DOMMatrix]
            m.e, m.f,                                            // 1, 2 -- the transform was applied
            DOMMatrix.prototype.setMatrixValue.length,           // 1 (WebIDL)
            DOMMatrix.prototype.setMatrixValue.toString().includes("[native code]")
        ].join("|");
    })()"#;
    let got = e.eval_value("dm-smv", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "TypeError|SyntaxError|object|true|[object DOMMatrix]|1|2|1|true",
        "setMatrixValue must return the matrix (webref: `DOMMatrix setMatrixValue(DOMString)`; Chrome 151 measured)",
    );
}

/// The `unrestricted double` coercion on the 22 writable matrix attributes. Was installed by
/// `js/dom/08c-collections.js` (now deleted); `installer::finish_native_member` supplies it from the
/// row's own `attr … : Double` + writable column now -- pinned here so the table
/// cutover kept it rather than rediscovering it.
#[tokio::test]
async fn every_writable_matrix_attribute_runs_webidl_tonumber() {
    let mut e = page().await;
    let src = r#"(() => {
        const names = ["a","b","c","d","e","f","m11","m12","m13","m14","m21","m22","m23","m24",
                       "m31","m32","m33","m34","m41","m42","m43","m44"];
        let threw = 0, notNaN = 0, notTwo = 0;
        for (const n of names) {
            const m = new DOMMatrix();
            try { m[n] = "abc"; } catch (e) { threw++; continue; }
            if (!Number.isNaN(m[n])) notNaN++;
            const q = new DOMMatrix();
            try { q[n] = "2"; } catch (e) { threw++; continue; }
            if (q[n] !== 2) notTwo++;
        }
        return [names.length, threw, notNaN, notTwo].join("|");
    })()"#;
    let got = e.eval_value("dm-num", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "22|0|0|0", "a writable `unrestricted double` attribute must ToNumber, never throw");
}

/// The JS `Proxy` interface object (`dom/00-bootstrap.js::__matrixCtor`) is gone.
/// `DOMMatrix`/`DOMMatrixReadOnly` are now `interface!` tables verified in place
/// (`install::live::reconcile_dom_matrix_family`), so the constructor CHAIN a `cppgc` row splices is
/// reachable: `Object.getPrototypeOf(DOMMatrix) === DOMMatrixReadOnly` was `false` (measured by L20,
/// Chrome says `true`) because the Proxy's own `[[GetPrototypeOf]]` deferred to its TARGET, whose
/// ctor chain nothing had spliced.
#[tokio::test]
async fn dommatrix_ctor_chain_matches_chrome() {
    let mut e = page().await;
    let src = r#"(() => {
        return [
            Object.getPrototypeOf(DOMMatrix) === DOMMatrixReadOnly,
            DOMMatrix.prototype.__proto__ === DOMMatrixReadOnly.prototype,
            DOMMatrix.length, DOMMatrixReadOnly.length,
            (new DOMMatrix()).constructor === DOMMatrix,
        ].join("|");
    })()"#;
    let got = e.eval_value("dm-chain", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "true|true|0|0|true", "DOMMatrix's ctor chain and .length must match Chrome 151 (webref: both constructors take one optional arg)");
}

/// The constructor's WebIDL union (`optional (DOMString or sequence<unrestricted double>) init`),
/// resolved natively now (`core/geometry/dom_matrix.rs::resolve_ctor_arg`) rather than by the deleted
/// Proxy. Same four cases the Proxy's doc comment named: a CSS string, a 6-array, a live matrix
/// (not iterable -> stringified -> reparsed, exactly why `new DOMMatrix(new DOMMatrix())` is 2D in
/// Chrome), and a non-iterable, non-string primitive (also stringified, and `"5"` is not a valid
/// transform list -> SyntaxError).
#[tokio::test]
async fn dommatrix_constructor_union_resolution_matches_chrome() {
    let mut e = page().await;
    let src = r#"(() => {
        const str = new DOMMatrix("translate(1px,2px)");
        const arr = new DOMMatrix([1,0,0,1,3,4]);
        const fromLive = new DOMMatrix(new DOMMatrix().translate(3, 4));
        let numName = "";
        try { new DOMMatrix(5); } catch (e) { numName = e.name; }
        return [
            str.e, str.f,
            arr.e, arr.f,
            fromLive.is2D, fromLive.e, fromLive.f,
            numName,
        ].join("|");
    })()"#;
    let got = e.eval_value("dm-ctor-union", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "1|2|3|4|true|3|4|SyntaxError",
        "constructor union resolution: string/array/live-matrix(stringified)/non-iterable-primitive(stringified) must all match Chrome");
}

/// `fromMatrix` on a LIVE matrix: measured `DOMMatrix.fromMatrix(new
/// DOMMatrix().translate(3, 4))` answering the IDENTITY, because the old `Option<serde_json::Value>`
/// row read OWN properties only and a live matrix's a/b/c…/m44 are prototype getters. The row is
/// `Option<MatrixLike>` now: serde_v8's named-struct path reads each field with
/// `v8::Object::get`, which walks the prototype chain, so this must no longer be the identity. The
/// a/m11 "validate and fixup" conflict check (webref, `DOMMatrixInit`) is pinned in the same test:
/// no live matrix ever disagrees with itself, so a synthetic dict exercises it.
#[tokio::test]
async fn dommatrix_from_matrix_reads_a_live_matrix_through_its_getters() {
    let mut e = page().await;
    let src = r#"(() => {
        const live = new DOMMatrix().translate(3, 4);
        const fromLive = DOMMatrix.fromMatrix(live);
        const fromDict = DOMMatrix.fromMatrix({a: 2});
        let conflictName = "";
        try { DOMMatrix.fromMatrix({a: 1, m11: 2}); } catch (e) { conflictName = e.name; }
        return [
            fromLive.e, fromLive.f, fromLive.is2D,
            fromDict.a, fromDict.m11,
            conflictName,
        ].join("|");
    })()"#;
    let got = e.eval_value("dm-from-matrix-live", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "3|4|true|2|2|TypeError",
        "DOMMatrix.fromMatrix(liveMatrix) must read through its getters, not answer the identity");
}
