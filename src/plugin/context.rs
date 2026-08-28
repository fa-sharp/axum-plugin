use std::{convert::Infallible, sync::Arc};

use axum::{extract::FromRequestParts, http::request::Parts};

use crate::TypeMapState;

/// App context used in plugins, with access to finalized state and config.
pub struct AppContext<S = TypeMapState, C = ()> {
    config: Arc<C>,
    state: S,
}

impl<S, C> Clone for AppContext<S, C>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            state: self.state.clone(),
        }
    }
}

impl<S, C> AppContext<S, C> {
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

impl<S, C> FromRequestParts<AppContext<S, C>> for AppContext<S, C>
where
    S: Clone + Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    type Rejection = Infallible;

    async fn from_request_parts(
        _parts: &mut Parts,
        app: &AppContext<S, C>,
    ) -> Result<Self, Self::Rejection> {
        Ok(app.clone())
    }
}
