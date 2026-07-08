#![doc = include_str!("../README.md")]

extern crate self as axum_plugin;

use std::{fmt::Display, sync::Arc};

use anyhow::Context;
use axum::{Router, routing::MethodRouter};
use futures::FutureExt;

pub use axum_plugin_macros::AppState;
pub use type_map::concurrent::TypeMap;

pub use anyhow;
#[cfg(feature = "figment")]
pub use figment;

use crate::plugin::{InitApp, SetupApp, ShutdownApp};

pub mod plugin;
pub mod utils;

pub use plugin::{AdHocPlugin, AppPlugin};

/// Error type used by this crate.
pub type Error = anyhow::Error;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

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
    /// Create an empty app (useful for testing only)
    fn new() -> Self {
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
        let mut state = TypeMap::new();
        let config = Arc::new(config);
        state.insert(Arc::clone(&config));

        Self {
            base_router: Router::new(),
            config,
            state,
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
                .on_setup(
                    SetupApp::new(Arc::clone(&self.config), state.clone()),
                    router,
                )
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
    on_shutdown: plugin::ShutdownFuture,
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
    pub fn into_parts(self) -> (Router<S>, S, Arc<C>, plugin::ShutdownFuture) {
        (self.router, self.state, self.config, self.on_shutdown)
    }

    /// Return the future that runs shutdown for all plugins. Consumes the app.
    pub fn shutdown(self) -> plugin::ShutdownFuture {
        self.on_shutdown
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
            .on_setup(move |app, router| {
                assert_eq!(app.state().value.as_str(), setup_value.as_str());
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
                .on_setup(move |app, router| {
                    setup_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("setup:{}", app.config().label));
                    Ok(router)
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
        fn on_shutdown(&mut self, _app: ShutdownApp<TestState>) -> Option<plugin::ShutdownFuture> {
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
