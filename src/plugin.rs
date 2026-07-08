use std::borrow::Cow;

use axum::Router;
use futures::{FutureExt, future::BoxFuture};

use crate::{Result, TypeMapState};

mod adhoc;
mod init;
mod setup;
mod shutdown;

pub use adhoc::AdHocPlugin;
pub use init::InitApp;
pub use setup::SetupApp;
pub use shutdown::ShutdownApp;

/// Startup initialization future returned by [`AppPlugin::on_init`].
pub type InitFuture<C = ()> = BoxFuture<'static, Result<InitApp<C>>>;

/// Shutdown future returned by [`AppPlugin::on_shutdown`].
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
    /// Use this hook to insert or transform values in the shared [`crate::TypeMap`].
    fn on_init(&mut self, app: InitApp<C>) -> InitFuture<C> {
        async { Ok(app) }.boxed()
    }

    /// Runs after typed state has been created.
    /// Use this hook to add routes, services, middleware, or router state.
    fn on_setup(&mut self, app: SetupApp<S, C>, router: Router<S>) -> Result<Router<S>> {
        Ok(router)
    }

    /// Return shutdown work for this plugin.
    ///
    /// Shutdown hooks are awaited consecutively in reverse registration order.
    fn on_shutdown(&mut self, app: ShutdownApp<S, C>) -> Option<ShutdownFuture> {
        None
    }
}
