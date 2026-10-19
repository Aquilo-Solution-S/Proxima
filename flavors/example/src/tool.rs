//! `proxima-example_list_notebooks`: the flavor's one tool, read-only and
//! owner-scoped. A test and tutorial fixture, not a product.

use std::future::Future;
use std::pin::Pin;

use proxima::OwnerScope;
use proxima::flavor::{Tool, ToolCtx, ToolEffect, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contract::LIST_NOTEBOOKS_TOOL;

/// Static SQL: the registry rows of one owner, read on an owner-scoped
/// transaction so owner RLS applies as well as the filter.
const LIST_NOTEBOOKS_SQL: &str = "SELECT notebook_id, name FROM proxima_example.notebooks \
     WHERE owner_id = $1 ORDER BY name, notebook_id";

/// The tool service: the host pool, wrapped as soon as the host hands it
/// out. Every statement runs on an owner-scoped transaction.
#[derive(Debug, Clone)]
pub struct ExampleStore {
    pool: sqlx::PgPool,
}

impl ExampleStore {
    /// Wrap the pool `ProximaHost::clone_pool_for_host` returns.
    #[must_use]
    pub const fn from_host_pool(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// The caller's notebooks, `(notebook_id, name)`, ordered by name.
    ///
    /// # Errors
    ///
    /// Storage errors from the owner-scoped transaction or the read.
    pub async fn list_notebooks(
        &self,
        scope: &OwnerScope,
        owner: Uuid,
    ) -> Result<Vec<(Uuid, String)>, ToolError> {
        let mut tx = proxima::host::begin_owner_transaction(&self.pool, scope)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        let rows: Vec<(Uuid, String)> = sqlx::query_as(LIST_NOTEBOOKS_SQL)
            .bind(owner)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        Ok(rows)
    }
}

/// No arguments: the tool lists the calling owner's notebooks.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListNotebooksArgs {}

/// One notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NotebookItem {
    /// The notebook id (the scope id its notes carry).
    pub notebook_id: Uuid,
    /// The notebook's name.
    pub name: String,
}

/// The caller's notebooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListNotebooksOutput {
    /// Ordered by name.
    pub notebooks: Vec<NotebookItem>,
}

/// `proxima-example_list_notebooks`.
#[derive(Debug)]
pub struct ListNotebooksTool;

impl Tool for ListNotebooksTool {
    const NAME: &'static str = LIST_NOTEBOOKS_TOOL;
    const DESCRIPTION: &'static str =
        "List the current owner's notebooks (example flavor; a test fixture).";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);

    type Args = ListNotebooksArgs;
    type Output = ListNotebooksOutput;

    fn call(
        ctx: ToolCtx,
        _args: ListNotebooksArgs,
    ) -> Pin<Box<dyn Future<Output = Result<ListNotebooksOutput, ToolError>> + Send>> {
        Box::pin(async move {
            let store = ctx.service::<ExampleStore>().ok_or_else(|| {
                ToolError::Other("example flavor requires an ExampleStore tool service".into())
            })?;
            let scope = ctx
                .authz()
                .owner_scope()
                .cloned()
                .ok_or_else(|| ToolError::Other("authenticated owner scope required".into()))?;
            let notebooks = store
                .list_notebooks(&scope, ctx.owner().into_inner())
                .await?
                .into_iter()
                .map(|(notebook_id, name)| NotebookItem { notebook_id, name })
                .collect();
            Ok(ListNotebooksOutput { notebooks })
        })
    }
}
