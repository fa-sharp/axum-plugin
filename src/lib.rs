#![allow(clippy::type_complexity)]
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
//! 4. The returned shutdown future runs [`AppPlugin::on_shutdown`] consecutively in reverse
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

use std::{fmt::Display, sync::Arc};

use anyhow::anyhow;
use axum::Router;
use futures::{FutureExt, future::BoxFuture};

// State extraction utilities

pub use axum_app_wrapper_macros::AppState;
pub use type_map::concurrent::TypeMap;

/// Extracts a value of type `T` from a [`TypeMap`], returning an Anyhow error if the type is not present.
///
/// This is used internally by the [`AppState`] macro but is also available for manual
/// [`TryFrom<TypeMap>`] implementations.
pub fn extract_type_field<T: Send + Sync + 'static>(map: &mut TypeMap) -> anyhow::Result<T> {
    map.remove::<T>()
        .ok_or_else(|| anyhow!("Missing type in TypeMap: {}", std::any::type_name::<T>()))
}

// Plugin system

/// An axum app builder with plugin-managed state, router setup, and shutdown hooks.
pub struct App<S = Arc<TypeMap>> {
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

    /// Register a plugin.
    ///
    /// `on_init` and `on_setup` run in registration order. Shutdown runs in reverse registration
    /// order, awaiting each hook before starting the next.
    pub fn register(mut self, plugin: impl AppPlugin<S> + 'static) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    // /// Mount the routes at the given path. All layers / middleware from plugins
    // /// will be applied to these routes.
    // pub fn mount(mut self, path: &str, router: Router<S>) -> Self {
    //     self.base_router = match path {
    //         "" | "/" => self.base_router.merge(router),
    //         _ => self.base_router.nest(path, router),
    //     };
    //     self
    // }

    // /// Store type T in state
    // pub fn store<T: Send + Sync + 'static>(mut self, state: T) -> Self {
    //     self.state.insert(state);
    //     self
    // }

    /// Build the router, finalized state, and graceful shutdown future.
    ///
    /// The returned shutdown future must be awaited by the caller when the server is shutting down.
    pub async fn init(mut self) -> anyhow::Result<(Router<S>, S, impl Future + Send)>
    where
        S::Error: Display,
    {
        let mut state = self.state;
        for plugin in self.plugins.iter_mut() {
            state = plugin.on_init(state).await?;
        }

        let state = S::try_from(state).map_err(|err| anyhow!("Error creating state: {err}"))?;

        let mut router = self.base_router;
        for plugin in self.plugins.iter_mut() {
            router = plugin.on_setup(router, &state)?;
        }

        let shutdown_fns: Vec<_> = self
            .plugins
            .into_iter()
            .rev()
            .filter_map(|mut p| p.on_shutdown(&state))
            .collect();
        let on_shutdown = async move {
            for shutdown_fn in shutdown_fns {
                shutdown_fn.await;
            }
        };

        Ok((router, state, on_shutdown))
    }
}

/// A plugin that can participate in app initialization, router setup, and shutdown.
#[allow(unused_variables, reason = "trait functions with default no-op")]
pub trait AppPlugin<S = Arc<TypeMap>> {
    /// Run during startup before typed state exists.
    ///
    /// Use this hook to insert or transform values in the shared [`TypeMap`].
    fn on_init(&mut self, app_state: TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> {
        async { Ok(app_state) }.boxed()
    }

    /// Run after typed state has been created.
    ///
    /// Use this hook to add routes, services, middleware, or router state.
    fn on_setup(&mut self, router: Router<S>, state: &S) -> anyhow::Result<Router<S>> {
        Ok(router)
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self, state: &S) -> Option<BoxFuture<'static, ()>> {
        None
    }
}

/// A closure-based plugin for application-local setup.
///
/// `on_init` and `on_setup` accept capturing closures. If `on_setup` uses typed state, prefer
/// `AdHocPlugin::<State>::new()` so the closure parameter type can be inferred.
pub struct AdHocPlugin<S = Arc<TypeMap>> {
    on_init: Option<Box<dyn FnOnce(TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> + Send>>,
    on_setup: Option<Box<dyn FnOnce(Router<S>, &S) -> anyhow::Result<Router<S>> + Send>>,
    on_shutdown: Option<Box<dyn FnOnce(&S) -> BoxFuture<'static, ()>>>,
}

impl<S: 'static> AdHocPlugin<S> {
    /// Create an empty ad-hoc plugin.
    pub fn new() -> Self {
        Self {
            on_init: None,
            on_setup: None,
            on_shutdown: None,
        }
    }

    /// Set startup state initialization for this plugin.
    pub fn on_init<F, T>(mut self, on_init: F) -> Self
    where
        F: FnOnce(TypeMap) -> T + Send + 'static,
        T: Future<Output = anyhow::Result<TypeMap>> + Send + 'static,
    {
        self.on_init = Some(Box::new(move |s| Box::pin(on_init(s))));
        self
    }

    /// Set router setup for this plugin.
    pub fn on_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(Router<S>, &S) -> anyhow::Result<Router<S>> + Send + 'static,
    {
        self.on_setup = Some(Box::new(on_setup));
        self
    }

    /// Set shutdown work for this plugin.
    pub fn on_shutdown<T>(mut self, on_shutdown: fn(state: &S) -> T) -> Self
    where
        T: Future<Output = ()> + Send + 'static,
    {
        self.on_shutdown = Some(Box::new(move |s| Box::pin(on_shutdown(s))));
        self
    }
}

impl<S> AppPlugin<S> for AdHocPlugin<S> {
    fn on_init(&mut self, app_state: TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> {
        match self.on_init.take() {
            Some(init_fn) => async move { init_fn(app_state).await }.boxed(),
            None => async { Ok(app_state) }.boxed(),
        }
    }

    fn on_setup(&mut self, router: Router<S>, state: &S) -> anyhow::Result<Router<S>> {
        match self.on_setup.take() {
            Some(setup_fn) => setup_fn(router, state),
            None => Ok(router),
        }
    }

    fn on_shutdown(&mut self, state: &S) -> Option<BoxFuture<'static, ()>> {
        match self.on_shutdown.take() {
            Some(shutdown_fn) => Some(shutdown_fn(state)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
    };

    use super::*;

    #[derive(Clone)]
    struct TestState {
        value: Arc<String>,
    }

    impl TryFrom<TypeMap> for TestState {
        type Error = Infallible;

        fn try_from(mut map: TypeMap) -> Result<Self, Self::Error> {
            Ok(Self {
                value: map.remove::<Arc<String>>().expect("missing state value"),
            })
        }
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

        let (_router, state, _shutdown) =
            futures::executor::block_on(app.init()).expect("app should initialize");

        assert_eq!(state.value.as_str(), "ready");
    }

    struct ShutdownOrderPlugin {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
        active_shutdowns: Arc<AtomicUsize>,
    }

    impl AppPlugin<TestState> for ShutdownOrderPlugin {
        fn on_shutdown(&mut self, _state: &TestState) -> Option<BoxFuture<'static, ()>> {
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
                Poll::Ready(())
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

        let (_router, _state, on_shutdown) =
            futures::executor::block_on(app.init()).expect("app should initialize");
        futures::executor::block_on(on_shutdown);

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
