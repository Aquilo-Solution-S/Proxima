//! Storage materializes the Lean mount/union lattice from a forest's written rows.
#![allow(clippy::too_many_lines)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::Duration;

use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseTarget, OwnerSurfaces};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{AccessCeiling, GroupId, OwnerId, Relation, Role, UserId};
use proxima_pg_testkit::{DbGuard, create_db, db_url, split_role_urls};
use proxima_storage_pg::{
    PgStorage, add_group_member, begin_migration_transaction, core_migrator, group_role,
    register_group, remove_group_member, remove_group_member_relation,
    replace_group_member_relation,
};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type Roles = BTreeMap<(Uuid, Uuid), Role>;
const RELATIONS: [Relation; 4] = [
    Relation::Admin,
    Relation::Editor,
    Relation::Viewer,
    Relation::Ingest,
];

#[path = "nested_groups/write_policies.rs"]
mod write_policies;

struct Harness {
    pool: PgPool,
    platform: PgPool,
    runtime: PgPool,
    database: DbGuard,
}

impl Harness {
    async fn new(last_migration: Option<i64>) -> TestResult<Self> {
        let name = format!("proxima_test_nested_{}", Uuid::now_v7().simple());
        create_db(&name).await?;
        let database = DbGuard::adopt(name.clone());
        let (runtime_url, platform_url) = split_role_urls(&name).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let runtime = PgPool::connect(&runtime_url).await?;
        let pool = PgPool::connect(&db_url(&name)).await?;
        let mut migrator = core_migrator();
        if let Some(last) = last_migration {
            migrator.migrations = Cow::Owned(
                migrator
                    .iter()
                    .filter(|m| m.version <= last)
                    .cloned()
                    .collect(),
            );
        }
        let mut conn = platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut conn).await?;
        migrator.run(tx.as_mut()).await?;
        tx.commit().await?;
        drop(conn);
        Ok(Self {
            pool,
            platform,
            runtime,
            database,
        })
    }

    async fn groups(&self, count: usize) -> TestResult<Vec<GroupId>> {
        let mut tx = self.pool.begin().await?;
        let mut groups = Vec::new();
        for _ in 0..count {
            let group = GroupId::new(Uuid::now_v7());
            // Seed the released registry shape too, before the v0.0.31 gateway exists.
            sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
                .bind(group.into_inner())
                .execute(&mut *tx)
                .await?;
            groups.push(group);
        }
        tx.commit().await?;
        Ok(groups)
    }

    async fn add(&self, parent: GroupId, member: OwnerId, relation: Relation) -> TestResult {
        let mut tx = self.pool.begin().await?;
        add_group_member(&mut tx, parent, member, relation).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn remove(&self, parent: GroupId, member: OwnerId) -> TestResult {
        let mut tx = self.pool.begin().await?;
        remove_group_member(&mut tx, parent, member).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn replace(
        &self,
        parent: GroupId,
        member: OwnerId,
        from: Relation,
        to: Relation,
    ) -> TestResult {
        let mut tx = self.pool.begin().await?;
        replace_group_member_relation(&mut tx, parent, member, from, to).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn assert_projection(&self) -> TestResult {
        let expected = reference_climb(&self.pool).await?;
        let rows: Vec<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
            "SELECT member_user_id, group_id, read_ceiling::text, write_ceiling::text, manage
             FROM proxima_core.group_role_projection ORDER BY member_user_id, group_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let actual: Roles = rows
            .into_iter()
            .map(|(person, group, read, write, manage)| {
                (
                    (person, group),
                    Role::new(ceiling(&read), ceiling(&write), manage).expect("valid stored role"),
                )
            })
            .collect();
        assert_eq!(
            actual, expected,
            "projection equals the independent climb from written rows"
        );
        let mut conn = self.pool.acquire().await?;
        for ((person, group), role) in expected {
            assert_eq!(
                group_role(&mut conn, UserId::new(person), GroupId::new(group)).await?,
                Some(role)
            );
        }
        Ok(())
    }
}

fn ceiling(text: &str) -> AccessCeiling {
    match text {
        "none" => AccessCeiling::None,
        "fact" => AccessCeiling::Fact,
        "abstraction" => AccessCeiling::Abstraction,
        "perspective" => AccessCeiling::Perspective,
        "goal" => AccessCeiling::Goal,
        _ => panic!("unknown stored ceiling {text}"),
    }
}

/// Independent Rust traversal; neither the projection nor SQL's folding is an input.
async fn reference_climb(pool: &PgPool) -> TestResult<Roles> {
    let rows: Vec<(Uuid, Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, group_id, relation FROM proxima_core.group_memberships",
    )
    .fetch_all(pool)
    .await?;
    let edges: Vec<(Uuid, Uuid, Relation)> = sqlx::query_as(
        "SELECT child_group_id, parent_group_id, relation FROM proxima_core.group_edges",
    )
    .fetch_all(pool)
    .await?;
    let parents: BTreeMap<_, _> = edges
        .into_iter()
        .map(|(child, parent, cap)| (child, (parent, cap)))
        .collect();
    let mut result = Roles::new();
    for (person, mut group, relation) in rows {
        let mut role = relation.role();
        loop {
            result
                .entry((person, group))
                .and_modify(|current| *current = current.join(role))
                .or_insert(role);
            let Some((parent, cap)) = parents.get(&group) else {
                break;
            };
            group = *parent;
            role = role.meet(cap.role());
        }
    }
    Ok(result)
}

#[tokio::test]
async fn projection_equals_reference_after_seeded_person_and_group_write_sequences() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(12).await?;
    let people: Vec<_> = (0..5).map(|_| OwnerId::new(Uuid::now_v7())).collect();
    // A deep chain and a person reached directly and through the chain.
    for index in 1..groups.len() {
        h.add(
            groups[index - 1],
            OwnerId::from(groups[index]),
            RELATIONS[index % 4],
        )
        .await?;
        h.assert_projection().await?;
    }
    h.add(groups[11], people[0], Relation::Admin).await?;
    h.add(groups[0], people[0], Relation::Viewer).await?;
    h.assert_projection().await?;
    let mut seed = 0x31_5eed_u64;
    for step in 0..160 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let person = people[usize::try_from(seed % 5)?];
        let group = groups[usize::try_from((seed >> 8) % 12)?];
        let relation = RELATIONS[usize::try_from((seed >> 16) % 4)?];
        match step % 6 {
            0 | 1 => h.add(group, person, relation).await?,
            2 => h.remove(group, person).await?,
            3 => {
                let mut tx = h.pool.begin().await?;
                remove_group_member_relation(&mut tx, group, person, relation).await?;
                tx.commit().await?;
            }
            4 => {
                h.add(group, person, relation).await?;
                h.replace(
                    group,
                    person,
                    relation,
                    RELATIONS[(usize::try_from(seed % 4)? + 1) % 4],
                )
                .await?;
            }
            5 => {
                let index = 1 + usize::try_from((seed >> 24) % 11)?;
                h.remove(groups[index - 1], OwnerId::from(groups[index]))
                    .await?;
                h.assert_projection().await?;
                h.add(groups[index - 1], OwnerId::from(groups[index]), relation)
                    .await?;
                h.replace(
                    groups[index - 1],
                    OwnerId::from(groups[index]),
                    relation,
                    Relation::Editor,
                )
                .await?;
            }
            _ => unreachable!(),
        }
        h.assert_projection().await?;
    }
    let scope =
        proxima_storage_pg::PgPlatformScope::new(h.platform.clone(), &["proxima_core"]).await?;
    let (runtime_url, _) = split_role_urls(h.database.name()).await?;
    let storage = PgStorage::connect_with_config(
        &runtime_url,
        proxima_storage_pg::PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
        scope,
    )
    .await?;
    for person in people {
        sqlx::query("INSERT INTO proxima_core.owners(owner_id,kind) VALUES($1,'personal') ON CONFLICT DO NOTHING")
            .bind(person.into_inner()).execute(&h.pool).await?;
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: person.as_user(),
            drop_event_id: "seeded-drop".into(),
        });
        let surfaces = OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let outcome = storage
            .erase_personal_owner(&auth, person.as_user(), &surfaces)
            .await?;
        assert!(matches!(
            outcome,
            proxima_core::owner_inverse::OwnerEraseOutcome::Completed { .. }
        ));
        h.assert_projection().await?;
    }
    Ok(())
}

#[tokio::test]
async fn database_lattice_equals_rust_for_all_relations_and_cap_chains() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(4).await?;
    for first in RELATIONS {
        for second in RELATIONS {
            for cap1 in RELATIONS {
                for cap2 in RELATIONS {
                    let person = OwnerId::new(Uuid::now_v7());
                    h.remove(groups[0], OwnerId::from(groups[1])).await?;
                    h.remove(groups[1], OwnerId::from(groups[2])).await?;
                    h.add(groups[0], OwnerId::from(groups[1]), cap2).await?;
                    h.add(groups[1], OwnerId::from(groups[2]), cap1).await?;
                    h.add(groups[2], person, first).await?;
                    h.add(groups[0], person, second).await?;
                    let mut conn = h.pool.acquire().await?;
                    let role = group_role(&mut conn, person.as_user(), groups[0]).await?;
                    assert_eq!(
                        role,
                        Some(
                            first
                                .role()
                                .meet(cap1.role())
                                .meet(cap2.role())
                                .join(second.role())
                        )
                    );
                    drop(conn);
                    h.remove(groups[2], person).await?;
                    h.remove(groups[0], person).await?;
                }
            }
        }
    }
    h.assert_projection().await
}

#[tokio::test]
async fn readding_same_group_edge_is_a_noop() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(2).await?;
    let person = OwnerId::new(Uuid::now_v7());
    h.add(groups[1], person, Relation::Admin).await?;
    h.add(groups[0], OwnerId::from(groups[1]), Relation::Viewer)
        .await?;
    let before: Vec<(Uuid, Uuid, Relation)> = sqlx::query_as(
        "SELECT child_group_id, parent_group_id, relation
         FROM proxima_core.group_edges
         WHERE child_group_id = $1",
    )
    .bind(groups[1].into_inner())
    .fetch_all(&h.pool)
    .await?;
    assert_eq!(before.len(), 1, "one stored group edge");
    let projection_before: Vec<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT member_user_id, group_id, read_ceiling::text, write_ceiling::text, manage
         FROM proxima_core.group_role_projection
         WHERE member_user_id = $1
         ORDER BY group_id",
    )
    .bind(person.into_inner())
    .fetch_all(&h.pool)
    .await?;

    h.add(groups[0], OwnerId::from(groups[1]), Relation::Viewer)
        .await?;

    let conflict = h
        .add(groups[0], OwnerId::from(groups[1]), Relation::Editor)
        .await
        .expect_err("a different relation requires replacement");
    assert!(matches!(
        conflict.downcast_ref::<proxima_core::StorageError>(),
        Some(proxima_core::StorageError::Conflict(message))
            if message == "group is already attached to a parent"
    ));

    let after: Vec<(Uuid, Uuid, Relation)> = sqlx::query_as(
        "SELECT child_group_id, parent_group_id, relation
         FROM proxima_core.group_edges
         WHERE child_group_id = $1",
    )
    .bind(groups[1].into_inner())
    .fetch_all(&h.pool)
    .await?;
    let projection_after: Vec<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT member_user_id, group_id, read_ceiling::text, write_ceiling::text, manage
         FROM proxima_core.group_role_projection
         WHERE member_user_id = $1
         ORDER BY group_id",
    )
    .bind(person.into_inner())
    .fetch_all(&h.pool)
    .await?;
    assert_eq!(before, after, "same edge re-add changes no edge row");
    assert_eq!(
        projection_before, projection_after,
        "same edge re-add changes no projection"
    );
    h.assert_projection().await
}

#[tokio::test]
async fn roles_climb_only_and_take_weakest_path_then_strongest_alternative() -> TestResult {
    let h = Harness::new(None).await?;
    let g = h.groups(4).await?;
    let person = OwnerId::new(Uuid::now_v7());
    let parent_only = OwnerId::new(Uuid::now_v7());
    h.add(g[0], OwnerId::from(g[1]), Relation::Editor).await?;
    h.add(g[1], person, Relation::Admin).await?;
    h.add(g[0], person, Relation::Viewer).await?;
    h.add(g[0], parent_only, Relation::Admin).await?;
    let mut conn = h.pool.acquire().await?;
    assert_eq!(
        group_role(&mut conn, person.as_user(), g[0]).await?,
        Some(Role::editor())
    );
    assert_eq!(
        group_role(&mut conn, parent_only.as_user(), g[1]).await?,
        None,
        "no downward authority"
    );
    drop(conn);
    h.add(g[2], OwnerId::from(g[0]), Relation::Viewer).await?;
    let mut conn = h.pool.acquire().await?;
    let role = group_role(&mut conn, person.as_user(), g[2])
        .await?
        .expect("ancestor role");
    assert!(role.may_read(proxima_core::AccessKind::Goal));
    assert!(!role.may_write(proxima_core::AccessKind::Fact));
    drop(conn);
    h.assert_projection().await
}

#[tokio::test]
async fn database_and_host_refuse_second_parent_cycles_self_edges_and_person_kind_confusion()
-> TestResult {
    let h = Harness::new(None).await?;
    let g = h.groups(3).await?;
    h.add(g[0], OwnerId::from(g[1]), Relation::Editor).await?;
    for (parent, child) in [(g[2], g[1]), (g[1], g[0]), (g[2], g[2])] {
        assert!(
            h.add(parent, OwnerId::from(child), Relation::Viewer)
                .await
                .is_err()
        );
        let error = sqlx::query("INSERT INTO proxima_core.group_edges (parent_group_id, child_group_id, relation) VALUES ($1,$2,'viewer')")
            .bind(parent.into_inner()).bind(child.into_inner()).execute(&h.pool).await;
        assert!(
            error.is_err(),
            "raw SQL must refuse {parent:?} <- {child:?}"
        );
    }
    let personal_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.group_memberships WHERE member_user_id = $1",
    )
    .bind(g[1].into_inner())
    .fetch_one(&h.pool)
    .await?;
    assert_eq!(personal_rows, 0, "registered group becomes an edge");
    sqlx::query("INSERT INTO proxima_core.owners(owner_id,kind) VALUES($1,'personal') ON CONFLICT (owner_id) DO NOTHING")
        .bind(g[1].into_inner()).execute(&h.pool).await?;
    assert!(sqlx::query("INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation) VALUES ($1,$2,'viewer')")
        .bind(g[0].into_inner()).bind(g[2].into_inner()).execute(&h.pool).await.is_err(), "raw person row cannot name group");
    let person = OwnerId::new(Uuid::now_v7());
    h.add(g[0], person, Relation::Viewer).await?;
    let mut tx = h.pool.begin().await?;
    assert!(
        register_group(&mut tx, person.as_group()).await.is_err(),
        "person membership blocks group registration"
    );
    tx.rollback().await?;
    assert!(
        sqlx::query("INSERT INTO proxima_core.owners (owner_id,kind) VALUES ($1,'group')")
            .bind(person.into_inner())
            .execute(&h.pool)
            .await
            .is_err()
    );
    h.assert_projection().await
}

#[tokio::test]
async fn fixed_snapshot_isolation_refuses_membership_writes() -> TestResult {
    let h = Harness::new(None).await?;
    let group = h.groups(1).await?[0];
    for serializable in [false, true] {
        let mut tx = h.pool.begin().await?;
        let query = if serializable {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        } else {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        };
        query.execute(&mut *tx).await?;
        let error = add_group_member(
            &mut tx,
            group,
            OwnerId::new(Uuid::now_v7()),
            Relation::Viewer,
        )
        .await
        .expect_err("fixed snapshot refused");
        assert!(
            matches!(error, proxima_core::StorageError::MembershipIsolation),
            "typed refusal: {error:?}"
        );
        tx.rollback().await?;
    }
    Ok(())
}

async fn wait_for_advisory(pool: &PgPool, pid: i32) -> TestResult {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND NOT granted)")
                .bind(pid).fetch_one(pool).await?;
            if waiting { return Ok::<(), sqlx::Error>(()) }
            tokio::task::yield_now().await;
        }
    }).await??;
    Ok(())
}

#[tokio::test]
async fn racing_person_and_subgroup_writes_wait_on_one_tree_in_both_orders() -> TestResult {
    let h = Harness::new(None).await?;
    let g = h.groups(3).await?;
    h.add(g[0], OwnerId::from(g[1]), Relation::Admin).await?;
    let person = OwnerId::new(Uuid::now_v7());
    for edge_first in [false, true] {
        let mut first = h.pool.begin().await?;
        if edge_first {
            replace_group_member_relation(
                &mut first,
                g[0],
                OwnerId::from(g[1]),
                Relation::Admin,
                Relation::Editor,
            )
            .await?;
        } else {
            add_group_member(&mut first, g[1], person, Relation::Admin).await?;
        }
        let mut second = h.pool.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *second)
            .await?;
        let (parent, child) = (g[0], g[1]);
        let pending = tokio::spawn(async move {
            if edge_first {
                add_group_member(&mut second, child, person, Relation::Viewer).await?;
            } else {
                replace_group_member_relation(
                    &mut second,
                    parent,
                    OwnerId::from(child),
                    Relation::Admin,
                    Relation::Editor,
                )
                .await?;
            }
            second
                .commit()
                .await
                .map_err(|e| proxima_core::StorageError::Internal(e.to_string()))
        });
        wait_for_advisory(&h.pool, pid).await?;
        assert!(
            !pending.is_finished(),
            "pg_locks proves second writer is waiting"
        );
        first.commit().await?;
        pending.await??;
        h.assert_projection().await?;
        h.replace(g[0], OwnerId::from(g[1]), Relation::Editor, Relation::Admin)
            .await?;
    }
    // Two-tree attach waits for a membership write in the destination tree.
    let mut first = h.pool.begin().await?;
    add_group_member(&mut first, g[2], person, Relation::Ingest).await?;
    let mut second = h.pool.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await?;
    let (parent, child) = (g[2], g[0]);
    let pending = tokio::spawn(async move {
        add_group_member(&mut second, parent, OwnerId::from(child), Relation::Viewer).await?;
        second
            .commit()
            .await
            .map_err(|e| proxima_core::StorageError::Internal(e.to_string()))
    });
    wait_for_advisory(&h.pool, pid).await?;
    first.commit().await?;
    pending.await??;
    h.assert_projection().await?;
    h.remove(g[2], OwnerId::from(g[0])).await?;
    // Reverse order: attachment holds both roots, and a raw writer that
    // started with the old source root must confirm the new root after waiting.
    let mut first = h.pool.begin().await?;
    add_group_member(&mut first, g[2], OwnerId::from(g[0]), Relation::Editor).await?;
    let mut second = h.pool.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await?;
    let group = g[1];
    let pending = tokio::spawn(async move {
        sqlx::query("INSERT INTO proxima_core.group_memberships(group_id,member_user_id,relation) VALUES($1,$2,'ingest') ON CONFLICT DO NOTHING")
            .bind(group.into_inner()).bind(person.into_inner()).execute(&mut *second).await?;
        second.commit().await
    });
    wait_for_advisory(&h.pool, pid).await?;
    assert!(!pending.is_finished());
    first.commit().await?;
    pending.await??;
    h.assert_projection().await
}

async fn scope(conn: &mut PgConnection, groups: &[GroupId]) -> TestResult {
    let ids: Vec<_> = groups.iter().map(|g| g.into_inner()).collect();
    sqlx::query("SELECT set_config('app.proxima_scope','owner',true), set_config('app.owner',$1::uuid[]::text,true), set_config('app.manage_owner',$1::uuid[]::text,true)")
        .bind(ids).execute(conn).await?;
    Ok(())
}

#[tokio::test]
async fn owner_scoped_writes_fold_the_whole_tree_without_leaking_platform_scope() -> TestResult {
    let h = Harness::new(None).await?;
    let g = h.groups(3).await?;
    h.add(g[0], OwnerId::from(g[1]), Relation::Editor).await?;
    h.add(g[1], OwnerId::from(g[2]), Relation::Viewer).await?;
    let person = OwnerId::new(Uuid::now_v7());
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &[g[2]]).await?;
    sqlx::query("INSERT INTO proxima_core.group_memberships (group_id,member_user_id,relation) VALUES ($1,$2,'admin')")
        .bind(g[2].into_inner()).bind(person.into_inner()).execute(&mut *tx).await?;
    let current_scope: String = sqlx::query_scalar("SELECT current_setting('app.proxima_scope')")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(
        current_scope, "owner",
        "trigger elevation must be scoped to the function"
    );
    let visible: Vec<Uuid> = sqlx::query_scalar(
        "SELECT group_id FROM proxima_core.group_role_projection WHERE member_user_id=$1",
    )
    .bind(person.into_inner())
    .fetch_all(&mut *tx)
    .await?;
    assert_eq!(
        visible,
        vec![g[2].into_inner()],
        "projection keeps group owner policy"
    );
    assert!(
        sqlx::query(
            "UPDATE proxima_core.group_role_projection SET manage=true WHERE member_user_id=$1"
        )
        .bind(person.into_inner())
        .execute(&mut *tx)
        .await
        .is_err(),
        "projection only written by triggers"
    );
    tx.rollback().await?;
    // Repeat and commit; compare with a platform-scoped write.
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &[g[2]]).await?;
    sqlx::query("INSERT INTO proxima_core.group_memberships (group_id,member_user_id,relation) VALUES ($1,$2,'admin')")
        .bind(g[2].into_inner()).bind(person.into_inner()).execute(&mut *tx).await?;
    tx.commit().await?;
    h.assert_projection().await?;
    h.remove(g[2], person).await?;
    let mut tx = h.platform.begin().await?;
    sqlx::query("SELECT set_config('app.proxima_scope','platform',true)")
        .execute(&mut *tx)
        .await?;
    add_group_member(&mut tx, g[2], person, Relation::Admin).await?;
    tx.commit().await?;
    h.assert_projection().await
}

#[tokio::test]
async fn v0_0_30_upgrade_preserves_every_persons_flat_roles() -> TestResult {
    let h = Harness::new(Some(27)).await?;
    let groups = h.groups(3).await?;
    let people: Vec<_> = (0..4).map(|_| Uuid::now_v7()).collect();
    let mut expected = Roles::new();
    for person in &people {
        for group in &groups {
            for relation in RELATIONS {
                sqlx::query("INSERT INTO proxima_core.group_memberships (group_id,member_user_id,relation) VALUES ($1,$2,$3)")
                    .bind(group.into_inner()).bind(person).bind(relation).execute(&h.pool).await?;
                expected
                    .entry((*person, group.into_inner()))
                    .and_modify(|role| *role = role.join(relation.role()))
                    .or_insert(relation.role());
            }
        }
    }
    let mut conn = h.platform.acquire().await?;
    let mut tx = begin_migration_transaction(&mut conn).await?;
    core_migrator().run(tx.as_mut()).await?;
    tx.commit().await?;
    drop(conn);
    assert_eq!(reference_climb(&h.pool).await?, expected);
    h.assert_projection().await
}
