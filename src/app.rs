use std::{borrow::Cow, fmt::Display, sync::Arc};

use anyhow::Context;
use axum::Router;
use futures::FutureExt;
use type_map::concurrent::TypeMap;

use crate::{
    Result, TypeMapState,
    plugin::{AppContext, AppInitContext, AppPlugin, ShutdownFuture},
};

/// An axum app builder with plugin-managed config, state, router setup, and shutdown hooks.
pub struct App<S = TypeMapState, C = ()> {
    base_router: Router<S>,
    #[allow(clippy::type_complexity)]
    plugins: Vec<(Box<dyn AppPlugin<S, C>>, Option<Cow<'static, str>>)>,
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
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self>
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
    pub fn from_env_and_file(env_prefix: &str, path: impl AsRef<std::path::Path>) -> Result<Self>
    where
        C: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        use figment::providers::Format;
        Self::load_config_with(|| {
            figment::Figment::from(figment::providers::Serialized::defaults(C::default()))
                .merge(figment::providers::Toml::file(path))
                .merge(figment::providers::Env::prefixed(env_prefix))
                .extract::<C>()
                .map_err(Into::into)
        })
    }

    /// Store a value in state. This will return an error if a value of this type
    /// already exists in state.
    pub fn store<T: Send + Sync + 'static>(mut self, state: T) -> Result<Self> {
        if self.state.contains::<T>() {
            anyhow::bail!(
                "Type already exists in state: '{}'",
                std::any::type_name::<T>()
            )
        }
        self.state.insert(state);
        Ok(self)
    }

    /// Register a plugin.
    pub fn register(mut self, plugin: impl AppPlugin<S, C> + 'static) -> Self {
        self.plugins.push((Box::new(plugin), None));
        self
    }

    /// Register a plugin, with local routes nested under the given prefix.
    pub fn register_at(
        mut self,
        plugin: impl AppPlugin<S, C> + 'static,
        prefix: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.plugins.push((Box::new(plugin), Some(prefix.into())));
        self
    }

    /// Build the router, finalized state, and graceful shutdown handle.
    pub async fn init(mut self) -> Result<InitializedApp<S, C>>
    where
        S::Error: Display,
    {
        let mut app = AppInitContext::new(Arc::clone(&self.config), self.state);
        for (plugin, _) in self.plugins.iter_mut() {
            app = plugin
                .on_init(app)
                .await
                .with_context(|| format!("Error in on_init hook of plugin '{}'", plugin.name()))?;
        }

        let state = S::try_from(app.into_state())
            .map_err(|err| ::anyhow::anyhow!("Error creating state: {err}"))?;

        let mut global_router = self.base_router;
        for (plugin, prefix) in self.plugins.iter_mut() {
            // Build and mount local router
            let local_router = plugin
                .local_setup(AppContext::new(Arc::clone(&self.config), state.clone()))
                .with_context(|| {
                    format!("Error in on_local_setup hook of plugin '{}'", plugin.name())
                })?;
            global_router = match prefix.as_deref() {
                Some(prefix) => global_router.nest(prefix, local_router),
                None => global_router.merge(local_router),
            };

            // Apply global setup hook
            global_router = plugin
                .global_setup(
                    AppContext::new(Arc::clone(&self.config), state.clone()),
                    global_router,
                )
                .with_context(|| {
                    format!("Error in global_setup hook of plugin '{}'", plugin.name())
                })?;
        }

        let shutdown_fns: Vec<_> = self
            .plugins
            .into_iter()
            .rev()
            .filter_map(|(mut plugin, _)| {
                let name = plugin.name();
                let shutdown_fn = plugin.on_shutdown()?;
                Some((name, shutdown_fn))
            })
            .collect();
        let shutdown_config = Arc::clone(&self.config);
        let shutdown_state = state.clone();

        let on_shutdown = async move {
            for (plugin_name, shutdown_fn) in shutdown_fns {
                shutdown_fn(AppContext::new(
                    Arc::clone(&shutdown_config),
                    shutdown_state.clone(),
                ))
                .await
                .with_context(|| format!("Error in on_shutdown hook of plugin '{plugin_name}'"))?;
            }
            Ok(())
        }
        .boxed();

        Ok(InitializedApp {
            config: self.config,
            router: global_router,
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

#[cfg(test)]
mod tests {
    use crate::plugin::{AdHocPlugin, ShutdownFn};

    use super::*;

    use axum::{
        body::Body,
        extract::{Request, State},
        http::{HeaderMap, HeaderValue, StatusCode},
        middleware::Next,
        response::Response,
        routing::get,
    };
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
    };
    use tower::ServiceExt;

    #[derive(Clone, crate::AppState)]
    struct TestState {
        value: Arc<String>,
    }

    #[test]
    fn adhoc_plugin_with_basic_state() {
        let init_value = Arc::new(String::from("ready"));
        let setup_value = Arc::clone(&init_value);

        let plugin = AdHocPlugin::<TestState, ()>::new()
            .on_init(async |mut app| {
                app.insert(init_value)?;
                Ok(app)
            })
            .global_setup(move |app, router| {
                assert_eq!(app.state().value.as_str(), setup_value.as_str());
                Ok(router)
            });
        let app = App::<TestState>::new().register(plugin);

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(app.state().value.as_str(), "ready");
    }

    #[test]
    fn store_and_router_handle_expose_initialized_state() {
        let app = App::<TestState>::new()
            .store(Arc::new(String::from("stored")))
            .unwrap();

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
                .on_init(async move |mut app| {
                    init_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("init:{}", app.config().label));
                    app.insert(Arc::clone(&app.config().label))?;
                    Ok(app)
                })
                .global_setup(move |app, router| {
                    setup_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("setup:{}", app.config().label));
                    Ok(router)
                })
                .on_shutdown(async move |app| {
                    let label = Arc::clone(&app.config().label);
                    shutdown_events
                        .lock()
                        .expect("events lock poisoned")
                        .push(format!("shutdown:{label}"));

                    Ok(())
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
        let app: App = App::new().store(Arc::new(String::from("typemap"))).unwrap();

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        let value = app
            .state()
            .type_map()
            .get::<Arc<String>>()
            .expect("stored value should remain in typemap");
        assert_eq!(value.as_str(), "typemap");
    }

    #[derive(Clone, crate::AppState)]
    struct GenericState<T: Send + Sync + 'static> {
        value: Arc<T>,
    }

    #[test]
    fn app_state_derive_supports_generics() {
        let app = App::<GenericState<String>>::new()
            .store(Arc::new(String::from("generic")))
            .unwrap();

        let app = futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(app.state().value.as_str(), "generic");
    }

    #[test]
    fn adhoc_shutdown_accepts_capturing_fallible_closure() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let shutdown_events = Arc::clone(&events);

        let app = App::<TestState>::new()
            .store(Arc::new(String::from("ready")))
            .unwrap()
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
        fn on_shutdown(&mut self) -> Option<ShutdownFn<TestState>> {
            let name = self.name;
            let events = Arc::clone(&self.events);
            let active_shutdowns = Arc::clone(&self.active_shutdowns);
            let mut yielded = false;

            Some(Box::new(move |_app| {
                Box::pin(futures::future::poll_fn(move |cx| {
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
                }))
            }))
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

    #[test]
    fn plugin_middleware_accepts_axum_extractors() {
        async fn handler() -> &'static str {
            "ok"
        }

        async fn middleware(
            state: State<TestState>,
            headers: HeaderMap,
            req: Request,
            next: Next,
        ) -> Response {
            assert_eq!(state.value.as_str(), "middleware-state");
            assert_eq!(headers.get("x-input").unwrap(), "yes");

            let mut response = next.run(req).await;
            response
                .headers_mut()
                .insert("x-plugin", HeaderValue::from_static("ran"));
            response
        }

        let app = App::<TestState, TestConfig>::with_config(TestConfig {
            label: Arc::new(String::from("middleware-config")),
        })
        .store(Arc::new(String::from("middleware-state")))
        .unwrap()
        .register(
            AdHocPlugin::named("middleware")
                .global_setup(|_app, router| Ok(router.route("/middleware", get(handler))))
                .global_middleware(middleware),
        );

        let app = futures::executor::block_on(app.init()).expect("app should initialize");
        let request = Request::builder()
            .uri("/middleware")
            .header("x-input", "yes")
            .body(Body::empty())
            .expect("request should build");

        let response = futures::executor::block_on(app.router().oneshot(request))
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-plugin").unwrap(), "ran");
    }
}
