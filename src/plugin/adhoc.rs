use std::{borrow::Cow, convert::Infallible};

use axum::{
    Router,
    extract::Request,
    middleware::{self, FromFnLayer},
    response::IntoResponse,
    routing::Route,
};
use futures::FutureExt;
use tower::{Layer, Service};

use crate::{
    Result, TypeMapState,
    plugin::{AppContext, AppInitContext, AppPlugin, InitFuture, ShutdownFn},
};

/**
A convenient closure-based plugin for application setup.

```
use axum_plugin::AdHocPlugin;

// Use your app config and state types
struct AppConfig {
    db_url: String,
}
struct AppState {
    db: DbConnection,
}
struct DbConnection(String);

let db_plugin = AdHocPlugin::<AppState, AppConfig>::named("database")
    .on_init(async |mut app| {
        // Initialize database
        let db = DbConnection(app.config().db_url.to_owned());

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
    local_setup: Option<SetupFn<S, C>>,
    local_middleware: Vec<SetupFn<S, C>>,
    global_setup: Option<SetupFn<S, C>>,
    global_middleware: Vec<SetupFn<S, C>>,
    on_shutdown: Option<ShutdownFn<S, C>>,
}

type InitFn<C> = Box<dyn FnOnce(AppInitContext<C>) -> InitFuture<C> + Send>;
type SetupFn<S, C> = Box<dyn FnOnce(AppContext<S, C>, Router<S>) -> Result<Router<S>> + Send>;

impl<S: 'static, C: 'static> Default for AdHocPlugin<S, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, C> AdHocPlugin<S, C> {
    /// Create an ad-hoc plugin. Prefer `named()` to help with debugging.
    pub fn new() -> Self {
        Self {
            name: Cow::Borrowed("adhoc"),
            on_init: None,
            local_setup: None,
            local_middleware: Vec::new(),
            global_setup: None,
            global_middleware: Vec::new(),
            on_shutdown: None,
        }
    }

    /// Create a named ad-hoc plugin.
    pub fn named(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            name: name.into(),
            on_init: None,
            local_setup: None,
            local_middleware: Vec::new(),
            global_setup: None,
            global_middleware: Vec::new(),
            on_shutdown: None,
        }
    }

    /**
    Hook that runs during startup, with mutable access to the state TypeMap.
    The async closure takes a [`AppInitContext`] parameter and should return it as
    a Result.

    Use this hook to perform initialization work and build state, e.g.
    setup database connections, etc.
    ```
    use axum_plugin::AdHocPlugin;

    struct AppState;
    struct Foo(&'static str);

    let plugin = AdHocPlugin::<AppState>::named("myplugin")
        .on_init(async |mut app| {
            // Do some init work
            let foo = Foo("some state");

            // Insert into the state TypeMap
            app.insert(foo)?;

            Ok(app)
        });
    ```
    */
    pub fn on_init<F, T>(mut self, on_init: F) -> Self
    where
        F: FnOnce(AppInitContext<C>) -> T + Send + 'static,
        T: Future<Output = Result<AppInitContext<C>>> + Send + 'static,
    {
        self.on_init = Some(Box::new(move |s| Box::pin(on_init(s))));
        self
    }

    /// Similar to [`Self::global_setup`], but manipulates the local router
    /// only. Routes added here will be nested under a prefix if configured.
    /// Middleware added here will only affect requests handled by this router.
    ///
    pub fn local_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(AppContext<S, C>, Router<S>) -> Result<Router<S>> + Send + 'static,
    {
        self.local_setup = Some(Box::new(on_setup));
        self
    }

    /**
    Hook that runs after state is initialized, to manipulate the global router.
    The closure takes
    a [`AppContext`] (with access to state and config) and [axum::Router]
    as parameters, and should return a Result of a transformed router.

    Use this hook to add routes and global middleware/services.
    ```
    use axum::routing::get;
    use axum_plugin::AdHocPlugin;

    #[derive(Clone)]
    struct AppState;

    let plugin = AdHocPlugin::<AppState>::named("myplugin")
        .global_setup(|app, router| {
            let routes = axum::Router::new()
                .route("/foo", get(async || "foo"));

            Ok(router.nest("/api", routes))
        });
    ```
    */
    pub fn global_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(AppContext<S, C>, Router<S>) -> Result<Router<S>> + Send + 'static,
    {
        self.global_setup = Some(Box::new(on_setup));
        self
    }

    /**
    Add middleware that will apply to the accumulated global app router. The middleware function
    can use normal axum extractors, including [`axum::extract::State`] for the app state.
    Uses [`axum::middleware::from_fn_with_state`] internally.

    ```
    use axum::{
        extract::{Request, State},
        middleware::Next,
        response::Response,
    };
    use axum_plugin::AdHocPlugin;

    #[derive(Clone)]
    struct AppState;

    async fn middleware(
        // Add extractors...
        State(state): State<AppState>,
        req: Request,
        next: Next,
    ) -> Response {
        next.run(req).await
    }

    let plugin = AdHocPlugin::<AppState>::named("middleware")
        .global_middleware(middleware);
    ```
    */
    pub fn global_middleware<F, T>(mut self, middleware: F) -> Self
    where
        S: Clone + Send + Sync + 'static,
        C: Send + Sync + 'static,
        F: PluginMiddleware<S, T>,
    {
        self.global_middleware.push(Box::new(move |app, router| {
            Ok(middleware.layer(app.state().clone(), router))
        }));
        self
    }

    /// Add middleware that is only applied to the local router for this plugin.
    ///
    /// The middleware function can use normal axum extractors, including
    /// [`axum::extract::State`] for the app state. Uses
    /// [`axum::middleware::from_fn_with_state`] internally.
    pub fn local_middleware<F, T>(mut self, middleware: F) -> Self
    where
        S: Clone + Send + Sync + 'static,
        C: Send + Sync + 'static,
        F: PluginMiddleware<S, T>,
    {
        self.local_middleware.push(Box::new(move |app, router| {
            Ok(middleware.layer(app.state().clone(), router))
        }));
        self
    }

    /// Hook to set shutdown work for this plugin. The async closure takes a
    /// [`AppContext`] parameter with access to state and config, and should return
    /// a `Result<()>`.
    ///
    /// Shutdown hooks are awaited consecutively in reverse plugin registration order.
    pub fn on_shutdown<F, T>(mut self, on_shutdown: F) -> Self
    where
        F: FnOnce(AppContext<S, C>) -> T + Send + 'static,
        T: Future<Output = Result<()>> + Send + 'static,
    {
        self.on_shutdown = Some(Box::new(move |s| Box::pin(on_shutdown(s))));
        self
    }
}

/// Trait implemented by functions that can be used as plugin middleware with axum extractors.
pub trait PluginMiddleware<S, T>: Clone + Send + 'static {
    /// Apply this middleware to a router.
    fn layer(self, state: S, router: Router<S>) -> Router<S>;
}

impl<S, F, T> PluginMiddleware<S, T> for F
where
    S: Clone + Send + Sync + 'static,
    F: Clone + Send + 'static,
    T: 'static,
    FromFnLayer<F, S, T>: Layer<Route> + Clone + Send + Sync + 'static,
    <FromFnLayer<F, S, T> as Layer<Route>>::Service:
        Service<Request> + Clone + Send + Sync + 'static,
    <<FromFnLayer<F, S, T> as Layer<Route>>::Service as Service<Request>>::Response:
        IntoResponse + 'static,
    <<FromFnLayer<F, S, T> as Layer<Route>>::Service as Service<Request>>::Error:
        Into<Infallible> + 'static,
    <<FromFnLayer<F, S, T> as Layer<Route>>::Service as Service<Request>>::Future: Send + 'static,
{
    fn layer(self, state: S, router: Router<S>) -> Router<S> {
        router.layer(middleware::from_fn_with_state(state, self))
    }
}

impl<S, C> AppPlugin<S, C> for AdHocPlugin<S, C>
where
    S: Send + Sync + Clone + 'static,
    C: Send + Sync + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        self.name.clone()
    }

    fn on_init(&mut self, app: AppInitContext<C>) -> InitFuture<C> {
        match self.on_init.take() {
            Some(init_fn) => async move { init_fn(app).await }.boxed(),
            None => async { Ok(app) }.boxed(),
        }
    }

    fn global_setup(&mut self, app: AppContext<S, C>, router: Router<S>) -> Result<Router<S>> {
        let router = match self.global_setup.take() {
            Some(setup_fn) => setup_fn(app.clone(), router),
            None => Ok(router),
        }?;

        self.global_middleware
            .drain(..)
            .try_fold(router, |router, middleware| middleware(app.clone(), router))
    }

    fn local_setup(&mut self, app: AppContext<S, C>, local_router: Router<S>) -> Result<Router<S>> {
        let local_router = match self.local_setup.take() {
            Some(setup_fn) => setup_fn(app.clone(), local_router),
            None => Ok(local_router),
        }?;

        self.local_middleware
            .drain(..)
            .try_fold(local_router, |router, middleware| {
                middleware(app.clone(), router)
            })
    }

    fn on_shutdown(&mut self) -> Option<ShutdownFn<S, C>> {
        match self.on_shutdown.take() {
            Some(shutdown_fn) => Some(shutdown_fn),
            None => None,
        }
    }
}
