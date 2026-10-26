//! The plugin kernel: a pure-Rust [`Context`] (service registry + lifecycle scope), the [`Plugin`]
//! contract, and a [`Loader`] that resolves inject-order and applies plugins into a scope.
//!
//! **Pure Rust by design.** `korvus-kernel` stays contract-only — no `deno_core`/`v8` (enforced by
//! `scripts/check-invariants.sh`). The engine crate (`korvus-runtime`) bridges this `Context` into
//! deno_core's `OpState`: it puts one `Context` there, and ops read services from it. So `OpState`
//! becomes a thin carrier and the `Context` is the real registry. See
//! `docs/archive/goal-plugin-core/EXECUTION.md` (D1/D4/D6/D7 and the A2 decision).

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

/// A capability a plugin registers on the [`Context`] and others consume. Marker trait: any
/// `Send + Sync + 'static` type is a service (a service carries its own interior mutability if it
/// needs mutation — it is shared as `Arc<S>`).
pub trait Service: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> Service for T {}

/// The per-plugin configuration island handed to `apply` (a TOML/JSON table in practice). Opaque
/// JSON for now — plugins deserialize what they need.
#[derive(Debug, Clone, Default)]
pub struct PluginConfig(pub serde_json::Value);

/// An error from resolving the dependency graph or applying a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// A plugin's `inject` names a required service no registered plugin `provides`.
    MissingService { plugin: String, service: String },
    /// The inject graph cannot be linearized (a cycle, or an unsatisfiable order).
    UnresolvableOrder { remaining: Vec<String> },
    /// A plugin's `apply` returned an error.
    Apply { plugin: String, detail: String },
}
impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KernelError::MissingService { plugin, service } => {
                write!(f, "plugin {plugin:?} requires service {service:?} but no plugin provides it")
            }
            KernelError::UnresolvableOrder { remaining } => {
                write!(f, "could not order plugins (cycle or unmet inject): {remaining:?}")
            }
            KernelError::Apply { plugin, detail } => write!(f, "plugin {plugin:?} failed to apply: {detail}"),
        }
    }
}
impl std::error::Error for KernelError {}
pub type Result<T> = std::result::Result<T, KernelError>;

/// A service registry **and** lifecycle scope. `fork` makes a child scope (per page/session); a
/// child's services override the parent's; `dispose` runs every effect registered under a scope in
/// reverse (revertible effects / temporal composability). Cheap to clone (an `Arc` handle to one scope).
#[derive(Clone)]
pub struct Context(Arc<Scope>);

struct Scope {
    services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    parent: Option<Context>,
    disposers: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    disposed: Mutex<bool>,
}

impl Context {
    /// The root scope (no parent).
    pub fn root() -> Context {
        Context(Arc::new(Scope {
            services: RwLock::new(HashMap::new()),
            parent: None,
            disposers: Mutex::new(Vec::new()),
            disposed: Mutex::new(false),
        }))
    }

    /// Register a service in **this** scope (replacing any prior service of the same type here).
    pub fn provide<S: Service>(&self, svc: S) {
        self.0.services.write().unwrap().insert(TypeId::of::<S>(), Arc::new(svc));
    }

    /// Get a service as `Arc<S>`, walking up the scope tree (a child's service shadows the parent's).
    pub fn get<S: Service>(&self) -> Option<Arc<S>> {
        if let Some(a) = self.0.services.read().unwrap().get(&TypeId::of::<S>()) {
            return a.clone().downcast::<S>().ok();
        }
        self.0.parent.as_ref().and_then(|p| p.get::<S>())
    }

    /// Whether a service of this type is reachable (this scope or an ancestor).
    pub fn has<S: Service>(&self) -> bool {
        self.get::<S>().is_some()
    }

    /// Fork a child scope: it inherits the parent's services (read-through) and can override them;
    /// disposing it reverts only its own effects, never the parent's.
    pub fn fork(&self) -> Context {
        Context(Arc::new(Scope {
            services: RwLock::new(HashMap::new()),
            parent: Some(self.clone()),
            disposers: Mutex::new(Vec::new()),
            disposed: Mutex::new(false),
        }))
    }

    /// Register an inverse to run when this scope is disposed (disposers run LIFO).
    pub fn on_dispose(&self, f: impl FnOnce() + Send + 'static) {
        self.0.disposers.lock().unwrap().push(Box::new(f));
    }

    /// Run a `setup` that returns its own disposer; the disposer runs on `dispose`. This pairs an
    /// effect with its inverse at the same site (the crux of revertible effects).
    pub fn effect<D: FnOnce() + Send + 'static>(&self, setup: impl FnOnce() -> D) {
        let disposer = setup();
        self.on_dispose(disposer);
    }

    /// Revert this scope: run every registered disposer in reverse order, then drop its services.
    /// Idempotent — disposing twice is a no-op.
    pub fn dispose(&self) {
        {
            let mut done = self.0.disposed.lock().unwrap();
            if *done {
                return;
            }
            *done = true;
        }
        let mut ds = std::mem::take(&mut *self.0.disposers.lock().unwrap());
        while let Some(f) = ds.pop() {
            f(); // LIFO: last effect registered is the first reverted
        }
        self.0.services.write().unwrap().clear();
    }
}

/// A plugin: applied to a [`Context`], it registers services + effects. Object-safe (dylib-ready):
/// no generics or associated types cross the trait boundary.
pub trait Plugin: Send + Sync {
    /// A stable identifier (also the applied-order label).
    fn name(&self) -> &str;
    /// The service names this plugin registers (drives the loader's dependency ordering).
    fn provides(&self) -> &[&str] {
        &[]
    }
    /// The service names this plugin requires present before it applies.
    fn inject(&self) -> &[&str] {
        &[]
    }
    /// Register services / effects on `ctx`. Everything registered is reverted when `ctx` disposes.
    fn apply(&self, ctx: &Context, config: &PluginConfig) -> Result<()>;
}

/// Registers plugins and applies them into a scope in dependency order: a plugin applies only after
/// every plugin providing its `inject` names has applied. An `inject` with no provider anywhere is a
/// startup error (D7: required-with-default — a required service must have exactly one provider).
#[derive(Default)]
pub struct Loader {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Loader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin (registration order does not constrain apply order — inject does).
    pub fn register(&mut self, p: impl Plugin + 'static) -> &mut Self {
        self.plugins.push(Box::new(p));
        self
    }

    /// Resolve the inject graph and apply every plugin into `ctx`. Returns the applied order (names).
    pub fn apply(&self, ctx: &Context, config: &PluginConfig) -> Result<Vec<String>> {
        // Every required service must be provided by *some* registered plugin.
        let all_provided: HashSet<String> =
            self.plugins.iter().flat_map(|p| p.provides().iter().map(|s| s.to_string())).collect();
        for p in &self.plugins {
            for need in p.inject() {
                if !all_provided.contains(*need) {
                    return Err(KernelError::MissingService {
                        plugin: p.name().to_string(),
                        service: need.to_string(),
                    });
                }
            }
        }
        // Fixpoint: apply any plugin whose injects are all satisfied by what has applied so far.
        let mut applied: Vec<String> = Vec::new();
        let mut done: HashSet<usize> = HashSet::new();
        let mut satisfied: HashSet<String> = HashSet::new();
        while done.len() < self.plugins.len() {
            let mut progressed = false;
            for (i, p) in self.plugins.iter().enumerate() {
                if done.contains(&i) || !p.inject().iter().all(|n| satisfied.contains(*n)) {
                    continue;
                }
                p.apply(ctx, config)
                    .map_err(|e| KernelError::Apply { plugin: p.name().to_string(), detail: e.to_string() })?;
                for prov in p.provides() {
                    satisfied.insert(prov.to_string());
                }
                done.insert(i);
                applied.push(p.name().to_string());
                progressed = true;
            }
            if !progressed {
                let remaining = self
                    .plugins
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !done.contains(i))
                    .map(|(_, p)| p.name().to_string())
                    .collect();
                return Err(KernelError::UnresolvableOrder { remaining });
            }
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    struct Db(u32);
    struct Cache(#[allow(dead_code)] String);

    #[test]
    fn provide_and_get() {
        let ctx = Context::root();
        ctx.provide(Db(42));
        assert_eq!(ctx.get::<Db>().unwrap().0, 42);
        assert!(ctx.get::<Cache>().is_none());
        assert!(ctx.has::<Db>() && !ctx.has::<Cache>());
    }

    #[test]
    fn fork_inherits_and_overrides() {
        let ctx = Context::root();
        ctx.provide(Db(1));
        let child = ctx.fork();
        assert_eq!(child.get::<Db>().unwrap().0, 1); // inherited from parent
        child.provide(Db(2)); // shadow in child
        assert_eq!(child.get::<Db>().unwrap().0, 2);
        assert_eq!(ctx.get::<Db>().unwrap().0, 1); // parent unchanged
    }

    #[test]
    fn dispose_reverts_effects_lifo_and_is_idempotent() {
        let ctx = Context::root();
        let log = Arc::new(StdMutex::new(Vec::<i32>::new()));
        for i in 0..3 {
            let l = log.clone();
            ctx.effect(move || move || l.lock().unwrap().push(i)); // disposer records i
        }
        assert!(log.lock().unwrap().is_empty()); // nothing runs before dispose
        ctx.dispose();
        assert_eq!(*log.lock().unwrap(), vec![2, 1, 0]); // LIFO
        ctx.dispose(); // idempotent
        assert_eq!(*log.lock().unwrap(), vec![2, 1, 0]);
    }

    #[test]
    fn dispose_totality_leaves_no_residue() {
        let ctx = Context::root();
        ctx.provide(Db(9));
        ctx.dispose();
        assert!(ctx.get::<Db>().is_none()); // services cleared on dispose
    }

    struct DbPlugin;
    impl Plugin for DbPlugin {
        fn name(&self) -> &str {
            "db"
        }
        fn provides(&self) -> &[&str] {
            &["database"]
        }
        fn apply(&self, ctx: &Context, _c: &PluginConfig) -> Result<()> {
            ctx.provide(Db(7));
            Ok(())
        }
    }
    struct AppPlugin;
    impl Plugin for AppPlugin {
        fn name(&self) -> &str {
            "app"
        }
        fn inject(&self) -> &[&str] {
            &["database"]
        }
        fn apply(&self, ctx: &Context, _c: &PluginConfig) -> Result<()> {
            // The loader guaranteed `db` applied first, so the required service is present.
            assert_eq!(ctx.get::<Db>().expect("database service present").0, 7);
            Ok(())
        }
    }

    #[test]
    fn loader_orders_by_inject_not_registration() {
        let mut l = Loader::new();
        l.register(AppPlugin).register(DbPlugin); // registered app-first
        let ctx = Context::root();
        let order = l.apply(&ctx, &PluginConfig::default()).unwrap();
        assert_eq!(order, vec!["db".to_string(), "app".to_string()]); // db applied first anyway
    }

    #[test]
    fn loader_errors_on_missing_required_service() {
        let mut l = Loader::new();
        l.register(AppPlugin); // injects "database"; nobody provides it
        let ctx = Context::root();
        let err = l.apply(&ctx, &PluginConfig::default()).unwrap_err();
        assert!(matches!(err, KernelError::MissingService { .. }));
    }
}
