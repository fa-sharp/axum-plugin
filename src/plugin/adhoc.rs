use std::{borrow::Cow, convert::Infallible, sync::Arc};

use axum::{
    Router,
    extract::Request,
    middleware::{self, FromFnLayer},
    response::{IntoResponse, Response},
    routing::Route,
};
use futures::FutureExt;
use tower::{Layer, Service};

use crate::{
    Result, TypeMapState,
    plugin::{
        AppContext, AppInitContext, AppPlugin, InitFuture, OnRequestFn, OnResponseFn, ShutdownFn,
    },
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
    on_request: Option<OnRequestFn<S, C>>,
    on_response: Option<OnResponseFn<S, C>>,
    on_setup: Option<SetupFn<S, C>>,
    middleware: Vec<SetupFn<S, C>>,
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
            on_request: None,
            on_response: None,
            on_setup: None,
            middleware: Vec::new(),
            on_shutdown: None,
        }
    }

    /// Create a named ad-hoc plugin.
    pub fn named(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            name: name.into(),
            on_init: None,
            on_request: None,
            on_response: None,
            on_setup: None,
            middleware: Vec::new(),
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

    /// Asynchronous hook that will run on each request. The closure takes a [`AppContext`] (with
    /// access to state and config) and [`Request`] as parameters, and should return the
    /// transformed request. If returning a result, the error type `E` must implement [`IntoResponse`].
    ///
    /// This hook runs **before** any middleware defined in this plugin's `on_setup` hook.
    pub fn on_request<F, T, R>(mut self, on_request: F) -> Self
    where
        F: Fn(AppContext<S, C>, Request) -> T + Send + Sync + 'static,
        T: Future<Output = R> + Send + 'static,
        R: IntoRequestHookResult,
    {
        self.on_request = Some(Arc::new(move |s, r| {
            #[allow(clippy::result_large_err)]
            Box::pin(on_request(s, r).map(|req| req.into_request_result()))
        }));
        self
    }

    /**
    Hook that runs after typed state has been created. The closure takes
    a [`AppContext`] (with access to state and config) and [axum::Router]
    as parameters, and should return a Result of a transformed router.

    Use this hook to add routes, services, middleware, or router state.
    ```
    use axum::routing::get;
    use axum_plugin::AdHocPlugin;

    #[derive(Clone)]
    struct AppState;

    let plugin = AdHocPlugin::<AppState>::named("myplugin")
        .on_setup(|app, router| {
            let routes = axum::Router::new()
                .route("/foo", get(async || "foo"));

            Ok(router.nest("/api", routes))
        });
    ```
    */
    pub fn on_setup<F>(mut self, on_setup: F) -> Self
    where
        F: FnOnce(AppContext<S, C>, Router<S>) -> Result<Router<S>> + Send + 'static,
    {
        self.on_setup = Some(Box::new(on_setup));
        self
    }

    /// Add middleware using [`axum::middleware::from_fn_with_state`] internally.
    ///
    /// The middleware function can use normal axum extractors. Add [`AppContext`] as an extractor
    /// to access finalized app state and config.
    ///
    /// ```
    /// use axum::{
    ///     extract::Request,
    ///     middleware::Next,
    ///     response::Response,
    /// };
    /// use axum_plugin::{AdHocPlugin, AppContext};
    ///
    /// #[derive(Clone)]
    /// struct AppState;
    ///
    /// async fn middleware(
    ///     app: AppContext<AppState>,
    ///     req: Request,
    ///     next: Next,
    /// ) -> Response {
    ///     let _state = app.state();
    ///     next.run(req).await
    /// }
    ///
    /// let plugin = AdHocPlugin::<AppState>::named("middleware")
    ///     .middleware(middleware);
    /// ```
    pub fn middleware<F, T>(mut self, middleware: F) -> Self
    where
        S: Clone + Send + Sync + 'static,
        C: Send + Sync + 'static,
        F: PluginMiddleware<S, C, T>,
    {
        self.middleware.push(Box::new(move |app, router| {
            let app = app.clone();
            Ok(middleware.layer(app, router))
        }));
        self
    }

    /// Asynchronous hook that will run on each response. The closure takes a [`AppContext`] (with
    /// access to state and config) and [`Response`] as parameters, and should return the
    /// transformed response. Can also return a `Result<Response, E>` where E implements
    /// [`IntoResponse`].
    pub fn on_response<F, T, R>(mut self, on_response: F) -> Self
    where
        F: Fn(AppContext<S, C>, Response) -> T + Send + Sync + 'static,
        T: Future<Output = R> + Send + 'static,
        R: IntoResponseHookResult,
    {
        self.on_response = Some(Arc::new(move |s, r| {
            Box::pin(on_response(s, r).map(|res| res.into_response()))
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

/// Trait for converting the result of a request hook into a [`Result<Request, Response>`].
pub trait IntoRequestHookResult {
    #[allow(clippy::result_large_err)]
    fn into_request_result(self) -> std::result::Result<Request, Response>;
}
impl IntoRequestHookResult for Request {
    fn into_request_result(self) -> std::result::Result<Request, Response> {
        Ok(self)
    }
}
impl<E> IntoRequestHookResult for std::result::Result<Request, E>
where
    E: IntoResponse,
{
    fn into_request_result(self) -> std::result::Result<Request, Response> {
        match self {
            Ok(req) => Ok(req),
            Err(e) => Err(e.into_response()),
        }
    }
}

/// Trait for converting the result of a response hook into a [`Response`].
pub trait IntoResponseHookResult {
    fn into_response(self) -> Response;
}
impl IntoResponseHookResult for Response {
    fn into_response(self) -> Response {
        self
    }
}
impl<E> IntoResponseHookResult for std::result::Result<Response, E>
where
    E: IntoResponse,
{
    fn into_response(self) -> Response {
        match self {
            Ok(resp) => resp,
            Err(e) => e.into_response(),
        }
    }
}

/// Trait implemented by functions that can be used as extractor-aware plugin middleware.
pub trait PluginMiddleware<S, C, T>: Clone + Send + 'static {
    /// Apply this middleware to a router.
    fn layer(self, app: AppContext<S, C>, router: Router<S>) -> Router<S>;
}

impl<S, C, F, T> PluginMiddleware<S, C, T> for F
where
    S: Clone + Send + Sync + 'static,
    C: Send + Sync + 'static,
    F: Clone + Send + 'static,
    T: 'static,
    FromFnLayer<F, AppContext<S, C>, T>: Layer<Route> + Clone + Send + Sync + 'static,
    <FromFnLayer<F, AppContext<S, C>, T> as Layer<Route>>::Service:
        Service<Request> + Clone + Send + Sync + 'static,
    <<FromFnLayer<F, AppContext<S, C>, T> as Layer<Route>>::Service as Service<Request>>::Response:
        IntoResponse + 'static,
    <<FromFnLayer<F, AppContext<S, C>, T> as Layer<Route>>::Service as Service<Request>>::Error:
        Into<Infallible> + 'static,
    <<FromFnLayer<F, AppContext<S, C>, T> as Layer<Route>>::Service as Service<Request>>::Future:
        Send + 'static,
{
    fn layer(self, app: AppContext<S, C>, router: Router<S>) -> Router<S> {
        router.layer(middleware::from_fn_with_state(app, self))
    }
}

impl<S, C> AppPlugin<S, C> for AdHocPlugin<S, C>
where
    S: Clone,
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

    fn on_request(&mut self) -> Option<OnRequestFn<S, C>> {
        match self.on_request.take() {
            Some(on_request) => Some(on_request),
            None => None,
        }
    }

    fn on_setup(&mut self, app: AppContext<S, C>, router: Router<S>) -> Result<Router<S>> {
        let router = match self.on_setup.take() {
            Some(setup_fn) => setup_fn(app.clone(), router),
            None => Ok(router),
        }?;

        self.middleware
            .drain(..)
            .try_fold(router, |router, middleware| middleware(app.clone(), router))
    }

    fn on_response(&mut self) -> Option<OnResponseFn<S, C>> {
        match self.on_response.take() {
            Some(on_response) => Some(on_response),
            None => None,
        }
    }

    fn on_shutdown(&mut self) -> Option<ShutdownFn<S, C>> {
        match self.on_shutdown.take() {
            Some(shutdown_fn) => Some(shutdown_fn),
            None => None,
        }
    }
}
