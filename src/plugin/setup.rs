use std::sync::Arc;

use crate::TypeMapState;

/// Router setup context passed to [`crate::plugin::AppPlugin::on_setup`].
pub struct SetupApp<S = TypeMapState, C = ()> {
    config: Arc<C>,
    state: S,
}

impl<S, C> SetupApp<S, C> {
    pub(crate) fn new(config: Arc<C>, state: S) -> Self {
        Self { config, state }
    }

    /// Borrow the finalized application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the finalized application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Borrow the finalized application state.
    pub fn state(&self) -> &S {
        &self.state
    }
}
