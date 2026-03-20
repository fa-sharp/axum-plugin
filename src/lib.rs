#![allow(clippy::type_complexity)]

use std::{fmt::Display, sync::Arc};

use anyhow::anyhow;
use axum::Router;
use futures::{
    FutureExt,
    future::{BoxFuture, join_all},
};

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

/// A lightweight wrapper around axum that enables building plugins around the router
pub struct App<S = Arc<TypeMap>> {
    base_router: Router<S>,
    plugins: Vec<Box<dyn AppPlugin<S>>>,
    state: TypeMap,
}

impl<S> App<S>
where
    S: TryFrom<TypeMap> + Clone + Send + Sync + 'static,
{
    /// Create a new server with the given routes.
    pub fn new() -> Self {
        Self {
            base_router: Router::new(),
            state: TypeMap::new(),
            plugins: Vec::new(),
        }
    }

    /// Register a plugin. This can be considered analogous to axum/tower's `layer` function
    /// and follows the same ordering (each plugin "wraps" the previous one).
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

    /// Build and initialize the server. Returns the base router, finalized state, and a future to run
    /// on graceful shutdown.
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

        let shutdown_fns = self
            .plugins
            .into_iter()
            .filter_map(|mut p| p.on_shutdown(&state));
        let on_shutdown = join_all(shutdown_fns);

        Ok((router, state, on_shutdown))
    }
}

/// Trait for a plugin that can be attached to the server
#[allow(unused_variables, reason = "trait functions with default no-op")]
pub trait AppPlugin<S = Arc<TypeMap>> {
    /// Init function that will run on server startup. Can add and manipulate state.
    fn on_init(&mut self, app_state: TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> {
        async { Ok(app_state) }.boxed()
    }

    /// Setup function that will run _after_ state is initialized (e.g. routes, middleware,
    /// and services should be added here)
    fn on_setup(&mut self, router: Router<S>, state: &S) -> anyhow::Result<Router<S>> {
        Ok(router)
    }

    /// Teardown function that will run on server shutdown
    fn on_shutdown(&mut self, state: &S) -> Option<BoxFuture<'static, ()>> {
        None
    }
}

/// Utility to build a plugin on the fly
pub struct AdHocPlugin<S = Arc<TypeMap>> {
    on_init: Option<Box<dyn FnOnce(TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> + Send>>,
    on_setup: Option<fn(router: Router<S>, state: &S) -> anyhow::Result<Router<S>>>,
    on_shutdown: Option<Box<dyn FnOnce(&S) -> BoxFuture<'static, ()>>>,
}

impl<S: 'static> AdHocPlugin<S> {
    pub fn new() -> Self {
        Self {
            on_init: None,
            on_setup: None,
            on_shutdown: None,
        }
    }

    /// Init function that will run on server startup. Can add and manipulate state.
    pub fn on_init<T>(mut self, on_init: fn(app_state: TypeMap) -> T) -> Self
    where
        T: Future<Output = anyhow::Result<TypeMap>> + Send + 'static,
    {
        self.on_init = Some(Box::new(move |s| Box::pin(on_init(s))));
        self
    }

    /// Setup function that will run _after_ state is initialized. (e.g. routes, middleware,
    /// and services should be added here)
    pub fn on_setup(
        mut self,
        on_setup: fn(router: Router<S>, state: &S) -> anyhow::Result<Router<S>>,
    ) -> Self {
        self.on_setup = Some(on_setup);
        self
    }

    /// Teardown function that will run on server shutdown
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
        match self.on_setup {
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
