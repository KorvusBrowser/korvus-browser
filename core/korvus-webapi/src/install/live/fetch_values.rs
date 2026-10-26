//! The two Fetch VALUE interfaces, `Headers` and `FormData` -- cppgc classes, own classes.
//!
//! WHAT THIS FIXES, and it is not a shape. Both are `[Exposed=(Window,Worker)]` and
//! both have a real `#[op2] impl` class -- but only the WINDOW ever got it. `js/dom/00-bootstrap.js`
//! assigns the class template onto the page global; nothing assigned it in a worker, so the worker's
//! `Headers` was whatever the generated worker registry built: a shell whose methods answer a
//! shape-correct empty value. Measured at `23428558`, inside a worker:
//!
//! ```text
//! new Headers(); h.append("a", "1"); h.get("a")   -> null        (the window answers "1")
//! [...new Headers([["a", "1"]])]                  -> TypeError: not iterable
//! Headers.prototype[Symbol.iterator] === entries  -> false       (the window: true)
//! Object.getOwnPropertyNames(Headers.prototype)[0] -> "constructor"  (Chrome puts it LAST)
//! ```
//!
//! One name, two realms, three different answers and no test. The table is the same table in both
//! realms, so the install has to be too: W5.1 L20 put the class template on whichever global lacked
//! it (`ensure_class`, reading it off `Deno.core.ops`) and verified the rows against it identically on
//! both sides. That read went with the op2 classes: [`install_fetch_values`] builds
//! both interfaces from their tables, in either realm, over the classes' generated constructors.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Drift};
use crate::modules::fetch::form_data::{FormData, FORMDATA};
use crate::modules::fetch::headers::{Headers, HEADERS};

/// `Headers` and `FormData` from their tables, in whichever realm is calling -- OWN classes:
/// each interface is built like any table interface, over its class's generated constructor, and its
/// `cppgc` rows are the class's generated members. (Until then the deno_core class template went onto the
/// global here -- `ensure_class`, off `Deno.core.ops` -- and `installer::verify_native`, deleted then, checked and finished
/// it: the tag, the lengths, the arity guards, `constructor` moved last.) Returns no drift: there is no
/// realised surface left to verify, the table IS the surface.
pub fn install_fetch_values<'s>(scope: &mut v8::PinScope<'s, '_>) -> Vec<Drift> {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    build_newable(scope, global, &HEADERS, Headers::__kc_ctor, None, None, &mut ledger);
    build_newable(scope, global, &FORMDATA, FormData::__kc_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
    Vec::new()
}
