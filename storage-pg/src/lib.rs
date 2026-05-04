//! Postgres `Storage` impl.
//!
//! See docs/07-storage.md and the `Storage` trait in
//! `proxima_core`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalDraft, GoalState, OperatorKind, SystemOrigin,
};
use proxima_core::{Principal, Storage, StorageError, StorageHandle};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub fn into_handle(self) -> StorageHandle {
        Arc::new(self)
    }

    /// Apply all pending migrations under
    /// `storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    /// Helper to extract authorship columns from `GoalAuthorship`.
    #[allow(clippy::type_complexity)]
    fn authorship_columns(
        authorship: &GoalAuthorship,
    ) -> (
        String,
        Option<String>,
        Option<uuid::Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<[u8; 32]>,
    ) {
        match authorship {
            GoalAuthorship::User => (
                "User".to_string(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            GoalAuthorship::System(SystemOrigin::Operator {
                operator_id,
                operator_kind,
                model_id,
                prompt_version,
                personality_id,
                personality_state_hash,
            }) => (
                "System".to_string(),
                Some("Operator".to_string()),
                Some(operator_id.into_inner()),
                None,
                Some(match operator_kind {
                    OperatorKind::AtoGoal => "AtoGoal".to_string(),
                }),
                Some(model_id.as_str().to_string()),
                Some(prompt_version.as_str().to_string()),
                Some(personality_id.as_str().to_string()),
                Some(personality_state_hash.into_inner()),
            ),
            GoalAuthorship::System(SystemOrigin::Tool { tool_id }) => (
                "System".to_string(),
                Some("Tool".to_string()),
                None,
                Some(tool_id.as_str().to_string()),
                None,
                None,
                None,
                None,
                None,
            ),
            GoalAuthorship::External => (
                "External".to_string(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        }
    }

    /// Helper to check if authorship columns match between existing goal and draft.
    async fn check_authorship_match(
        &self,
        tx: &mut sqlx::PgConnection,
        existing_goal_id: uuid::Uuid,
        draft: &GoalDraft,
    ) -> Result<bool, StorageError> {
        #[allow(clippy::type_complexity)]
        let existing_auth: (
            String,
            Option<String>,
            Option<uuid::Uuid>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<Vec<u8>>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT authorship_kind, authorship_origin, authorship_operator_id, \
                         authorship_tool_id, operator_kind, model_id, \
                         prompt_version, personality_id, personality_state_hash \
                 FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(existing_goal_id)
        .fetch_one(tx)
        .await
        .map_err(map_err)?;

        let (
            draft_kind,
            draft_origin,
            draft_op_id,
            draft_tool_id,
            draft_op_kind,
            draft_model,
            draft_prompt,
            draft_personality,
            draft_hash,
        ) = PgStorage::authorship_columns(&draft.authorship);

        let kind_match = existing_auth.0 == draft_kind;
        let origin_match = existing_auth.1 == draft_origin;
        let op_id_match = existing_auth.2 == draft_op_id;
        let tool_id_match = existing_auth.3 == draft_tool_id;
        let op_kind_match = existing_auth.4 == draft_op_kind;
        let model_match = existing_auth.5 == draft_model;
        let prompt_match = existing_auth.6 == draft_prompt;
        let personality_match = existing_auth.8 == draft_personality;
        let hash_match = existing_auth.7.as_deref() == draft_hash.as_ref().map(|h| &h[..]);

        Ok(kind_match
            && origin_match
            && op_id_match
            && tool_id_match
            && op_kind_match
            && model_match
            && prompt_match
            && personality_match
            && hash_match)
    }
}

#[async_trait::async_trait]
impl Storage for PgStorage {
    #[allow(clippy::too_many_lines)]
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        let event_id = draft.event_id();
        let event_id_bytes = event_id.into_inner();

        let (owner_kind, owner_principal_id) = match &draft.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        let owner_org_id = draft.owner.org_id.into_inner();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Replay check.
        let existing: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT memory_id FROM proxima_core.memories WHERE event_id = $1")
                .bind(&event_id_bytes[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_err)?;

        if let Some((memory_id,)) = existing {
            let seq_row: (uuid::Uuid,) = sqlx::query_as(
                "SELECT seq FROM proxima_core.change_event \
                 WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
            )
            .bind(memory_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_err)?;

            tx.commit().await.map_err(map_err)?;

            return Ok(EventIngestOutcome {
                event_id,
                memory_id: proxima_core::MemoryId::new(memory_id),
                change_event_seq: seq_row.0,
                idempotent_replay: true,
            });
        }

        // Generate ids inside the tx; UUIDv7 carries time so seq
        // is monotonic-ish even across concurrent writers.
        let memory_id = uuid::Uuid::now_v7();
        let citation_mapping_id = uuid::Uuid::now_v7();
        let cited_object_id_new = uuid::Uuid::now_v7();
        let change_seq = uuid::Uuid::now_v7();

        // 1. cited_object — idempotent on the UNIQUE.
        let cited_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.cited_objects \
                (cited_object_id, schema_id, owner_principal_kind, \
                 owner_principal_id, owner_org_id, content_hash) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (owner_principal_kind, owner_principal_id, \
                          owner_org_id, schema_id, content_hash) \
             DO UPDATE SET schema_id = EXCLUDED.schema_id \
             RETURNING cited_object_id",
        )
        .bind(cited_object_id_new)
        .bind(draft.cited_object.schema_id.as_str())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&draft.cited_object.content_hash[..])
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

        // 2. source_batch upsert (idempotent on PK). Must come before
        //    event insert due to FK from events.source_batch_id.
        sqlx::query(
            "INSERT INTO proxima_core.source_batches \
                (id, source_id, owner_principal_kind, \
                 owner_principal_id, owner_org_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(draft.source_batch_id.into_inner())
        .bind(draft.source_id.as_str())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 3. event — collision = replay. We already short-circuited
        //    the replay path above, so a conflict here means a race.
        //    Treat as Internal (caller can retry).
        sqlx::query(
            "INSERT INTO proxima_core.events \
                (event_id, source_id, source_batch_id, \
                 owner_principal_kind, owner_principal_id, owner_org_id, \
                 schema_id, schema_version, observed_at, occurred_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&event_id_bytes[..])
        .bind(draft.source_id.as_str())
        .bind(draft.source_batch_id.into_inner())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(draft.schema_id.as_str())
        .bind(draft.schema_version.into_inner().cast_signed())
        .bind(draft.observed_at)
        .bind(draft.occurred_at)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 4. memory (Fact) — citation_mapping_id FK is deferred.
        sqlx::query(
            "INSERT INTO proxima_core.memories \
                (memory_id, owner_principal_kind, owner_principal_id, \
                 owner_org_id, schema_id, event_id, citation_mapping_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(draft.schema_id.as_str())
        .bind(&event_id_bytes[..])
        .bind(citation_mapping_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 5. citation_mapping — memory_id FK is deferred.
        sqlx::query(
            "INSERT INTO proxima_core.citation_mappings \
                (citation_mapping_id, schema_id, memory_id, \
                 cited_object_id, owner_principal_kind, \
                 owner_principal_id, owner_org_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(citation_mapping_id)
        .bind(draft.citation_mapping.schema_id.as_str())
        .bind(memory_id)
        .bind(cited_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 6. change_event (EntityAppend / Fact).
        sqlx::query(
            "INSERT INTO proxima_core.change_event \
                (seq, owner_principal_kind, owner_principal_id, \
                 owner_org_id, kind, entity_kind, \
                 entity_memory_id, entity_schema_id, \
                 entity_schema_version) \
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, $7)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory_id)
        .bind(draft.schema_id.as_str())
        .bind(draft.schema_version.into_inner().cast_signed())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        tx.commit().await.map_err(map_err)?;

        Ok(EventIngestOutcome {
            event_id,
            memory_id: proxima_core::MemoryId::new(memory_id),
            change_event_seq: change_seq,
            idempotent_replay: false,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn write_goal_atomic(
        &self,
        draft: &GoalDraft,
    ) -> Result<proxima_core::verbs::goal_write::GoalWriteOutcome, StorageError> {
        let (owner_kind, owner_principal_id) = match &draft.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        let owner_org_id = draft.owner.org_id.into_inner();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Replay check by (owner, request_id).
        // We need to join with change_event to get the seq.
        let existing: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT g.goal_id, ce.seq \
             FROM proxima_core.goals g \
             JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id \
             WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id) \
               = ($1, $2, $3, $4) \
             ORDER BY ce.seq ASC LIMIT 1",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&draft.request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;

        if let Some((existing_goal_id, existing_seq)) = existing {
            // Compare the existing body with the draft.
            // goals has no schema_version column (sidecar tables encode
            // version implicitly per docs/06); we compare schema_id only.
            let existing_row: (String, String, String, Vec<uuid::Uuid>, Option<uuid::Uuid>) =
                sqlx::query_as(
                    "SELECT schema_id, text, state, \
                             COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]), \
                             supersedes \
                     FROM proxima_core.goals WHERE goal_id = $1",
                )
                .bind(existing_goal_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_err)?;

            let existing_parents: HashSet<uuid::Uuid> = existing_row.3.into_iter().collect();
            let draft_parents: HashSet<uuid::Uuid> = draft
                .parent_goal_ids
                .iter()
                .map(|g| g.into_inner())
                .collect();

            let state_str = match draft.state {
                GoalState::Active => "Active",
                GoalState::Paused => "Paused",
                GoalState::Achieved => "Achieved",
                GoalState::Abandoned => "Abandoned",
            };

            // Check if all fields match.
            let schema_id_match = existing_row.0 == draft.schema_id.as_str();
            let text_match = existing_row.1 == draft.text;
            let state_match = existing_row.2 == state_str;
            let parents_match = existing_parents == draft_parents;
            let supersedes_match = existing_row.4.is_none(); // supersedes must be NULL for write_goal

            // Also need to check authorship fields.
            let authorship_matches = self
                .check_authorship_match(&mut tx, existing_goal_id, draft)
                .await?;

            let body_matches =
                schema_id_match && text_match && state_match && parents_match && supersedes_match;

            if body_matches && authorship_matches {
                tx.commit().await.map_err(map_err)?;
                return Ok(proxima_core::verbs::goal_write::GoalWriteOutcome {
                    goal_id: proxima_core::GoalId::new(existing_goal_id),
                    change_event_seq: existing_seq,
                    idempotent_replay: true,
                });
            }
            return Err(StorageError::ConstraintViolation(format!(
                "idempotency_conflict:{}",
                draft.request_id
            )));
        }

        // Generate ids inside the tx.
        let goal_id = uuid::Uuid::now_v7();
        let change_seq = uuid::Uuid::now_v7();

        // Insert goal with all authorship-discriminator columns.
        let state_str = match draft.state {
            GoalState::Active => "Active",
            GoalState::Paused => "Paused",
            GoalState::Achieved => "Achieved",
            GoalState::Abandoned => "Abandoned",
        };

        let (
            authorship_kind,
            authorship_origin,
            authorship_operator_id,
            authorship_tool_id,
            operator_kind,
            model_id,
            prompt_version,
            personality_id,
            personality_state_hash,
        ) = PgStorage::authorship_columns(&draft.authorship);

        sqlx::query(
            "INSERT INTO proxima_core.goals \
                (goal_id, schema_id, owner_principal_kind, owner_principal_id, \
                 owner_org_id, text, state, supersedes, authorship_kind, \
                 authorship_origin, authorship_operator_id, authorship_tool_id, \
                 operator_kind, model_id, prompt_version, personality_id, \
                 personality_state_hash, request_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(goal_id)
        .bind(draft.schema_id.as_str())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&draft.text)
        .bind(state_str)
        .bind::<Option<uuid::Uuid>>(None) // supersedes is NULL for write_goal
        .bind(authorship_kind)
        .bind(authorship_origin)
        .bind(authorship_operator_id)
        .bind(authorship_tool_id)
        .bind(operator_kind)
        .bind(model_id)
        .bind(prompt_version)
        .bind(personality_id)
        .bind(personality_state_hash)
        .bind(&draft.request_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // Insert goal_parents rows.
        for parent_id in &draft.parent_goal_ids {
            sqlx::query(
                "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id) \
                 VALUES ($1, $2)",
            )
            .bind(goal_id)
            .bind(parent_id.into_inner())
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;
        }

        // Insert change_event.
        sqlx::query(
            "INSERT INTO proxima_core.change_event \
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, \
                 kind, entity_kind, entity_goal_id, entity_schema_id, \
                 entity_schema_version) \
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(goal_id)
        .bind(draft.schema_id.as_str())
        .bind(draft.schema_version.into_inner().cast_signed())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        tx.commit().await.map_err(map_err)?;

        Ok(proxima_core::verbs::goal_write::GoalWriteOutcome {
            goal_id: proxima_core::GoalId::new(goal_id),
            change_event_seq: change_seq,
            idempotent_replay: false,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn supersede_goal_atomic(
        &self,
        prior: proxima_core::GoalId,
        draft: &GoalDraft,
    ) -> Result<proxima_core::verbs::goal_write::GoalWriteOutcome, StorageError> {
        let (owner_kind, owner_principal_id) = match &draft.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        let owner_org_id = draft.owner.org_id.into_inner();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Replay check by (owner, request_id) — same as write_goal.
        let existing: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT g.goal_id, ce.seq \
             FROM proxima_core.goals g \
             JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id \
             WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id) \
               = ($1, $2, $3, $4) \
             ORDER BY ce.seq ASC LIMIT 1",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&draft.request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;

        if let Some((existing_goal_id, existing_seq)) = existing {
            // Compare the existing body with the draft (including supersedes = prior).
            // goals has no schema_version column (sidecar tables encode
            // version implicitly per docs/06).
            let existing_row: (String, String, String, Vec<uuid::Uuid>, Option<uuid::Uuid>) =
                sqlx::query_as(
                    "SELECT schema_id, text, state, \
                             COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]), \
                             supersedes \
                     FROM proxima_core.goals WHERE goal_id = $1",
                )
                .bind(existing_goal_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_err)?;

            let existing_parents: HashSet<uuid::Uuid> = existing_row.3.into_iter().collect();
            let draft_parents: HashSet<uuid::Uuid> = draft
                .parent_goal_ids
                .iter()
                .map(|g| g.into_inner())
                .collect();

            let state_str = match draft.state {
                GoalState::Active => "Active",
                GoalState::Paused => "Paused",
                GoalState::Achieved => "Achieved",
                GoalState::Abandoned => "Abandoned",
            };

            // Check if all fields match (including supersedes = prior).
            let schema_id_match = existing_row.0 == draft.schema_id.as_str();
            let text_match = existing_row.1 == draft.text;
            let state_match = existing_row.2 == state_str;
            let parents_match = existing_parents == draft_parents;
            let supersedes_match = existing_row.4 == Some(prior.into_inner());

            // Also need to check authorship fields.
            let authorship_matches = self
                .check_authorship_match(&mut tx, existing_goal_id, draft)
                .await?;

            let body_matches =
                schema_id_match && text_match && state_match && parents_match && supersedes_match;

            if body_matches && authorship_matches {
                tx.commit().await.map_err(map_err)?;
                return Ok(proxima_core::verbs::goal_write::GoalWriteOutcome {
                    goal_id: proxima_core::GoalId::new(existing_goal_id),
                    change_event_seq: existing_seq,
                    idempotent_replay: true,
                });
            }
            return Err(StorageError::ConstraintViolation(format!(
                "idempotency_conflict:{}",
                draft.request_id
            )));
        }

        // Validate prior exists and belongs to the same owner.
        let prior_row: Option<(String, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT owner_principal_kind, owner_principal_id, owner_org_id \
             FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(prior.into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;

        match prior_row {
            None => return Err(StorageError::NotFound),
            Some((p_kind, p_principal_id, p_org_id)) => {
                if p_kind != owner_kind
                    || p_principal_id != owner_principal_id
                    || p_org_id != owner_org_id
                {
                    return Err(StorageError::ConstraintViolation(
                        "supersede crosses Owner boundary".to_string(),
                    ));
                }
            }
        }

        // Generate ids inside the tx.
        let goal_id = uuid::Uuid::now_v7();
        let change_seq = uuid::Uuid::now_v7();

        // Insert goal with all authorship-discriminator columns.
        let state_str = match draft.state {
            GoalState::Active => "Active",
            GoalState::Paused => "Paused",
            GoalState::Achieved => "Achieved",
            GoalState::Abandoned => "Abandoned",
        };

        let (
            authorship_kind,
            authorship_origin,
            authorship_operator_id,
            authorship_tool_id,
            operator_kind,
            model_id,
            prompt_version,
            personality_id,
            personality_state_hash,
        ) = PgStorage::authorship_columns(&draft.authorship);

        sqlx::query(
            "INSERT INTO proxima_core.goals \
                (goal_id, schema_id, owner_principal_kind, owner_principal_id, \
                 owner_org_id, text, state, supersedes, authorship_kind, \
                 authorship_origin, authorship_operator_id, authorship_tool_id, \
                 operator_kind, model_id, prompt_version, personality_id, \
                 personality_state_hash, request_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(goal_id)
        .bind(draft.schema_id.as_str())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(&draft.text)
        .bind(state_str)
        .bind::<Option<uuid::Uuid>>(Some(prior.into_inner())) // supersedes = prior
        .bind(authorship_kind)
        .bind(authorship_origin)
        .bind(authorship_operator_id)
        .bind(authorship_tool_id)
        .bind(operator_kind)
        .bind(model_id)
        .bind(prompt_version)
        .bind(personality_id)
        .bind(personality_state_hash)
        .bind(&draft.request_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // Insert goal_parents rows.
        for parent_id in &draft.parent_goal_ids {
            sqlx::query(
                "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id) \
                 VALUES ($1, $2)",
            )
            .bind(goal_id)
            .bind(parent_id.into_inner())
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;
        }

        // Insert change_event with supersedes_goal_id.
        sqlx::query(
            "INSERT INTO proxima_core.change_event \
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, \
                 kind, entity_kind, entity_goal_id, entity_schema_id, \
                 entity_schema_version, supersedes_goal_id) \
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7, $8)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(goal_id)
        .bind(draft.schema_id.as_str())
        .bind(draft.schema_version.into_inner().cast_signed())
        .bind(prior.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        tx.commit().await.map_err(map_err)?;

        Ok(proxima_core::verbs::goal_write::GoalWriteOutcome {
            goal_id: proxima_core::GoalId::new(goal_id),
            change_event_seq: change_seq,
            idempotent_replay: false,
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_err(e: sqlx::Error) -> StorageError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        Error::Database(db) if db.is_check_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        _ => StorageError::Internal(e.to_string()),
    }
}
