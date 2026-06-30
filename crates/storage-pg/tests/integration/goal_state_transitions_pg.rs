use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::verbs::goal_write::GoalAuthorshipKind::{External, System, User};
use proxima_core::verbs::goal_write::GoalState::{Abandoned, Achieved, Active, Paused};
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalAuthorshipOrigin, GoalState};
use proxima_core::{Owner, OwnerRefKind};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

async fn insert_goal(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    authorship_kind: GoalAuthorshipKind,
    supersedes: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let goal_id = Uuid::now_v7();
    let request_id = format!("req-{}", Uuid::now_v7());
    let (authorship_origin, authorship_tool_id): (Option<GoalAuthorshipOrigin>, Option<&str>) =
        if matches!(authorship_kind, GoalAuthorshipKind::System) {
            (Some(GoalAuthorshipOrigin::Tool), Some("test/tool"))
        } else {
            (None, None)
        };

    let inserted = sqlx::query_scalar(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version, title, text, payload, state,
             supersedes, authorship_kind, authorship_origin, authorship_tool_id,
             request_id, idempotency_key)
         VALUES ($1, $2, $3, 'test/goal_blob', 1, 'goal', 'goal',
                 convert_to('{}', 'UTF8'), $4, $5, $6, $7, $8, $9,
                 md5($2::text || ':' || $3::text || ':' || $9))
         RETURNING goal_id",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(state)
    .bind(supersedes)
    .bind(authorship_kind)
    .bind(authorship_origin)
    .bind(authorship_tool_id)
    .bind(request_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(inserted)
}

async fn assert_seed_allowed(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    authorship_kind: GoalAuthorshipKind,
) {
    insert_goal(pg, owner, state, authorship_kind, None)
        .await
        .unwrap_or_else(|err| {
            panic!("expected seed {state:?}/{authorship_kind:?} to be allowed: {err}")
        });
}

async fn assert_seed_forbidden(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    authorship_kind: GoalAuthorshipKind,
) {
    let err = insert_goal(pg, owner, state, authorship_kind, None)
        .await
        .expect_err("seed should be forbidden");
    assert!(
        err.to_string().contains("goal:"),
        "unexpected forbidden seed error: {err}"
    );
}

async fn assert_transition_allowed(
    pg: &PgStorage,
    owner: &Owner,
    prior_state: GoalState,
    next_state: GoalState,
    authorship_kind: GoalAuthorshipKind,
) -> Uuid {
    let prior = insert_goal(pg, owner, prior_state, GoalAuthorshipKind::User, None)
        .await
        .expect("prior seed succeeds");
    insert_goal(pg, owner, next_state, authorship_kind, Some(prior))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "expected transition {prior_state:?}->{next_state:?}/{authorship_kind:?} to be allowed: {err}"
            )
        })
}

async fn assert_transition_forbidden(
    pg: &PgStorage,
    owner: &Owner,
    prior: Uuid,
    next_state: GoalState,
    authorship_kind: GoalAuthorshipKind,
) {
    let err = insert_goal(pg, owner, next_state, authorship_kind, Some(prior))
        .await
        .expect_err("transition should be forbidden");
    assert!(
        err.to_string().contains("goal:"),
        "unexpected forbidden transition error: {err}"
    );
}

#[tokio::test]
async fn goal_transition_trigger_enforces_matrix() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();

        assert_seed_allowed(&pg, &owner, Active, User).await;
        assert_seed_allowed(&pg, &owner, Active, System).await;
        for state in [Paused, Achieved, Abandoned] {
            assert_seed_forbidden(&pg, &owner, state, User).await;
            assert_seed_forbidden(&pg, &owner, state, System).await;
        }
        for state in [Active, Paused, Achieved, Abandoned] {
            assert_seed_forbidden(&pg, &owner, state, External).await;
        }

        assert_transition_allowed(&pg, &owner, Active, Active, User).await;
        assert_transition_allowed(&pg, &owner, Active, Active, System).await;
        let paused = assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        assert_transition_allowed(&pg, &owner, Active, Paused, System).await;
        assert_transition_allowed(&pg, &owner, Active, Achieved, User).await;
        assert_transition_allowed(&pg, &owner, Active, Achieved, System).await;
        assert_transition_allowed(&pg, &owner, Active, Abandoned, User).await;
        assert_transition_allowed(&pg, &owner, Active, Abandoned, System).await;
        insert_goal(&pg, &owner, Active, User, Some(paused))
            .await
            .expect("Paused -> Active/User is allowed");
        let paused_system = assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        insert_goal(&pg, &owner, Active, System, Some(paused_system))
            .await
            .expect("Paused -> Active/System is allowed");

        let paused = assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        assert_transition_forbidden(&pg, &owner, paused, Paused, User).await;
        let paused = assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        assert_transition_forbidden(&pg, &owner, paused, Achieved, User).await;
        let paused = assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        assert_transition_forbidden(&pg, &owner, paused, Abandoned, User).await;

        let active = insert_goal(&pg, &owner, Active, User, None).await?;
        assert_transition_forbidden(&pg, &owner, active, Paused, External).await;

        let achieved = assert_transition_allowed(&pg, &owner, Active, Achieved, User).await;
        let abandoned = assert_transition_allowed(&pg, &owner, Active, Abandoned, User).await;
        assert_transition_forbidden(&pg, &owner, achieved, Active, User).await;
        assert_transition_forbidden(&pg, &owner, abandoned, Active, User).await;

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
