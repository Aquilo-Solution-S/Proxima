mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalAuthorshipOrigin, GoalState};
use proxima_core::{Owner, OwnerPrincipalKind, Principal};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

async fn insert_goal(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    authorship_kind: GoalAuthorshipKind,
    supersedes: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let goal_id = Uuid::now_v7();
    let request_id = format!("req-{}", Uuid::now_v7());
    let (authorship_origin, authorship_tool_id): (Option<GoalAuthorshipOrigin>, Option<&str>) =
        if matches!(authorship_kind, GoalAuthorshipKind::System) {
            (Some(GoalAuthorshipOrigin::Tool), Some("test/tool"))
        } else {
            (None, None)
        };

    sqlx::query_scalar(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, state, supersedes,
             authorship_kind, authorship_origin, authorship_tool_id, request_id)
         VALUES ($1, 'test/goal_blob', 1, $2, $3, $4, 'goal', 'goal', $5, $6,
                 $7, $8, $9, $10)
         RETURNING goal_id",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(state)
    .bind(supersedes)
    .bind(authorship_kind)
    .bind(authorship_origin)
    .bind(authorship_tool_id)
    .bind(request_id)
    .fetch_one(pg.pool())
    .await
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
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        use GoalAuthorshipKind::{External, System, User};
        use GoalState::{Abandoned, Achieved, Active, Paused, Proposed, Rejected};

        assert_seed_allowed(&pg, &owner, Proposed, External).await;
        assert_seed_allowed(&pg, &owner, Active, User).await;
        assert_seed_allowed(&pg, &owner, Achieved, System).await;
        assert_seed_forbidden(&pg, &owner, Active, External).await;
        assert_seed_forbidden(&pg, &owner, Rejected, User).await;
        assert_seed_forbidden(&pg, &owner, Rejected, External).await;

        assert_transition_allowed(&pg, &owner, Proposed, Active, User).await;
        let rejected = assert_transition_allowed(&pg, &owner, Proposed, Rejected, User).await;
        assert_transition_allowed(&pg, &owner, Active, Active, User).await;
        assert_transition_allowed(&pg, &owner, Active, Paused, User).await;
        assert_transition_allowed(&pg, &owner, Active, Achieved, User).await;
        assert_transition_allowed(&pg, &owner, Active, Achieved, System).await;
        assert_transition_allowed(&pg, &owner, Active, Abandoned, User).await;
        assert_transition_allowed(&pg, &owner, Paused, Active, User).await;
        assert_transition_allowed(&pg, &owner, Paused, Abandoned, User).await;

        let proposed = insert_goal(&pg, &owner, Proposed, External, None).await?;
        assert_transition_forbidden(&pg, &owner, proposed, Active, External).await;
        assert_transition_forbidden(&pg, &owner, proposed, Rejected, External).await;

        let achieved = insert_goal(&pg, &owner, Achieved, User, None).await?;
        let abandoned = insert_goal(&pg, &owner, Abandoned, User, None).await?;
        assert_transition_forbidden(&pg, &owner, achieved, Active, User).await;
        assert_transition_forbidden(&pg, &owner, abandoned, Active, User).await;
        assert_transition_forbidden(&pg, &owner, rejected, Active, User).await;

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
