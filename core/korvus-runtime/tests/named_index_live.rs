//! The `window` and `document` named lookups read their candidates from the arena's `name`
//! and `id` indexes (`korvus_dom::Dom::elements_named`, `elements_with_id`) instead of scanning it.
//!
//! An index is only as good as its invalidation, so every case here CHANGES a lookup's candidates --
//! a rename through `setAttribute` and through the reflected `name`, a removal, an insertion, a move
//! that puts two same-named forms out of NodeId order -- and a stale entry would show as a wrong
//! answer. Each expected line is stock Chrome 150's, measured 2026-09-29 with
//! `tools/realweb/seed/chrome_eval.py` on this script.
//!
//! Lines 8w, 9, 10, 11: the Window's named objects are the DOCUMENT tree's -- a detached
//! or removed form is not one -- and several are an HTMLCollection in tree order, as `document`'s are.
//! Measured on stock Chrome 150 the same day (`tools/realweb/seed/chrome_eval.py`).
use engine::Engine;

const HTML: &str = r#"<!doctype html><html><body></body></html>"#;

#[tokio::test]
async fn named_lookups_follow_renames_removals_and_moves() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(HTML).unwrap();
    e.eval("t", r#"
        document.body.insertAdjacentHTML('beforeend', '<form name="a"></form><img name="i" id="j"><object id="o"></object><embed name="e">');
        const f = document.querySelector('form'), img = document.querySelector('img');
        const obj = document.querySelector('object'), emb = document.querySelector('embed');
        print('1 ' + [window.a === f, document.a === f, window.i === img, document.i === img, document.j === img,
                      document.o === obj, window.o === obj, window.e === emb, document.e === emb].join());
        f.setAttribute('name', 'b');
        print('2 ' + [typeof window.a, window.b === f, typeof document.a, document.b === f].join());
        f.removeAttribute('name');
        print('3 ' + [typeof window.b, typeof document.b].join());
        img.removeAttribute('name');
        print('4 ' + [typeof document.j, window.j === img, typeof document.i, typeof window.i].join());
        const f2 = document.createElement('form');
        f2.name = 'z';
        print('5 ' + typeof document.z);
        document.body.appendChild(f2);
        print('6 ' + [document.z === f2, window.z === f2].join());
        f2.name = 'y';
        print('7 ' + [typeof document.z, typeof window.z, document.y === f2, window.y === f2].join());
        document.body.insertAdjacentHTML('beforeend', '<form name="m" id="m1"></form><form name="m" id="m2"></form>');
        const m1 = document.getElementById('m1'), m2 = document.getElementById('m2');
        document.body.insertBefore(m2, m1);
        const dm = document.m;
        print('8 ' + [Object.prototype.toString.call(dm), dm.length, dm[0] === m2, dm[1] === m1].join());
        const wm = window.m;
        print('8w ' + [Object.prototype.toString.call(wm), wm.length, wm[0] === m2, wm[1] === m1].join());
        f2.remove();
        print('9 ' + [typeof document.y, typeof window.y].join());
        const f3 = document.createElement('form');
        f3.name = 'd';
        print('10 ' + [typeof window.d, 'd' in window].join());
        document.body.insertAdjacentHTML('beforeend', '<div id="dup"></div><p id="dup"></p>');
        print('11 ' + [Object.prototype.toString.call(window.dup), window.dup.length, window.dup[0].tagName].join());
        print('miss ' + [typeof window.nothingNamedThis, typeof document.nothingNamedThis, typeof nothingNamedThis].join());
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "1 true,true,true,true,true,true,true,true,true",
        "2 undefined,true,undefined,true",
        "3 undefined,undefined",
        "4 undefined,true,undefined,undefined",
        "5 undefined",
        "6 true,true",
        "7 undefined,undefined,true,true",
        "8 [object HTMLCollection],2,true,true",
        "8w [object HTMLCollection],2,true,true",
        "9 undefined,undefined",
        "10 undefined,false",
        "11 [object HTMLCollection],2,DIV",
        "miss undefined,undefined,undefined",
    ]);
}
