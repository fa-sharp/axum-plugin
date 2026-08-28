use std::{borrow::Cow, sync::Arc};

use axum::{Router, extract::Request, response::Response};
use futures::{FutureExt, future::BoxFuture};

use crate::{Result, TypeMapState};

mod adhoc;
mod init;
mod setup;
mod shutdown;

pub use adhoc::{AdHocPlugin, IntoRequestHookResult, IntoResponseHookResult};
pub use init::InitApp;
pub use setup::SetupApp;
pub use shutdown::ShutdownApp;

/// Startup initialization future returned by [`AppPlugin::on_init`].
pub type InitFuture<C = ()> = BoxFuture<'static, Result<InitApp<C>>>;

/// Asynchronous hook that will run on each request, returned by [`AppPlugin::on_request`].
pub type OnRequestFn<S = TypeMapState, C = ()> = Arc<
    dyn Fn(SetupApp<S, C>, Request) -> BoxFuture<'static, std::result::Result<Request, Response>>
        + Send
        + Sync,
>;

/// Asynchronous hook that will run on each response, returned by [`AppPlugin::on_response`].
pub type OnResponseFn<S = TypeMapState, C = ()> =
    Arc<dyn Fn(SetupApp<S, C>, Response) -> BoxFuture<'static, Response> + Send + Sync>;

/// Shutdown function returned by [`AppPlugin::on_shutdown`]
pub type ShutdownFn<S = TypeMapState, C = ()> =
    Box<dyn FnOnce(ShutdownApp<S, C>) -> ShutdownFuture + Send>;

/// Future returned by the shutdown function
pub type ShutdownFuture = BoxFuture<'static, Result<()>>;

/// Trait for plugins that can participate in app initialization, router setup, and shutdown.
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
    ///
    /// Use this hook to insert or transform values in state.
    fn on_init(&mut self, app: InitApp<C>) -> InitFuture<C> {
        async { Ok(app) }.boxed()
    }

    /// Runs before each request asynchronously, before any middleware defined in `on_setup`.
    fn on_request(&mut self) -> Option<OnRequestFn<S, C>> {
        None
    }

    /// Runs after each response asynchronously, after any middleware defined in `on_setup`.
    fn on_response(&mut self) -> Option<OnResponseFn<S, C>> {
        None
    }

    /// Runs after typed state has been created.
    ///
    /// Use this hook to add routes, services, middleware, or router state.
    fn on_setup(&mut self, app: SetupApp<S, C>, router: Router<S>) -> Result<Router<S>> {
        Ok(router)
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self) -> Option<ShutdownFn<S, C>> {
        None
    }
}
