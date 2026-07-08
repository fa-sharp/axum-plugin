use std::sync::Arc;

use axum::Router;

use crate::TypeMapState;

/// Router setup context passed to [`AppPlugin::on_setup`].
pub struct SetupApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    router: Router<S>,
    state: S,
}

impl<S, C> SetupApp<S, C> {
    pub(crate) fn new(config: Arc<C>, router: Router<S>, state: S) -> Self {
        Self {
            config,
            router,
            state,
        }
    }

    /// Borrow the finalized application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the shared application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the finalized application state.
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Borrow the router.
    pub fn router(&self) -> &Router<S> {
        &self.router
    }

    /// Consume the setup context and return its router.
    pub fn into_router(self) -> Router<S> {
        self.router
    }
}
