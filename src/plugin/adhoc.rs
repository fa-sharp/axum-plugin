use std::borrow::Cow;

use axum::Router;
use futures::FutureExt;

use crate::{
    Result, TypeMapState,
    plugin::{AppPlugin, InitApp, InitFuture, SetupApp, ShutdownApp, ShutdownFuture},
};

/**
A convenient closure-based plugin for application setup.

```
use axum_plugin::AdHocPlugin;

// Use your app state and config types
type AppState = String;
type AppConfig = ();

let db_plugin = AdHocPlugin::<AppState, AppConfig>::named("database")
    .on_init(async |mut app| {
        // Initialize database
        let db = "database";

        // Add to state
        app.insert(db)?;

        // Return app
        Ok(app)
    });
```
*/
pub struct AdHocPlugin<S = TypeMapState, C = ()> {
    name: Cow<'static, str>,
    on_init: Option<InitFn<C>>,
    on_setup: Option<SetupFn<S, C>>,
    on_shutdown: Option<ShutdownFn<S, C>>,
}

type InitFn<C> = Box<dyn FnOnce(InitApp<C>) -> InitFuture<C> + Send>;
type SetupFn<S, C> = Box<dyn FnOnce(SetupApp<S, C>) -> Result<Router<S>> + Send>;
type ShutdownFn<S, C> = Box<dyn FnOnce(ShutdownApp<S, C>) -> ShutdownFuture + Send>;

impl<S, C> AdHocPlugin<S, C> {
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
