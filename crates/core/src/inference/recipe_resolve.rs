//! Resolve wake-entry recipe refs to filesystem paths.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::FlavorRegistryFrozen;

#[derive(Debug, Error)]
pub enum RecipeResolveError {
    #[error("malformed recipe_ref: `{0}`")]
    Malformed(String),
    #[error("bundled recipe `{0}` not registered")]
    BundledNotRegistered(String),
    #[error("user recipe file `{0}` does not exist")]
    UserMissing(PathBuf),
}

pub fn resolve_recipe_ref(
    recipe_ref: &str,
    owner_recipes_root: &Path,
    registry: &FlavorRegistryFrozen,
) -> Result<PathBuf, RecipeResolveError> {
    if let Some(slug) = recipe_ref.strip_prefix("bundled:") {
        return registry
            .bundled_recipe_path(slug)
            .ok_or_else(|| RecipeResolveError::BundledNotRegistered(slug.to_string()));
    }

    if let Some(filename) = recipe_ref.strip_prefix("user:") {
        let path = owner_recipes_root.join(filename);
        return if path.exists() {
            Ok(path)
        } else {
            Err(RecipeResolveError::UserMissing(path))
        };
    }

    Err(RecipeResolveError::Malformed(recipe_ref.to_string()))
}
