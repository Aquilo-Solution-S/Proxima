//! Host-path coverage for Goal collaboration and exact-version fork actions.

use super::goal_evidence_kinds::Env;
use super::goal_evidence_kinds::handle_of;
use proxiima::CoreMcpErrorKind;
use proxiima_core::{OwnerId, UserId};
use uuid::Uuid;

type DerivationPayload = (
    String,
    String,
    Vec<String>,
    Option<String>,
    Vec<Uuid>,
    String,
    String,
    String,
);

async fn derivation_payload(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<DerivationPayload, sqlx::Error> {
    sqlx::query_as(
        "SELECT title, body, tags, idempotency_key, source_memory_ids, model_id, client_name, client_version
           FROM proxiima_core.agent_derivation_v1 WHERE t = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

async fn memory_origins(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<(Vec<Uuid>, Vec<Uuid>), sqlx::Error> {
    sqlx::query_as("SELECT origins, refs FROM proxiima_core.memory WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "MCP acceptance keeps grant provenance, publication, work, and fork receipts together"
)]
async fn goal_collaboration_roundtrip_uses_stable_goal_handle_and_typed_fork() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_goal_collaboration_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("collaboration fact").await?;
        let abstraction = env.derive("collaboration abstraction", &fact).await?;
        let perspective = env
            .interpret("collaboration perspective", &abstraction)
            .await?;
        let goal = env
            .set_goal(&[&fact], &perspective, "collaboration goal")
            .await?;
        let goal_handle = goal["goal_handle"].as_str().ok_or("stable goal handle")?;
        let exact_goal = goal["handle"].as_str().ok_or("exact goal handle")?;
        assert_ne!(
            goal_handle, exact_goal,
            "series and revision handles are distinct"
        );

        let subject = uuid::Uuid::now_v7().to_string();
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": goal_handle,
                "subject": subject, "read_ceiling": "Perspective",
                "write_ceiling": "Perspective", "manage": false,
            }),
        )
        .await?;
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": goal_handle,
                "subject": subject, "read_ceiling": "Perspective",
                "write_ceiling": "None", "manage": false,
            }),
        )
        .await?;
        let admin = env.admin().await?;
        let granted_by: Uuid = sqlx::query_scalar(
            "SELECT granted_by FROM proxiima_core.goal_access_grant WHERE goal_handle = $1 AND subject_id = $2",
        )
        .bind(Uuid::parse_str(goal_handle)?).bind(Uuid::parse_str(&subject)?).fetch_one(&admin).await?;
        assert_eq!(granted_by, env.owner.into_inner());
        admin.close().await;
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "publish", "goal_handle": goal_handle,
                "memory_id": perspective, "permitted_use": "Read",
            }),
        )
        .await?;

        let interpreted = env
            .call(
                "core_interpret",
                serde_json::json!({
                    "claim": "the working goal changes the perspective",
                    "confidence": 80,
                    "subjects": [abstraction],
                    "working_goal": goal_handle,
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("derive hidden revision: {error}")))?;
        assert!(interpreted["handle"].as_str().is_some());

        let forked = env
            .call(
                "core_fork",
                serde_json::json!({
                    "source": perspective,
                    "owner": env.owner.into_inner(),
                    "idempotency_key": "collaboration-fork",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("second hidden fork: {error}")))?;
        assert!(forked["memory"].as_str().is_some());
        assert!(forked["series"].as_str().is_some());
        assert_eq!(forked["forked_from"], perspective);

        env.call(
            "core_goal",
            serde_json::json!({
                "action": "unpublish", "goal_handle": goal_handle,
                "memory_id": perspective,
            }),
        )
        .await?;
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "revoke_access", "goal_handle": goal_handle,
                "subject": subject,
            }),
        )
        .await?;
        let _ = handle_of(&goal)?;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("Goal collaboration host roundtrip failed");
}

#[tokio::test]
async fn keyless_goal_scoped_derive_and_interpret_are_distinct_per_goal() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_goal_keyless_planner_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("shared keyless fact").await?;
        let motivation = env.interpret("shared keyless motivation", &fact).await?;
        let goal_one = env
            .set_goal(&[&fact], &motivation, "keyless goal one")
            .await?;
        let goal_two = env
            .set_goal(&[&fact], &motivation, "keyless goal two")
            .await?;
        let goal_one_handle = goal_one["goal_handle"].as_str().ok_or("goal one handle")?;
        let goal_two_handle = goal_two["goal_handle"].as_str().ok_or("goal two handle")?;

        let abstraction_one = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction", "title": "same keyless abstraction",
                    "body": "same keyless abstraction body", "tags": [],
                    "source_handles": [fact.clone()], "model_id": "test-model",
                    "working_goal": goal_one_handle,
                }),
            )
            .await?;
        let abstraction_two = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction", "title": "same keyless abstraction",
                    "body": "same keyless abstraction body", "tags": [],
                    "source_handles": [fact], "model_id": "test-model",
                    "working_goal": goal_two_handle,
                }),
            )
            .await?;
        let abstraction_one_id = handle_of(&abstraction_one)?;
        let abstraction_two_id = handle_of(&abstraction_two)?;
        assert_ne!(abstraction_one_id, abstraction_two_id);

        let perspective_one = env
            .call(
                "core_interpret",
                serde_json::json!({
                    "claim": "same keyless perspective", "confidence": 80,
                    "subjects": [abstraction_one_id.clone()], "working_goal": goal_one_handle,
                }),
            )
            .await?;
        let perspective_two = env
            .call(
                "core_interpret",
                serde_json::json!({
                    "claim": "same keyless perspective", "confidence": 80,
                    "subjects": [abstraction_one_id], "working_goal": goal_two_handle,
                }),
            )
            .await?;
        assert_ne!(handle_of(&perspective_one)?, handle_of(&perspective_two)?);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("keyless Goal-scoped authoring must remain distinct per Goal");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "MCP acceptance keeps keyless and explicit guidance replay evidence together"
)]
async fn keyless_derive_guidance_is_part_of_identity_and_exact_replays() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_goal_derive_guidance_identity")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("guidance identity fact").await?;
        let source = env.derive("guidance identity source", &fact).await?;
        let motivation = env
            .interpret("guidance identity motivation", &source)
            .await?;
        let goal_one = env
            .set_goal(&[&fact], &motivation, "guidance identity one")
            .await?;
        let goal_two = env
            .set_goal(&[&fact], &motivation, "guidance identity two")
            .await?;
        let goal_one_id = goal_one["handle"].as_str().ok_or("exact Goal one")?;
        let goal_two_id = goal_two["handle"].as_str().ok_or("exact Goal two")?;

        let first = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "guided identity",
                    "body": "same derived body", "tags": [],
                    "source_handles": [source.clone()], "model_id": "test-model",
                    "guided_by": [goal_one_id],
                }),
            )
            .await?;
        let second = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "guided identity",
                    "body": "same derived body", "tags": [],
                    "source_handles": [source.clone()], "model_id": "test-model",
                    "guided_by": [goal_two_id],
                }),
            )
            .await?;
        assert_eq!(first["idempotent_replay"], false);
        assert_eq!(second["idempotent_replay"], false);
        assert_ne!(handle_of(&first)?, handle_of(&second)?);

        let admin = env.admin().await?;
        let stored_guidance: Vec<Uuid> =
            sqlx::query_scalar("SELECT guided_by FROM proxiima_core.memory WHERE t = $1")
                .bind(Uuid::parse_str(&handle_of(&second)?)?)
                .fetch_one(&admin)
                .await?;
        assert_eq!(stored_guidance, vec![Uuid::parse_str(goal_two_id)?]);
        admin.close().await;

        let replay = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "guided identity",
                    "body": "same derived body", "tags": [],
                    "source_handles": [source.clone()], "model_id": "test-model",
                    "guided_by": [goal_two_id],
                }),
            )
            .await?;
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(handle_of(&replay)?, handle_of(&second)?);

        let explicit_first = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "explicit guided identity",
                    "body": "same explicit body", "tags": [],
                    "source_handles": [source.clone()], "model_id": "test-model",
                    "idempotency_key": "explicit-guidance-key",
                    "guided_by": [goal_one_id],
                }),
            )
            .await?;
        assert_eq!(explicit_first["idempotent_replay"], false);
        let explicit_err = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "explicit guided identity",
                    "body": "same explicit body", "tags": [],
                    "source_handles": [source], "model_id": "test-model",
                    "idempotency_key": "explicit-guidance-key",
                    "guided_by": [goal_two_id],
                }),
            )
            .await
            .expect_err("same explicit key with changed guidance must conflict");
        assert!(
            explicit_err.to_string().contains("IdempotencyConflict"),
            "wrong explicit-guidance error: {explicit_err}"
        );
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("keyless core_derive guidance identity failed");
}

#[tokio::test]
async fn agent_derivation_abstraction_and_perspective_forks_copy_payloads() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_agent_derivation_fork_copy")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("fork grounding fact").await?;
        let abstraction = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction", "title": "fork abstraction", "body": "fork abstraction body",
                    "tags": ["fork"], "source_handles": [fact], "model_id": "test-model",
                    "idempotency_key": "original-abstraction-key",
                }),
            )
            .await?;
        let abstraction_id = handle_of(&abstraction)?;
        let perspective = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "fork perspective", "body": "fork perspective body",
                    "tags": ["fork"], "source_handles": [abstraction_id], "model_id": "test-model",
                    "idempotency_key": "original-perspective-key",
                }),
            )
            .await?;
        let perspective_id = handle_of(&perspective)?;

        let abstraction_fork = env
            .call(
                "core_fork",
                serde_json::json!({"source": abstraction_id.clone(), "owner": env.owner.into_inner(), "idempotency_key": "abstraction-fork-key"}),
            )
            .await?;
        let perspective_fork = env
            .call(
                "core_fork",
                serde_json::json!({"source": perspective_id.clone(), "owner": env.owner.into_inner(), "idempotency_key": "perspective-fork-key"}),
            )
            .await?;
        let abstraction_fork_id = abstraction_fork["memory"].as_str().ok_or("abstraction fork")?;
        let perspective_fork_id = perspective_fork["memory"].as_str().ok_or("perspective fork")?;
        assert_ne!(abstraction_fork_id, abstraction_id);
        assert_ne!(perspective_fork_id, perspective_id);
        assert_eq!(abstraction_fork["forked_from"], abstraction_id);
        assert_eq!(perspective_fork["forked_from"], perspective_id);
        env.read_as(
            env.owner,
            &format!("proxiima://memory/{}?version=head", abstraction_fork["series"].as_str().ok_or("abstraction series")?),
        )
        .await?;
        env.read_as(
            env.owner,
            &format!("proxiima://memory/{}?version=head", perspective_fork["series"].as_str().ok_or("perspective series")?),
        )
        .await?;

        let admin = env.admin().await?;
        let abstraction_payload = derivation_payload(&admin, Uuid::parse_str(&abstraction_id)?).await?;
        let abstraction_fork_payload =
            derivation_payload(&admin, Uuid::parse_str(abstraction_fork_id)?).await?;
        let perspective_payload = derivation_payload(&admin, Uuid::parse_str(&perspective_id)?).await?;
        let perspective_fork_payload =
            derivation_payload(&admin, Uuid::parse_str(perspective_fork_id)?).await?;
        assert_eq!(abstraction_payload, abstraction_fork_payload);
        assert_eq!(perspective_payload, perspective_fork_payload);
        let abstraction_origins = memory_origins(&admin, Uuid::parse_str(&abstraction_id)?).await?;
        let abstraction_fork_origins =
            memory_origins(&admin, Uuid::parse_str(abstraction_fork_id)?).await?;
        let perspective_origins = memory_origins(&admin, Uuid::parse_str(&perspective_id)?).await?;
        let perspective_fork_origins =
            memory_origins(&admin, Uuid::parse_str(perspective_fork_id)?).await?;
        assert_eq!(abstraction_origins, abstraction_fork_origins);
        assert_eq!(perspective_origins, perspective_fork_origins);
        admin.close().await;

        let abstraction_replay = env
            .call("core_fork", serde_json::json!({"source": abstraction_id.clone(), "owner": env.owner.into_inner(), "idempotency_key": "abstraction-fork-key"}))
            .await?;
        let perspective_replay = env
            .call("core_fork", serde_json::json!({"source": perspective_id.clone(), "owner": env.owner.into_inner(), "idempotency_key": "perspective-fork-key"}))
            .await?;
        assert_eq!(abstraction_replay["memory"], abstraction_fork["memory"]);
        assert_eq!(perspective_replay["memory"], perspective_fork["memory"]);
        assert_eq!(abstraction_replay["idempotent_replay"], true);
        assert_eq!(perspective_replay["idempotent_replay"], true);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("AgentDerivation fork copy failed");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "MCP acceptance covers retained grounding for both revision tools"
)]
async fn goal_guest_revises_forked_perspective_without_hidden_ancestors() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_goal_hidden_revision_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("hidden revision fact").await?;
        let abstraction = env.derive("hidden revision abstraction", &fact).await?;
        let perspective = env
            .call(
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "published source",
                    "body": "published source body", "tags": [],
                    "source_handles": [abstraction.clone()], "model_id": "test-model",
                    "idempotency_key": "hidden-published-perspective",
                }),
            )
            .await?;
        let perspective_id = handle_of(&perspective)?;
        let interpretation_source = env
            .interpret("published interpretation", &abstraction)
            .await?;
        let goal = env
            .set_goal(&[&fact], &perspective_id, "hidden revision goal")
            .await?;
        let goal_handle = goal["goal_handle"].as_str().ok_or("Goal handle")?;
        let guest = OwnerId::from(UserId::new(Uuid::now_v7()));
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": goal_handle,
                "subject": guest.into_inner(), "read_ceiling": "Goal",
                "write_ceiling": "Perspective", "manage": false,
            }),
        )
        .await?;
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "publish", "goal_handle": goal_handle,
                "memory_id": interpretation_source, "permitted_use": "Fork",
            }),
        )
        .await?;
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "publish", "goal_handle": goal_handle,
                "memory_id": perspective_id, "permitted_use": "Fork",
            }),
        )
        .await?;

        let fork = env
            .call_as(
                guest,
                "core_fork",
                serde_json::json!({
                    "source": interpretation_source, "working_goal": goal_handle,
                    "idempotency_key": "hidden-revision-interpret-fork",
                }),
            )
            .await?;
        let fork_id = fork["memory"].as_str().ok_or("forked memory")?.to_owned();
        assert!(
            env.read_as(guest, &format!("proxiima://memory/{fact}"))
                .await
                .is_err()
        );
        assert!(
            env.read_as(guest, &format!("proxiima://memory/{abstraction}"))
                .await
                .is_err()
        );
        let interpreted = env
            .call_as(
                guest,
                "core_interpret",
                serde_json::json!({
                    "claim": "interpreted revision", "confidence": 80,
                    "subjects": [abstraction.clone()], "working_goal": goal_handle,
                    "supersedes": fork_id,
                }),
            )
            .await
            .map_err(|error| {
                std::io::Error::other(format!("interpret hidden revision: {error}"))
            })?;
        let interpreted_id = handle_of(&interpreted)?;

        let fork_two = env
            .call_as(
                guest,
                "core_fork",
                serde_json::json!({
                    "source": perspective_id, "working_goal": goal_handle,
                    "idempotency_key": "hidden-revision-second-fork",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("second hidden fork: {error}")))?;
        let fork_two_id = fork_two["memory"]
            .as_str()
            .ok_or("second forked memory")?
            .to_owned();
        let derived = env
            .call_as(
                guest,
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "derived revision",
                    "body": "derived revision body", "tags": [],
                    "source_handles": [abstraction], "model_id": "test-model",
                    "working_goal": goal_handle, "supersedes": fork_two_id,
                    "idempotency_key": "hidden-revision-derive",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("derive hidden revision: {error}")))?;
        let derived_id = handle_of(&derived)?;

        let new_private_abstraction = env.derive("new unreadable pin", &fact).await?;
        let rejected_fork = env
            .call_as(
                guest,
                "core_fork",
                serde_json::json!({
                    "source": interpretation_source, "working_goal": goal_handle,
                    "idempotency_key": "hidden-revision-rejected-fork",
                }),
            )
            .await?;
        let rejected_prior = rejected_fork["memory"]
            .as_str()
            .ok_or("rejected revision fork")?
            .to_owned();
        let admin = env.admin().await?;
        let before_rejected: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.memory WHERE handle = (SELECT handle FROM proxiima_core.memory WHERE t = $1)",
        )
        .bind(Uuid::parse_str(&rejected_prior)?)
        .fetch_one(&admin)
        .await?;
        admin.close().await;
        let rejected = env
            .call_as(
                guest,
                "core_interpret",
                serde_json::json!({
                    "claim": "must refuse new hidden pin", "confidence": 80,
                    "subjects": [new_private_abstraction], "working_goal": goal_handle,
                    "supersedes": rejected_prior,
                }),
            )
            .await
            .expect_err("new unreadable revision pin must be refused");
        assert!(rejected.to_string().contains("not found"));
        let admin = env.admin().await?;
        let after_rejected: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.memory WHERE handle = (SELECT handle FROM proxiima_core.memory WHERE t = $1)",
        )
        .bind(Uuid::parse_str(&rejected_prior)?)
        .fetch_one(&admin)
        .await?;
        assert_eq!(after_rejected, before_rejected);
        admin.close().await;

        let bob = OwnerId::from(UserId::new(Uuid::now_v7()));
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": goal_handle,
                "subject": bob.into_inner(), "read_ceiling": "Goal",
                "write_ceiling": "Perspective", "manage": false,
            }),
        )
        .await?;
        let bob_interpret_fork = env
            .call_as_with_role(
                bob,
                proxiima_core::Role::admin(),
                "core_fork",
                serde_json::json!({
                    "source": interpretation_source, "owner": bob.into_inner(),
                    "idempotency_key": "bob-hidden-interpret-fork",
                }),
            )
            .await?;
        let bob_derive_fork = env
            .call_as_with_role(
                bob,
                proxiima_core::Role::admin(),
                "core_fork",
                serde_json::json!({
                    "source": perspective_id, "owner": bob.into_inner(),
                    "idempotency_key": "bob-hidden-derive-fork",
                }),
            )
            .await?;
        let bob_interpret = env
            .call_as_with_role(
                bob,
                proxiima_core::Role::admin(),
                "core_interpret",
                serde_json::json!({
                    "claim": "Bob adapts hidden interpretation", "confidence": 80,
                    "subjects": [abstraction.clone()],
                    "supersedes": bob_interpret_fork["memory"],
                }),
            )
            .await?;
        let bob_derive = env
            .call_as_with_role(
                bob,
                proxiima_core::Role::admin(),
                "core_derive",
                serde_json::json!({
                    "kind": "Perspective", "title": "Bob adapts hidden derive",
                    "body": "Bob's adapted perspective", "tags": [],
                    "source_handles": [abstraction], "model_id": "test-model",
                    "supersedes": bob_derive_fork["memory"],
                    "idempotency_key": "bob-hidden-derive",
                }),
            )
            .await?;
        assert!(bob_interpret["handle"].as_str().is_some());
        assert!(bob_derive["handle"].as_str().is_some());
        assert!(env.read_as(bob, &format!("proxiima://memory/{fact}")).await.is_err());
        assert!(env.read_as(bob, &format!("proxiima://memory/{abstraction}")).await.is_err());

        let admin = env.admin().await?;
        for (new_id, prior_id, anchor) in [
            (interpreted_id, fork_id, interpretation_source),
            (derived_id, fork_two_id, perspective_id),
        ] {
            let (new_handle, new_anchor): (Uuid, Option<Uuid>) =
                sqlx::query_as("SELECT handle, forked_from FROM proxiima_core.memory WHERE t = $1")
                    .bind(Uuid::parse_str(&new_id)?)
                    .fetch_one(&admin)
                    .await?;
            let (prior_handle, prior_anchor): (Uuid, Option<Uuid>) =
                sqlx::query_as("SELECT handle, forked_from FROM proxiima_core.memory WHERE t = $1")
                    .bind(Uuid::parse_str(&prior_id)?)
                    .fetch_one(&admin)
                    .await?;
            assert_eq!(new_handle, prior_handle);
            assert_eq!(new_anchor, prior_anchor);
            assert_eq!(new_anchor, Some(Uuid::parse_str(&anchor)?));
        }
        admin.close().await;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("Goal guest hidden-ancestor revisions failed");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "MCP acceptance keeps grant, publication, edit, and fork receipts together"
)]
async fn goal_g4_guest_can_edit_exact_readable_goal_without_source_membership() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let env = Env::new("proxiima_goal_g4_guest_edit")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let evidence = handle_of(
            &env.call(
                "core_remember",
                serde_json::json!({
                    "title": "source evidence",
                    "body": "private source body",
                    "tags": [],
                }),
            )
            .await?,
        )?;
        let target = env.interpret("goal motivation", &evidence).await?;
        let created = env.set_goal(&[&evidence], &target, "owner title").await?;
        let goal = handle_of(&created)?;

        let guest = OwnerId::from(UserId::new(Uuid::now_v7()));
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": created["goal_handle"],
                "subject": guest.into_inner(), "read_ceiling": "Goal",
                "write_ceiling": "Goal", "manage": false,
            }),
        )
        .await?;

        let guest_goal = env
            .read_as(guest, &format!("proxiima://goal/{goal}"))
            .await
            .map_err(|error| std::io::Error::other(format!("guest Goal read: {error}")))?;
        assert_eq!(guest_goal["title"], "owner title");
        assert_eq!(guest_goal["evidence"], serde_json::json!([evidence]));
        assert!(!guest_goal.to_string().contains("private source body"));
        assert!(
            env.read_as(guest, &format!("proxiima://memory/{evidence}"))
                .await
                .is_err(),
            "Goal access keeps private source evidence as a pointer"
        );

        let modified = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "modify", "goal": goal, "schema_id": "core/simple-text-v1",
                    "title": "guest title", "text": "guest text", "body": {},
                    "idempotency_key": "g4-guest-modify",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("guest modify: {error}")))?;
        let paused = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "transition", "goal": modified["handle"],
                    "transition": "pause", "pending": "keep",
                    "idempotency_key": "g4-guest-pause",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("guest pause: {error}")))?;
        let resumed = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "transition", "goal": paused["handle"],
                    "transition": "resume", "idempotency_key": "g4-guest-resume",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("guest resume: {error}")))?;
        let viewer = OwnerId::from(UserId::new(Uuid::now_v7()));
        env.call(
            "core_goal",
            serde_json::json!({
                "action": "grant_access", "goal_handle": created["goal_handle"],
                "subject": viewer.into_inner(), "read_ceiling": "Goal",
                "write_ceiling": "None", "manage": false,
            }),
        )
        .await?;
        let viewer_err = env
            .call_as(
                viewer,
                "core_goal",
                serde_json::json!({
                    "action": "modify", "goal": resumed["handle"],
                    "schema_id": "core/simple-text-v1", "title": "viewer title",
                    "text": "viewer text", "body": {}, "idempotency_key": "g4-viewer-modify",
                }),
            )
            .await
            .expect_err("Goal read-only viewer cannot edit");
        assert_eq!(viewer_err.kind(), CoreMcpErrorKind::InvalidRequest);
        assert!(viewer_err.to_string().contains("Goal-level write required"));

        let private_evidence_err = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "mark_achieved", "goal": resumed["handle"],
                    "evidence": [evidence], "idempotency_key": "g4-private-evidence",
                }),
            )
            .await
            .expect_err("Goal write does not admit a new private evidence statement");
        assert!(private_evidence_err.to_string().contains("entry not found"));

        let achieved = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "mark_achieved", "goal": resumed["handle"],
                    "evidence": [], "idempotency_key": "g4-guest-achieve",
                }),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("guest achieve: {error}")))?;
        let final_goal = env
            .read_as(
                guest,
                &format!(
                    "proxiima://goal/{}",
                    achieved["handle"].as_str().expect("achieved Goal id")
                ),
            )
            .await
            .map_err(|error| std::io::Error::other(format!("guest final read: {error}")))?;
        assert_eq!(final_goal["title"], "guest title");
        assert_eq!(final_goal["state"], "Achieved");

        env.call(
            "core_goal",
            serde_json::json!({
                "action": "revoke_access", "goal_handle": created["goal_handle"],
                "subject": guest.into_inner(),
            }),
        )
        .await?;
        let revoked_replay = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "modify", "goal": goal, "schema_id": "core/simple-text-v1",
                    "title": "guest title", "text": "guest text", "body": {},
                    "idempotency_key": "g4-guest-modify",
                }),
            )
            .await
            .expect_err("revoked guest replay is rejected");
        assert_eq!(revoked_replay.kind(), CoreMcpErrorKind::NotFound);
        let revoked_write = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "transition", "goal": achieved["handle"],
                    "transition": "resume", "idempotency_key": "g4-revoked-write",
                }),
            )
            .await
            .expect_err("revoked guest write is rejected");
        assert_eq!(revoked_write.kind(), CoreMcpErrorKind::NotFound);
        let unknown = env
            .call_as(
                guest,
                "core_goal",
                serde_json::json!({
                    "action": "transition", "goal": uuid::Uuid::now_v7(),
                    "transition": "resume", "idempotency_key": "g4-unknown-write",
                }),
            )
            .await
            .expect_err("unknown Goal is hidden identically");
        assert_eq!(unknown.kind(), revoked_write.kind());
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("G4 guest Goal collaboration failed");
}
