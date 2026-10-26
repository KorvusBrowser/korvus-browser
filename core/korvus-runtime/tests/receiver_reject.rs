//! A PROMISE-RETURNING member REJECTS a foreign receiver; it does not throw (unit: TY2).
//!
//! `core/korvus-runtime/tests/receiver_sweep.rs` counts this over every declared row and is the
//! number the rollout is measured against. It cannot, on its own, say WHICH MOUNT a row came through
//! -- and the two mounts disagreed. `installer::put_member` wraps a row with a BODY in
//! `install::receiver`'s guard, which has read the rule since it was written; a `= stub` row is
//! mounted instead through `install::stub`, which "carries its OWN receiver check", and that check
//! threw for everything. So the rule was true of one mount and false of the other, and the count
//! could not tell you because the ledger recorded no policy at all for a stub row.
//!
//! MEASURED, Google Chrome 150.0.7871.46 `--headless=new --dump-dom` over a loopback server,
//! 2026-09-23, by reaching every own member of all 954 window interface prototypes through the
//! receiver `{}` and AWAITING every promise that came back:
//!
//! ```text
//!   364 (interface, member) pairs answer a REJECTED promise.  0 of them RESOLVED.
//!   crypto.subtle.digest.call({}, ...)   REJECT TypeError: Failed to execute 'digest' on
//!                                          'SubtleCrypto': Illegal invocation
//!   ClipboardItem.prototype.getType.call({}, "x")             REJECT (same shape)
//!   Clipboard.prototype.readText.call({})                     REJECT (same shape)
//!   Node.prototype.appendChild.call({}, null)  THROW  <- the control: this probe CAN see a
//!                                                        synchronous throw, so the rest means something
//! ```
//!
//! The four cases below are one per MOUNT, which is the axis the sweep's total hides:
//! a hand table's Rust body, a hand table's `= stub` row, a generated-registry `Stub` row, and a
//! non-promise control that must still throw. Drop the control and a refusal that rejected
//! EVERYTHING would pass this file.
use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    e.eval_value("receiver-reject-probe", src)
        .unwrap_or_else(|err| panic!("eval failed: {err}"))
        .as_str()
        .expect("probe returns a string")
        .to_string()
}

#[tokio::test]
async fn a_promise_returning_member_rejects_a_foreign_receiver_and_a_plain_one_throws() {
    let mut e = page().await;
    // Phase 1: call each member on `{}`, register the handlers, record the SYNCHRONOUS outcome now.
    // A member is called with its own `length` of `undefined` arguments so the ARGUMENT-COUNT guard
    // cannot answer first and hide the receiver question -- Chrome checks the receiver first.
    let src = r#"(() => {
        globalThis.__recv = [];
        const push = (s) => globalThis.__recv.push(s);
        const one = (label, host, member) => {
            const d = Object.getOwnPropertyDescriptor(host, member);
            if (!d) { push(label + "=absent"); return; }
            const f = d.get || d.value;
            let r;
            try { r = d.get ? d.get.call({}) : d.value.apply({}, new Array(d.value.length).fill(undefined)); }
            catch (err) { push(label + "=throw:" + err.constructor.name + ":" + err.message); return; }
            if (r && typeof r.then === "function") {
                r.then(v  => push(label + "=resolve:" + Object.prototype.toString.call(v)),
                       err => push(label + "=reject:" + err.constructor.name + ":" + err.message));
            } else {
                push(label + "=value:" + Object.prototype.toString.call(r));
            }
        };
        // 1. A HAND TABLE with a Rust body, wrapped by `install::receiver` -- the mount that has
        //    always read the rule.
        one("blobText", Blob.prototype, "text");
        // 2. A HAND TABLE `= stub` row, mounted by `installer::put_d2_stub` -> `install::stub::cb_stub`.
        //    `ClipboardItem::getType` is `Any` -> `Promise` (TY2) AND a stub, so it needs BOTH halves
        //    of this unit; before either it threw.
        one("clipboardItemGetType", ClipboardItem.prototype, "getType");
        // 3. A GENERATED-REGISTRY `Stub` row (`put_shell_stub`), which is 176 of the 190 rows the
        //    stub's refusal was wrong for.
        one("clipboardReadText", Clipboard.prototype, "readText");
        // 4. THE CONTROL: a non-promise member must still THROW, synchronously.
        one("appendChild", Node.prototype, "appendChild");
        return "probed";
    })()"#;
    assert_eq!(probe(&mut e, src), "ok:probed");
    // Phase 2: the microtask checkpoint, so every rejection above has been observed. A `.then` that
    // was never pumped is not a rejection check -- three artefacts in this project made that error.
    // Every rejection names its member, in Chrome's words, whichever mount refuses (the
    // fused check, then the stub row -- measured on `readText`).
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    let got = probe(&mut e, "globalThis.__recv.slice().sort().join('|')");
    assert_eq!(
        got,
        "ok:appendChild=throw:TypeError:Illegal invocation\
         |blobText=reject:TypeError:Failed to execute 'text' on 'Blob': Illegal invocation\
         |clipboardItemGetType=reject:TypeError:Failed to execute 'getType' on 'ClipboardItem': Illegal invocation\
         |clipboardReadText=reject:TypeError:Failed to execute 'readText' on 'Clipboard': Illegal invocation",
        "a promise row must REJECT the receiver TypeError through EVERY mount, and a plain row must throw it"
    );
}

/// The ledger the sweep joins on must know a `stub` row has a policy at all.
///
/// `installer::put_member` recorded nothing for a stub row, so `receiver_sweep.rs` had no policy to
/// join and classified every stub row that answered as "a second writer overwrote this member after
/// the installer ran". That was invisible while every stub row THREW -- a throw is not an answer --
/// and the moment they correctly answered a rejected promise, 194 rows landed in that bucket and the
/// grader would have reported the fix as 194 overwrites. This asserts the ledger covers the stub
/// path, which is what makes the sweep's `rejected` counter able to see these rows at all.
#[tokio::test]
async fn the_receiver_ledger_records_a_policy_for_stub_rows_too() {
    let e = page().await;
    drop(e);
    // LAYERS FIRST, THEN RECEIVERS. Both ledgers are process-wide and other tests install pages while
    // this one reads. Every writer records a row's receiver policy BEFORE its layer, so a layer this
    // read sees was written after its receiver, and a receivers read taken AFTER it sees that receiver
    // too. Read the other way round, a concurrent install can land both writes between the two reads.
    // That is the layer without a receiver this test reported in 2 of 3 runs of a 51-test subset
    // (`SpeechSynthesis::preload`); the whole suite hid it because some earlier
    // test had always recorded the row first.
    let layers: serde_json::Value = serde_json::from_str(&webapi::behavior_rt::layers_json()).unwrap();
    let layers = layers.as_object().expect("layer ledger is an object");
    let recv: serde_json::Value = serde_json::from_str(&webapi::behavior_rt::receiver_json()).unwrap();
    let recv = recv.as_object().expect("receiver ledger is an object");

    let mut stub_rows = 0usize;
    let mut missing: Vec<&String> = Vec::new();
    for (label, layer) in layers {
        if layer.as_str() != Some("stub") {
            continue;
        }
        stub_rows += 1;
        if !recv.contains_key(label) {
            missing.push(label);
        }
    }
    assert!(stub_rows > 500, "only {stub_rows} stub rows mounted -- the layer ledger is not being read");
    assert!(
        missing.is_empty(),
        "{} row(s) the installer mounted as `stub` have NO receiver policy in the ledger, so the \
         sweep cannot classify what they answer: {:?}",
        missing.len(),
        missing.iter().take(20).collect::<Vec<_>>()
    );
    // And the policy recorded is the one the stub applies: `reject` for a promise row.
    assert_eq!(
        recv.get("ClipboardItem::getType").and_then(|v| v.as_str()),
        Some("reject"),
        "a `Promise`-typed stub row's ledger entry must say `reject`, not `throw` and not nothing"
    );
}
