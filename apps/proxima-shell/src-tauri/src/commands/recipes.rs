use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use proxima_core::error::ProtocolError;
use proxima_core::{Engine, Owner};
use tauri::State;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct OwnerRecipeTs {
    pub filename: String,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct OwnerRecipesListingTs {
    pub root_path: String,
    pub recipes: Vec<OwnerRecipeTs>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListOwnerRecipesTs {
    pub owner: Owner,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct BundledRecipeTs {
    pub slug: String,
    pub flavor_id: String,
}

#[tauri::command]
#[specta::specta]
pub async fn list_owner_recipes(
    engine: State<'_, Arc<Engine>>,
    req: ListOwnerRecipesTs,
) -> Result<OwnerRecipesListingTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("list_owner_recipes", req_bytes, async move {
        let root = engine.owner_recipes_root(&req.owner);
        read_recipes_at(&root).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_bundled_recipes(
    engine: State<'_, Arc<Engine>>,
) -> Result<Vec<BundledRecipeTs>, ProtocolError> {
    crate::perf::ipc::record("list_bundled_recipes", 0, async move {
        Ok(engine
            .registry()
            .list_bundled_recipes()
            .map(|slug| BundledRecipeTs {
                slug: slug.to_string(),
                flavor_id: slug
                    .split_once('/')
                    .map(|(f, _)| f.to_string())
                    .unwrap_or_default(),
            })
            .collect())
    })
    .await
}

async fn read_recipes_at(root: &Path) -> Result<OwnerRecipesListingTs, ProtocolError> {
    let root_path = root.display().to_string();

    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(rd) => rd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OwnerRecipesListingTs {
                root_path,
                recipes: Vec::new(),
            });
        }
        Err(err) => {
            return Err(ProtocolError::internal(format!(
                "read recipes dir {root_path}: {err}"
            )));
        }
    };

    let mut recipes: Vec<OwnerRecipeTs> = Vec::new();
    loop {
        let next = entries
            .next_entry()
            .await
            .map_err(|err| ProtocolError::internal(format!("readdir: {err}")))?;
        let Some(entry) = next else { break };

        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_yaml(filename) {
            continue;
        }

        let metadata = entry
            .metadata()
            .await
            .map_err(|err| ProtocolError::internal(format!("metadata: {err}")))?;
        if !metadata.is_file() {
            continue;
        }

        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|m| modified_to_iso(m).ok());

        recipes.push(OwnerRecipeTs {
            filename: filename.to_string(),
            modified_at,
        });
    }

    recipes.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(OwnerRecipesListingTs { root_path, recipes })
}

fn is_yaml(filename: &str) -> bool {
    std::path::Path::new(filename)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
}

fn modified_to_iso(modified: SystemTime) -> Result<String, ()> {
    let dt: OffsetDateTime = modified.into();
    dt.format(&Rfc3339).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::fs;

    #[test]
    fn is_yaml_filters_correctly() {
        assert!(is_yaml("default.yaml"));
        assert!(is_yaml("Default.YAML"));
        assert!(is_yaml("foo.yml"));
        assert!(is_yaml("a.yaml"));
        assert!(!is_yaml("recipe.txt"));
        assert!(!is_yaml("yaml"));
        assert!(!is_yaml("foo.yamlish"));
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "proxima-recipes-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4(),
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn missing_dir_returns_empty_listing() {
        let path = tempdir().join("does-not-exist");
        let listing = read_recipes_at(&path).await.unwrap();
        assert_eq!(listing.root_path, path.display().to_string());
        assert!(listing.recipes.is_empty());
    }

    #[tokio::test]
    async fn lists_only_yaml_files_sorted() {
        let dir = tempdir();
        fs::write(dir.join("zeta.yaml"), "z: 1").await.unwrap();
        fs::write(dir.join("alpha.yml"), "a: 1").await.unwrap();
        fs::write(dir.join("beta.yaml"), "b: 1").await.unwrap();
        fs::write(dir.join("notes.txt"), "x").await.unwrap();
        fs::create_dir(dir.join("ignored.yaml")).await.unwrap();

        let listing = read_recipes_at(&dir).await.unwrap();
        let names: Vec<&str> = listing
            .recipes
            .iter()
            .map(|r| r.filename.as_str())
            .collect();
        assert_eq!(names, vec!["alpha.yml", "beta.yaml", "zeta.yaml"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn empty_dir_returns_root_path_and_no_recipes() {
        let dir = tempdir();
        let listing = read_recipes_at(&dir).await.unwrap();
        assert_eq!(listing.root_path, dir.display().to_string());
        assert!(listing.recipes.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
