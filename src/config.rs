use std::sync::Arc;

use axum::{extract::FromRequestParts, http::StatusCode};

/// Axum extractor to get the application config
pub struct Config<C>(pub Arc<C>);

impl<S: Send + Sync, C: Send + Sync + 'static> FromRequestParts<S> for Config<C> {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<Arc<C>>() {
            Some(config) => Ok(Self(Arc::clone(config))),
            None => Err((StatusCode::INTERNAL_SERVER_ERROR, "app config not found")),
        }
    }
}
