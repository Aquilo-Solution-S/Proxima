//! `core/list_recipes` — enumerate flavor-bundled and owner recipes.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListRecipesTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListRecipesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecipeItem {
    pub recipe_ref: String,
    /// `"flavor:<flavor_id>"` for bundled; `"owner"` for filesystem.
    pub source: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListRecipesOutput {
    pub recipes: Vec<RecipeItem>,
}

impl McpTool for ListRecipesTool {
    const NAME: &'static str = "core/list_recipes";
    const DESCRIPTION: &'static str =
        "List recipes referenceable as recipe_ref in WakeEntryDraftInput.";
    type Args = ListRecipesArgs;
    type Output = ListRecipesOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListRecipesArgs,
    ) -> BoxFuture<'static, Result<ListRecipesOutput, McpToolError>> {
        Box::pin(async move {
            let mut recipes = Vec::new();
            for slug in ctx.registry.list_bundled_recipes() {
                let flavor = slug.split('/').next().unwrap_or("");
                recipes.push(RecipeItem {
                    recipe_ref: slug.to_string(),
                    source: format!("flavor:{flavor}"),
                });
            }
            if let Some(engine) = ctx.engine() {
                let root = engine.owner_recipes_root(&ctx.owner);
                if let Ok(entries) = std::fs::read_dir(&root) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
                            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                                recipes.push(RecipeItem {
                                    recipe_ref: stem.to_string(),
                                    source: "owner".into(),
                                });
                            }
                        }
                    }
                }
            }
            Ok(ListRecipesOutput { recipes })
        })
    }
}
