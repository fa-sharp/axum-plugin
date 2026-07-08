use type_map::concurrent::TypeMap;

use crate::Result;

/// Extracts a value of type `T` from a [`TypeMap`], returning an Anyhow error if the type is not present.
///
/// This is used internally by the [`crate::AppState`] macro but is also available for manual
/// [`TryFrom<TypeMap>`] implementations.
pub fn extract_type_field<T: Send + Sync + 'static>(map: &mut TypeMap) -> Result<T> {
    map.remove::<T>()
        .ok_or_else(|| ::anyhow::anyhow!("Missing type in TypeMap: {}", std::any::type_name::<T>()))
}
