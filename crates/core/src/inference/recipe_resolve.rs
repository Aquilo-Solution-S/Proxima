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

    if let Some(path) = registry.bundled_recipe_path(recipe_ref) {
        return Ok(path);
    }

    Err(RecipeResolveError::Malformed(recipe_ref.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlavorRegistry;

    #[test]
    fn bare_registered_bundled_slug_resolves_for_legacy_wake_entries() {
        let mut registry = FlavorRegistry::new();
        let path = PathBuf::from("/tmp/proxima-code/recipes/plan_execution_requests.yaml");
        registry.add_bundled_recipe(
            "proxima-code/plan_execution_requests".to_string(),
            path.clone(),
        );
        let frozen = registry.freeze();

        let resolved = resolve_recipe_ref(
            "proxima-code/plan_execution_requests",
            Path::new("/tmp/owner-recipes"),
            &frozen,
        )
        .expect("legacy bare bundled recipe slug resolves");

        assert_eq!(resolved, path);
    }
}
