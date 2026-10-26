//! `PermissionsPolicy` (Chrome 152's name for `FeaturePolicy`) — the interface `document.featurePolicy` hands back.

use deno_core::v8;

use crate::install::installer::{self, Flavor};

/// `FeaturePolicy` from its `interface!` table, in the WINDOW.
///
/// A non-constructible interface: `create_interface` builds an interface object whose `new` and whose
/// plain call both throw "Illegal constructor", which is what all four bundles' browsers answer.
/// Nothing else in the tree writes this prototype — the interface leaves
/// `install/generated_registry.rs` with the table, so the registry's own stub rows are gone with it.
///
/// Nothing is registered: an instance takes its realm's own prototype through `shell_proto`, which reads
/// the realm's interface and not the page-writable global.
///
/// Position: anywhere in the realm's install block. `FeaturePolicy`'s parent is `Object.prototype`,
/// nothing inherits from it, and the registry walk's last step
/// ([`installer::seat_globals_in_capture_order`]) puts the global at the bundle's own position
/// whatever order it was created in.
///
/// Window-only: the bundles' `worker.surface.json` does not list it.
pub fn install_window_feature_policy<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::feature_policy::feature_policy::PERMISSIONSPOLICY;

    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &PERMISSIONSPOLICY, PERMISSIONSPOLICY.name, None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] feature-policy install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}
