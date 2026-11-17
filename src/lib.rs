#![doc = include_str!("../README.md")]

extern crate self as axum_plugin;

use std::sync::Arc;

#[cfg(feature = "macros")]
pub use axum_plugin_macros::AppState;
pub use type_map::concurrent::TypeMap;

pub use anyhow;
#[cfg(feature = "config")]
pub use figment;

mod app;

pub mod plugin;
pub mod utils;

pub use app::{App, InitializedApp};
pub use plugin::{AdHocPlugin, AppContext, AppInitContext, AppPlugin, PluginMiddleware};

/// Error type used by this crate.
pub type Error = anyhow::Error;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Default app state that keeps the startup [`TypeMap`] available behind an [`Arc`].
#[derive(Clone)]
pub struct TypeMapState(Arc<TypeMap>);

impl TypeMapState {
    /// Borrow the underlying [`TypeMap`].
    pub fn type_map(&self) -> &TypeMap {
        &self.0
    }
}

impl From<TypeMap> for TypeMapState {
    fn from(map: TypeMap) -> Self {
        Self(Arc::new(map))
    }
}
