use std::borrow::Cow;

use axum::Router;
use futures::{FutureExt, future::BoxFuture};

use crate::{Result, TypeMapState};

mod adhoc;
mod context;
mod init;

pub use adhoc::{AdHocPlugin, PluginMiddleware};
pub use context::AppContext;
pub use init::AppInitContext;

/// Startup initialization future returned by [`AppPlugin::on_init`].
pub type InitFuture<C = ()> = BoxFuture<'static, Result<AppInitContext<C>>>;

/// Shutdown function returned by [`AppPlugin::on_shutdown`]
pub type ShutdownFn<S = TypeMapState, C = ()> =
    Box<dyn FnOnce(AppContext<S, C>) -> ShutdownFuture + Send>;

/// Future returned by the shutdown function
pub type ShutdownFuture = BoxFuture<'static, Result<()>>;

/// Trait for plugins that can participate in app initialization, router setup, and shutdown.
#[allow(unused_variables, reason = "trait functions with default no-op")]
pub trait AppPlugin<S = TypeMapState, C = ()>
where
    S: Send + Sync + Clone + 'static,
    C: Send + Sync + 'static,
{
    /// Plugin name
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Runs during startup before typed state exists.
    ///
    /// Use this hook to insert or transform values in state.
    fn on_init(&mut self, app: AppInitContext<C>) -> InitFuture<C> {
        async { Ok(app) }.boxed()
    }

    /// Runs after state has been initialized, to return the local router for this plugin.
    fn local_setup(&mut self, app: AppContext<S, C>) -> Result<Router<S>> {
        Ok(Router::<S>::new())
    }

    /// Runs after state has been initialized, to manipulate the global router.
    fn global_setup(
        &mut self,
        app: AppContext<S, C>,
        global_router: Router<S>,
    ) -> Result<Router<S>> {
        Ok(global_router)
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self) -> Option<ShutdownFn<S, C>> {
        None
    }
}
