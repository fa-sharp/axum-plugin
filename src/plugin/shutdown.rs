use std::sync::Arc;

use crate::TypeMapState;

/// Shutdown context passed to [`crate::plugin::AppPlugin::on_shutdown`].
pub struct ShutdownApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    state: S,
}

impl<S, C> ShutdownApp<S, C> {
    pub(crate) fn new(config: Arc<C>, state: S) -> Self {
        Self { config, state }
    }

    /// Borrow the application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the application state.
    pub fn state(&self) -> &S {
        &self.state
    }
}
