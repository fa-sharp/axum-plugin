//! A small plugin layer for [`axum`](https://docs.rs/axum/latest/axum/) applications.
//!
//! `axum-app-wrapper` lets plugins contribute startup state, router setup, and shutdown work
//! around an `axum::Router`.
//!
//! Lifecycle:
//!
//! 1. [`AppPlugin::on_init`] runs in registration order and builds a [`TypeMap`].
//! 2. The `TypeMap` is converted into the app state `S` with `TryFrom<TypeMap>`.
//! 3. [`AppPlugin::on_setup`] runs in registration order and receives `Router<S>` plus `&S`.
//! 4. [`InitializedApp::shutdown`] runs [`AppPlugin::on_shutdown`] consecutively in reverse
//!    registration order.
//!
//! For one-off plugins, use [`AdHocPlugin`]. If the setup closure needs access to typed state,
//! spell the state type at construction time so Rust can infer the closure parameters:
//!
//! ```ignore
//! let plugin = AdHocPlugin::<AppState>::new()
//!     .on_init(async |mut state| {
//!         state.insert(config);
//!         Ok(state)
//!     })
//!     .on_setup(|router, state| {
//!         let config = state.config.clone();
//!         Ok(router.layer(axum::Extension(config)))
//!     });
//! ```

extern crate self as axum_app_wrapper;

use std::{borrow::Cow, fmt::Display, sync::Arc};

use anyhow::Context;
use axum::{Router, routing::MethodRouter};
use futures::{FutureExt, future::BoxFuture};

pub use axum_app_wrapper_macros::AppState;
pub use type_map::concurrent::TypeMap;

pub use anyhow;

/// Error type used by this crate.
pub type Error = anyhow::Error;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Startup initialization future returned by [`AppPlugin::on_init`].
pub type InitFuture = BoxFuture<'static, Result<TypeMap>>;

/// Shutdown future returned by [`AppPlugin::on_shutdown`].
pub type ShutdownFuture = BoxFuture<'static, Result<()>>;

/// Default app state that keeps the startup [`TypeMap`] available behind an [`Arc`].
#[derive(Clone)]
pub struct TypeMapState(Arc<TypeMap>);

impl TypeMapState {
    /// Borrow the underlying [`TypeMap`].
    pub fn type_map(&self) -> &TypeMap {
        &self.0
    }
}

impl From<TypeMap> for TypeMapState {
    fn from(map: TypeMap) -> Self {
        Self(Arc::new(map))
    }
}

/// Extracts a value of type `T` from a [`TypeMap`], returning an Anyhow error if the type is not present.
///
/// This is used internally by the [`AppState`] macro but is also available for manual
/// [`TryFrom<TypeMap>`] implementations.
pub fn extract_type_field<T: Send + Sync + 'static>(map: &mut TypeMap) -> Result<T> {
    map.remove::<T>()
        .ok_or_else(|| ::anyhow::anyhow!("Missing type in TypeMap: {}", std::any::type_name::<T>()))
}

// Plugin system

/// An axum app builder with plugin-managed state, router setup, and shutdown hooks.
pub struct App<S = TypeMapState> {
    base_router: Router<S>,
    plugins: Vec<Box<dyn AppPlugin<S>>>,
    state: TypeMap,
}

impl<S> App<S>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
{
    /// Create an empty app.
    pub fn new() -> Self {
        Self {
            base_router: Router::new(),
            state: TypeMap::new(),
            plugins: Vec::new(),
        }
    }

    /// Add a route to the base router before plugins run setup.
    pub fn route(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        self.base_router = self.base_router.route(path, method_router);
        self
    }

    /// Mount routes at the given path before any setup hooks run. Prefer mounting most
    /// routes within plugins.
    pub fn mount(mut self, path: &str, router: Router<S>) -> Self {
        self.base_router = match path {
            "" | "/" => self.base_router.merge(router),
            _ => self.base_router.nest(path, router),
        };
        self
    }

    /// Store a value in router state.
    pub fn store<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        self.state.insert(state);
        self
    }

    /// Register a plugin.
    ///
    /// `on_init` and `on_setup` run in registration order. Shutdown runs in reverse registration
    /// order, awaiting each hook before starting the next.
    pub fn register(mut self, plugin: impl AppPlugin<S> + 'static) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Build the router, finalized state, and graceful shutdown handle.
    pub async fn init(mut self) -> Result<InitializedApp<S>>
    where
        S::Error: Display,
    {
        let mut state = self.state;
        for plugin in self.plugins.iter_mut() {
            state = plugin
                .on_init(state)
                .await
                .with_context(|| format!("Error in on_init hook of plugin '{}'", plugin.name()))?;
        }

        let state =
            S::try_from(state).map_err(|err| ::anyhow::anyhow!("Error creating state: {err}"))?;

        let mut router = self.base_router;
        for plugin in self.plugins.iter_mut() {
            router = plugin
                .on_setup(router, &state)
                .with_context(|| format!("Error in on_setup hook of plugin '{}'", plugin.name()))?;
        }

        let shutdown_fns: Vec<_> = self
            .plugins
            .into_iter()
            .rev()
            .filter_map(|mut p| Some((p.name(), p.on_shutdown(&state)?)))
            .collect();
        let on_shutdown = async move {
            for (plugin_name, shutdown_fn) in shutdown_fns {
                shutdown_fn.await.with_context(|| {
                    format!("Error in on_shutdown hook of plugin '{plugin_name}'")
                })?;
            }
            Ok(())
        }
        .boxed();

        Ok(InitializedApp {
            router,
            state,
            on_shutdown,
        })
    }
}

impl<S> Default for App<S>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// A fully initialized axum app.
pub struct InitializedApp<S> {
    router: Router<S>,
    state: S,
    on_shutdown: ShutdownFuture,
}

impl<S> InitializedApp<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Build a ready-to-serve router by attaching the finalized state.
    pub fn router(&self) -> Router<()> {
        self.router.clone().with_state(self.state.clone())
    }

    /// Borrow the finalized app state.
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Consume the initialized app and return its raw parts.
    pub fn into_parts(self) -> (Router<S>, S, ShutdownFuture) {
        (self.router, self.state, self.on_shutdown)
    }

    /// Run graceful shutdown work for all plugins.
    pub async fn shutdown(self) -> Result<()> {
        self.on_shutdown.await
    }
}

/// A plugin that can participate in app initialization, router setup, and shutdown.
#[allow(unused_variables, reason = "trait functions with default no-op")]
pub trait AppPlugin<S = TypeMapState> {
    /// Plugin name
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Run during startup before typed state exists.
    ///
    /// Use this hook to insert or transform values in the shared [`TypeMap`].
    fn on_init(&mut self, app_state: TypeMap) -> InitFuture {
        async { Ok(app_state) }.boxed()
    }

    /// Run after typed state has been created.
    ///
    /// Use this hook to add routes, services, middleware, or router state.
    fn on_setup(&mut self, router: Router<S>, state: &S) -> Result<Router<S>> {
        Ok(router)
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self, state: &S) -> Option<ShutdownFuture> {
        None
    }
}

/// A closure-based plugin for application-local setup.
///
/// `on_init` and `on_setup` accept capturing closures. If `on_setup` uses typed state, prefer
/// `AdHocPlugin::<State>::new()` so the closure parameter type can be inferred.
pub struct AdHocPlugin<S = TypeMapState> {
    name: Cow<'static, str>,
    on_init: Option<InitFn>,
    on_setup: Option<SetupFn<S>>,
    on_shutdown: Option<ShutdownFn<S>>,
}

type InitFn = Box<dyn FnOnce(TypeMap) -> InitFuture + Send>;
type SetupFn<S> = Box<dyn FnOnce(Router<S>, &S) -> Result<Router<S>> + Send>;
type ShutdownFn<S> = Box<dyn FnOnce(&S) -> ShutdownFuture + Send>;

impl<S: 'static> AdHocPlugin<S> {
    /// Create an ad-hoc plugin. Prefer `named()` to help with debugging.
    pub fn new() -> Self {
        Self {
            name: Cow::Borrowed("adhoc"),
            on_init: None,
            on_setup: None,
            on_shutdown: None,
        }
    }

    /// Create a named ad-hoc plugin.
    pub fn named(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            name: name.into(),
            on_init: None,
            on_setup: None,
            on_shutdown: None,
        }
    }

    /// Set startup state initialization for this plugin.
    pub fn on_init<F, T>(mut self, on_init: F) -> Self
    where
        F: FnOnce(TypeMap) -> T + Send + 'static,
        T: Future<Output = Result<TypeMap>> + Send + 'static,
    {
        self.on_init = Some(Box::new(move |s| Box::pin(on_init(s))));
        self
    }

    /// Set router setup for this plugin.
    pub fn on_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(Router<S>, &S) -> Result<Router<S>> + Send + 'static,
    {
        self.on_setup = Some(Box::new(on_setup));
        self
    }

    /// Set shutdown work for this plugin.
    pub fn on_shutdown<F, T>(mut self, on_shutdown: F) -> Self
    where
        F: FnOnce(&S) -> T + Send + 'static,
        T: Future<Output = Result<()>> + Send + 'static,
    {
        self.on_shutdown = Some(Box::new(move |s| Box::pin(on_shutdown(s))));
        self
    }
}

impl<S: 'static> Default for AdHocPlugin<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> AppPlugin<S> for AdHocPlugin<S> {
    fn name(&self) -> Cow<'static, str> {
        self.name.clone()
    }

    fn on_init(&mut self, app_state: TypeMap) -> InitFuture {
        match self.on_init.take() {
            Some(init_fn) => async move { init_fn(app_state).await }.boxed(),
            None => async { Ok(app_state) }.boxed(),
        }
    }

    fn on_setup(&mut self, router: Router<S>, state: &S) -> Result<Router<S>> {
        match self.on_setup.take() {
            Some(setup_fn) => setup_fn(router, state),
            None => Ok(router),
        }
    }

    fn on_shutdown(&mut self, state: &S) -> Option<ShutdownFuture> {
        match self.on_shutdown.take() {
            Some(shutdown_fn) => Some(shutdown_fn(state)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
    };

    #[derive(Clone, self::AppState)]
    struct TestState {
        value: Arc<String>,
    }

    #[test]
    fn adhoc_plugin_with_basic_state() {
        let init_value = Arc::new(String::from("ready"));
        let setup_value = Arc::clone(&init_value);

        let plugin = AdHocPlugin::<TestState>::new()
            .on_init(async |mut state| {
                state.insert(init_value);
                Ok(state)
            })
            .on_setup(move |router, state| {
                assert_eq!(state.value.as_str(), setup_value.as_str());
                Ok(router)
            });
        let app = App::<TestState>::new().register(plugin);

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(app.state().value.as_str(), "ready");
    }

    #[test]
    fn store_and_router_handle_expose_initialized_state() {
        let app = App::<TestState>::new().store(Arc::new(String::from("stored")));

        let app = futures::executor::block_on(app.init()).expect("app should initialize");
        let _router = app.router();

        assert_eq!(app.state().value.as_str(), "stored");
    }

    #[test]
    fn default_state_keeps_type_map_available() {
        let app: App = App::new().store(Arc::new(String::from("typemap")));

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        let value = app
            .state()
            .type_map()
            .get::<Arc<String>>()
            .expect("stored value should remain in typemap");
        assert_eq!(value.as_str(), "typemap");
    }

    #[derive(Clone, AppState)]
    struct GenericState<T: Send + Sync + 'static> {
        value: Arc<T>,
    }

    #[test]
    fn app_state_derive_supports_generics() {
        let app = App::<GenericState<String>>::new().store(Arc::new(String::from("generic")));

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(app.state().value.as_str(), "generic");
    }

    #[test]
    fn adhoc_shutdown_accepts_capturing_fallible_closure() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let shutdown_events = Arc::clone(&events);

        let app = App::<TestState>::new()
            .store(Arc::new(String::from("ready")))
            .register(AdHocPlugin::<TestState>::new().on_shutdown(move |state| {
                let events = Arc::clone(&shutdown_events);
                let value = Arc::clone(&state.value);
                async move {
                    events
                        .lock()
                        .expect("events lock poisoned")
                        .push(value.to_string());
                    Ok(())
                }
            }));

        let app = futures::executor::block_on(app.init()).expect("app should initialize");
        futures::executor::block_on(app.shutdown()).expect("shutdown should succeed");

        assert_eq!(
            *events.lock().expect("events lock poisoned"),
            [String::from("ready")]
        );
    }

    struct ShutdownOrderPlugin {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
        active_shutdowns: Arc<AtomicUsize>,
    }

    impl AppPlugin<TestState> for ShutdownOrderPlugin {
        fn on_shutdown(&mut self, _state: &TestState) -> Option<ShutdownFuture> {
            let name = self.name;
            let events = Arc::clone(&self.events);
            let active_shutdowns = Arc::clone(&self.active_shutdowns);
            let mut yielded = false;

            Some(Box::pin(futures::future::poll_fn(move |cx| {
                if !yielded {
                    yielded = true;
                    let previously_active = active_shutdowns.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(previously_active, 0, "shutdown hooks ran concurrently");
                    events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("{name}:start"));
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }

                events
                    .lock()
                    .expect("events lock poisoned")
                    .push(format!("{name}:finish"));
                active_shutdowns.fetch_sub(1, Ordering::SeqCst);
                Poll::Ready(Ok(()))
            })))
        }
    }

    #[test]
    fn shutdown_hooks_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let active_shutdowns = Arc::new(AtomicUsize::new(0));

        let app = App::<TestState>::new()
            .register(AdHocPlugin::<TestState>::new().on_init(async |mut state| {
                state.insert(Arc::new(String::from("ready")));
                Ok(state)
            }))
            .register(ShutdownOrderPlugin {
                name: "first",
                events: Arc::clone(&events),
                active_shutdowns: Arc::clone(&active_shutdowns),
            })
            .register(ShutdownOrderPlugin {
                name: "second",
                events: Arc::clone(&events),
                active_shutdowns,
            });

        let app = futures::executor::block_on(app.init()).expect("app should initialize");
        futures::executor::block_on(app.shutdown()).expect("shutdown should succeed");

        assert_eq!(
            *events.lock().expect("events lock poisoned"),
            [
                "second:start",
                "second:finish",
                "first:start",
                "first:finish"
            ]
        );
    }
}
