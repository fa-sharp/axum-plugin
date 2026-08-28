use std::sync::Arc;

use type_map::concurrent::TypeMap;

use crate::Result;

/// Initialization context passed to [`crate::plugin::AppPlugin::on_init`].
pub struct AppInitContext<C = ()> {
    config: Arc<C>,
    state: TypeMap,
}

impl<C> AppInitContext<C> {
    pub(crate) fn new(config: Arc<C>, state: TypeMap) -> Self {
        Self { config, state }
    }

    /// Get a reference to the application config.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Clone the application config.
    pub fn config_arc(&self) -> Arc<C> {
        Arc::clone(&self.config)
    }

    /// Store a new value in state. This will fail if the type already exists in state.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> Result<()> {
        match self.state.contains::<T>() {
            true => anyhow::bail!(
                "Type already exists in state: '{}'",
                std::any::type_name::<T>()
            ),
            false => self.state.insert(value),
        };
        Ok(())
    }

    /// Borrow a state value by type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.state.get::<T>()
    }

    /// Remove a state value by type.
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.state.remove::<T>()
    }

    /// Borrow the current state [`TypeMap`].
    pub fn state(&self) -> &TypeMap {
        &self.state
    }

    /// Mutably borrow the current state [`TypeMap`].
    pub fn state_mut(&mut self) -> &mut TypeMap {
        &mut self.state
    }

    pub(crate) fn into_state(self) -> TypeMap {
        self.state
    }
}
