//! A small plugin layer for [`axum`](https://docs.rs/axum/latest/axum/) applications.
//!
//! `axum-app-wrapper` lets plugins contribute startup state, router setup, and shutdown work
//! around an `axum::Router`.
//!
//! Lifecycle:
//!
//! 1. Configuration is loaded.
//! 2. [`AppPlugin::on_init`] runs in registration order and builds a [`TypeMap`].
//! 3. The `TypeMap` is converted into the app state `S` with `TryFrom<TypeMap>`.
//! 4. [`AppPlugin::on_setup`] runs in registration order and receives the router, state, and config.
//! 5. [`InitializedApp::shutdown`] runs [`AppPlugin::on_shutdown`] consecutively in reverse
//!    registration order.
//!
//! For one-off plugins, use [`AdHocPlugin`]. If the setup closure needs access to typed state,
//! spell the state type at construction time so Rust can infer the closure parameters:
//!
//! ```ignore
//! let plugin = AdHocPlugin::<AppState, AppConfig>::new()
//!     .on_init(|mut app| async move {
//!         app.insert(config);
//!         Ok(app)
//!     })
//!     .on_setup(|app| {
//!         let config = app.state().config.clone();
//!         Ok(app.into_router().layer(axum::Extension(config)))
//!     });
//! ```

extern crate self as axum_plugin;

use std::{borrow::Cow, fmt::Display, sync::Arc};

use anyhow::Context;
use axum::{Router, routing::MethodRouter};
use futures::{FutureExt, future::BoxFuture};

pub use axum_plugin_macros::AppState;
pub use type_map::concurrent::TypeMap;

pub use anyhow;

/// Error type used by this crate.
pub type Error = anyhow::Error;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Startup initialization future returned by [`AppPlugin::on_init`].
pub type InitFuture<C = ()> = BoxFuture<'static, Result<InitApp<C>>>;

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

/// Initialization context passed to [`AppPlugin::on_init`].
pub struct InitApp<C = ()> {
    config: Arc<C>,
    state: TypeMap,
}

impl<C> InitApp<C> {
    fn new(config: Arc<C>, state: TypeMap) -> Self {
        Self { config, state }
    }

    /// Get a reference to the application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the current state [`TypeMap`].
    pub fn state(&self) -> &TypeMap {
        &self.state
    }

    /// Mutably borrow the current state [`TypeMap`].
    pub fn state_mut(&mut self) -> &mut TypeMap {
        &mut self.state
    }

    /// Store a new value in state. This will fail if the type already exists in state.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> Result<()> {
        match self.state.contains::<T>() {
            true => anyhow::bail!(
                "Type already exists in state: '{}'",
                std::any::type_name::<T>()
            ),
            false => self.state.insert(value),
        };
        Ok(())
    }

    /// Borrow a state value by type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.state.get::<T>()
    }

    /// Remove a state value by type.
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.state.remove::<T>()
    }

    fn into_state(self) -> TypeMap {
        self.state
    }
}

/// Router setup context passed to [`AppPlugin::on_setup`].
pub struct SetupApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    router: Router<S>,
    state: S,
}

impl<S, C> SetupApp<S, C> {
    fn new(config: Arc<C>, router: Router<S>, state: S) -> Self {
        Self {
            config,
            router,
            state,
        }
    }

    /// Borrow the finalized application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the shared application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the finalized application state.
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Borrow the router.
    pub fn router(&self) -> &Router<S> {
        &self.router
    }

    /// Consume the setup context and return its router.
    pub fn into_router(self) -> Router<S> {
        self.router
    }
}

/// Shutdown context passed to [`AppPlugin::on_shutdown`].
pub struct ShutdownApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    state: S,
}

impl<S, C> ShutdownApp<S, C> {
    fn new(config: Arc<C>, state: S) -> Self {
        Self { config, state }
    }

    /// Borrow the finalized application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the shared application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the finalized application state.
    pub fn state(&self) -> &S {
        &self.state
    }
}

/// An axum app builder with plugin-managed config, state, router setup, and shutdown hooks.
pub struct App<S = TypeMapState, C = ()> {
    base_router: Router<S>,
    plugins: Vec<Box<dyn AppPlugin<S, C>>>,
    config: Arc<C>,
    state: TypeMap,
}

impl<S> App<S, ()>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
{
    /// Create an empty app.
    pub fn new() -> Self {
        Self {
            base_router: Router::new(),
            config: Arc::new(()),
            state: TypeMap::new(),
            plugins: Vec::new(),
        }
    }
}

impl<S, C> App<S, C>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    /// Create an empty app with preloaded config.
    pub fn with_config(config: C) -> Self {
        Self {
            base_router: Router::new(),
            config: Arc::new(config),
            state: TypeMap::new(),
            plugins: Vec::new(),
        }
    }

    /// Create an empty app by extracting config with a caller-provided loader.
    pub fn load_config_with(loader: impl FnOnce() -> Result<C>) -> Result<Self> {
        Ok(Self::with_config(loader()?))
    }

    /// Create an empty app by extracting config from a [`figment::Figment`].
    #[cfg(feature = "figment")]
    pub fn from_figment(figment: figment::Figment) -> Result<Self>
    where
        C: serde::de::DeserializeOwned,
    {
        Self::load_config_with(|| figment.extract::<C>().map_err(Into::into))
    }

    /// Create an empty app by extracting config from (in order of priority):
    /// 1. TOML file
    /// 2. Config struct defaults
    #[cfg(feature = "figment")]
    pub fn from_file(path: &str) -> Result<Self>
    where
        C: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        use figment::providers::Format;
        Self::load_config_with(|| {
            figment::Figment::from(figment::providers::Serialized::defaults(C::default()))
                .merge(figment::providers::Toml::file(path))
                .extract::<C>()
                .map_err(Into::into)
        })
    }

    /// Create an empty app by extracting config from (in order of priority):
    /// 1. Prefixed environment variables
    /// 2. Config struct defaults
    #[cfg(feature = "figment")]
    pub fn from_env(env_prefix: &str) -> Result<Self>
    where
        C: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        Self::load_config_with(|| {
            figment::Figment::from(figment::providers::Serialized::defaults(C::default()))
                .merge(figment::providers::Env::prefixed(env_prefix))
                .extract::<C>()
                .map_err(Into::into)
        })
    }

    /// Create an empty app by extracting config from (in order of priority):
    /// 1. Prefixed environment variables
    /// 2. TOML file
    /// 3. Config struct defaults
    #[cfg(feature = "figment")]
    pub fn from_env_and_file(path: impl AsRef<std::path::Path>, prefix: &str) -> Result<Self>
    where
        C: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        use figment::providers::Format;
        Self::load_config_with(|| {
            figment::Figment::from(figment::providers::Serialized::defaults(C::default()))
                .merge(figment::providers::Toml::file(path))
                .merge(figment::providers::Env::prefixed(prefix))
                .extract::<C>()
                .map_err(Into::into)
        })
    }

    /// Add a route to the base router before any setup hooks run. Prefer mounting most
    /// routes within plugins.
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

    /// Store a value in state. ⚠️ Beware this will replace any value of the same type.
    pub fn store<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        self.state.insert(state);
        self
    }

    /// Register a plugin.
    ///
    /// `on_init` and `on_setup` run in registration order. Shutdown runs in reverse registration
    /// order, awaiting each hook before starting the next.
    pub fn register(mut self, plugin: impl AppPlugin<S, C> + 'static) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Build the router, finalized state, and graceful shutdown handle.
    pub async fn init(mut self) -> Result<InitializedApp<S, C>>
    where
        S::Error: Display,
    {
        let mut app = InitApp::new(Arc::clone(&self.config), self.state);
        for plugin in self.plugins.iter_mut() {
            app = plugin
                .on_init(app)
                .await
                .with_context(|| format!("Error in on_init hook of plugin '{}'", plugin.name()))?;
        }

        let state = S::try_from(app.into_state())
            .map_err(|err| ::anyhow::anyhow!("Error creating state: {err}"))?;

        let mut router = self.base_router;
        for plugin in self.plugins.iter_mut() {
            router = plugin
                .on_setup(SetupApp::new(
                    Arc::clone(&self.config),
                    router,
                    state.clone(),
                ))
                .with_context(|| format!("Error in on_setup hook of plugin '{}'", plugin.name()))?;
        }

        let shutdown_fns: Vec<_> = self
            .plugins
            .into_iter()
            .rev()
            .filter_map(|mut p| {
                let name = p.name();
                let shutdown =
                    p.on_shutdown(ShutdownApp::new(Arc::clone(&self.config), state.clone()))?;
                Some((name, shutdown))
            })
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
            config: self.config,
            router,
            state,
            on_shutdown,
        })
    }
}

impl<S> Default for App<S, ()>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// A fully initialized axum app.
pub struct InitializedApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    router: Router<S>,
    state: S,
    on_shutdown: ShutdownFuture,
}

impl<S, C> InitializedApp<S, C>
where
    S: Clone + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    /// Build a ready-to-serve router by attaching the finalized state.
    pub fn router(&self) -> Router<()> {
        self.router.clone().with_state(self.state.clone())
    }

    /// Borrow the finalized app state.
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Borrow the finalized app config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the shared app config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Consume the initialized app and return its raw parts.
    pub fn into_parts(self) -> (Router<S>, S, Arc<C>, ShutdownFuture) {
        (self.router, self.state, self.config, self.on_shutdown)
    }

    /// Return the future that runs shutdown for all plugins. Consumes the app.
    pub fn shutdown(self) -> ShutdownFuture {
        self.on_shutdown
    }
}

/// A plugin that can participate in app initialization, router setup, and shutdown.
#[allow(unused_variables, reason = "trait functions with default no-op")]
pub trait AppPlugin<S = TypeMapState, C = ()>
where
    C: Send + Sync + 'static,
{
    /// Plugin name
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Runs during startup before typed state exists.
    /// Use this hook to insert or transform values in the shared [`TypeMap`].
    fn on_init(&mut self, app: InitApp<C>) -> InitFuture<C> {
        async { Ok(app) }.boxed()
    }

    /// Runs after typed state has been created.
    /// Use this hook to add routes, services, middleware, or router state.
    fn on_setup(&mut self, app: SetupApp<S, C>) -> Result<Router<S>> {
        Ok(app.into_router())
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self, app: ShutdownApp<S, C>) -> Option<ShutdownFuture> {
        None
    }
}

/// A closure-based plugin for application-local setup.
///
/// `on_init` and `on_setup` accept capturing closures. If hooks use typed state or config,
/// prefer `AdHocPlugin::<State, Config>::new()` so the closure parameter type can be inferred.
pub struct AdHocPlugin<S = TypeMapState, C = ()> {
    name: Cow<'static, str>,
    on_init: Option<InitFn<C>>,
    on_setup: Option<SetupFn<S, C>>,
    on_shutdown: Option<ShutdownFn<S, C>>,
}

type InitFn<C> = Box<dyn FnOnce(InitApp<C>) -> InitFuture<C> + Send>;
type SetupFn<S, C> = Box<dyn FnOnce(SetupApp<S, C>) -> Result<Router<S>> + Send>;
type ShutdownFn<S, C> = Box<dyn FnOnce(ShutdownApp<S, C>) -> ShutdownFuture + Send>;

impl<S: 'static, C: 'static> AdHocPlugin<S, C> {
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

    /// Runs during startup before typed state exists.
    /// Use this hook to insert or transform values in the shared [`TypeMap`].
    pub fn on_init<F, T>(mut self, on_init: F) -> Self
    where
        F: FnOnce(InitApp<C>) -> T + Send + 'static,
        T: Future<Output = Result<InitApp<C>>> + Send + 'static,
    {
        self.on_init = Some(Box::new(move |s| Box::pin(on_init(s))));
        self
    }

    /// Runs after typed state has been created.
    /// Use this hook to add routes, services, middleware, or router state.
    pub fn on_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(SetupApp<S, C>) -> Result<Router<S>> + Send + 'static,
    {
        self.on_setup = Some(Box::new(on_setup));
        self
    }

    /// Set shutdown work for this plugin.
    pub fn on_shutdown<F, T>(mut self, on_shutdown: F) -> Self
    where
        F: FnOnce(ShutdownApp<S, C>) -> T + Send + 'static,
        T: Future<Output = Result<()>> + Send + 'static,
    {
        self.on_shutdown = Some(Box::new(move |s| Box::pin(on_shutdown(s))));
        self
    }
}

impl<S: 'static, C: 'static> Default for AdHocPlugin<S, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, C> AppPlugin<S, C> for AdHocPlugin<S, C>
where
    C: Send + Sync + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        self.name.clone()
    }

    fn on_init(&mut self, app: InitApp<C>) -> InitFuture<C> {
        match self.on_init.take() {
            Some(init_fn) => async move { init_fn(app).await }.boxed(),
            None => async { Ok(app) }.boxed(),
        }
    }

    fn on_setup(&mut self, app: SetupApp<S, C>) -> Result<Router<S>> {
        match self.on_setup.take() {
            Some(setup_fn) => setup_fn(app),
            None => Ok(app.into_router()),
        }
    }

    fn on_shutdown(&mut self, app: ShutdownApp<S, C>) -> Option<ShutdownFuture> {
        match self.on_shutdown.take() {
            Some(shutdown_fn) => Some(shutdown_fn(app)),
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
            .on_init(async |mut app| {
                app.insert(init_value)?;
                Ok(app)
            })
            .on_setup(move |app| {
                assert_eq!(app.state().value.as_str(), setup_value.as_str());
                Ok(app.into_router())
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

    #[derive(Clone)]
    struct TestConfig {
        label: Arc<String>,
    }

    #[test]
    fn config_is_available_during_lifecycle_and_after_init() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let init_events = Arc::clone(&events);
        let setup_events = Arc::clone(&events);
        let shutdown_events = Arc::clone(&events);

        let app = App::<TestState, TestConfig>::with_config(TestConfig {
            label: Arc::new(String::from("configured")),
        })
        .register(
            AdHocPlugin::<TestState, TestConfig>::named("ConfigAware")
                .on_init(move |mut app| async move {
                    init_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("init:{}", app.config().label));
                    app.insert(Arc::clone(&app.config().label))?;
                    Ok(app)
                })
                .on_setup(move |app| {
                    setup_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("setup:{}", app.config().label));
                    Ok(app.into_router())
                })
                .on_shutdown(move |app| {
                    let label = Arc::clone(&app.config().label);
                    async move {
                        shutdown_events
                            .lock()
                            .expect("events lock poisoned")
                            .push(format!("shutdown:{label}"));
                        Ok(())
                    }
                }),
        );

        let app = futures::executor::block_on(app.init()).expect("app should initialize");
        assert_eq!(app.config().label.as_str(), "configured");
        assert_eq!(app.state().value.as_str(), "configured");

        futures::executor::block_on(app.shutdown()).expect("shutdown should succeed");

        assert_eq!(
            *events.lock().expect("events lock poisoned"),
            [
                String::from("init:configured"),
                String::from("setup:configured"),
                String::from("shutdown:configured")
            ]
        );
    }

    #[cfg(feature = "figment")]
    #[derive(Clone, serde::Deserialize)]
    struct FigmentConfig {
        label: String,
    }

    #[cfg(feature = "figment")]
    #[test]
    fn from_figment_extracts_config() {
        let figment = figment::Figment::new().merge(("label", "figment"));
        let app = App::<TypeMapState, FigmentConfig>::from_figment(figment)
            .expect("config should extract from figment");

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(app.config().label, "figment");
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
            .register(AdHocPlugin::<TestState>::new().on_shutdown(move |app| {
                let events = Arc::clone(&shutdown_events);
                let value = Arc::clone(&app.state().value);
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
        fn on_shutdown(&mut self, _app: ShutdownApp<TestState>) -> Option<ShutdownFuture> {
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
            .register(AdHocPlugin::<TestState>::new().on_init(async |mut app| {
                app.insert(Arc::new(String::from("ready")))?;
                Ok(app)
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
