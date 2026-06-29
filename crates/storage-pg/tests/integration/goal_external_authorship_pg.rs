//! External authorship cannot seed concrete Goal states.

use crate::common::{create_db, db_url, drop_db};

use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{Owner, OwnerRef, OwnerRefKind, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

async fn insert_external_seed(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    request_id: &str,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = owner_parts(owner);
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, payload, state,
             authorship_kind, request_id, idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 $4, $4, convert_to('{}', 'UTF8'), $5,
                 'External', $6,
                 md5($2::text || ':' || $3::text || ':' || $6))",
    )
    .bind(Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(request_id)
    .bind(state)
    .bind(request_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn external_authorship_cannot_seed_goal_state() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

        for state in [
            GoalState::Active,
            GoalState::Paused,
            GoalState::Achieved,
            GoalState::Abandoned,
        ] {
            let err = insert_external_seed(&pg, &owner, state, state_name(state))
                .await
                .expect_err("External seed must be rejected");
            assert!(
                err.to_string().contains("only User/System may seed"),
                "unexpected error from External/{state:?}: {err}"
            );
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_external_authorship_pg test failed");
}

fn state_name(state: GoalState) -> &'static str {
    match state {
        GoalState::Active => "req-active",
        GoalState::Paused => "req-paused",
        GoalState::Achieved => "req-achieved",
        GoalState::Abandoned => "req-abandoned",
    }
}
