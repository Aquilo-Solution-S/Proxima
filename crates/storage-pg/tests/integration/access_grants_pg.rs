//! Storage-layer coverage for the `access_grants` repository (migration 0005):
//! space-level resolution, the `member` group indirection, the multiple-owner
//! ops + orphan guard, and entry-level grants + visibility + existence trigger.

use crate::common::personality::ingest_test_fact;
use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::Storage;
use proxima_core::access::{
    GrantResource, GrantSelector, GrantSubject, NewAccessGrant, Relation, RelationSelector,
    RemoveOwnerOutcome, Visibility,
};
use proxima_core::{GroupId, MemoryId, PersonalityInstanceId, Principal, UserId};
use uuid::Uuid;

fn user() -> Principal {
    Principal::User(UserId::new(Uuid::now_v7()))
}

fn group() -> Principal {
    Principal::Group(GroupId::new(Uuid::now_v7()))
}

fn pers() -> PersonalityInstanceId {
    PersonalityInstanceId::new(Uuid::now_v7())
}

fn space_grant(space: &Principal, relation: Relation, subject: GrantSubject) -> NewAccessGrant {
    NewAccessGrant {
        space_owner: space.clone(),
        resource: GrantResource::Space,
        relation,
        subject,
        granted_by: pers(),
    }
}

fn group_subject(principal: &Principal) -> GrantSubject {
    match principal {
        Principal::Group(group) => GrantSubject::Group(*group),
        Principal::User(_) => unreachable!("test helper expects a group principal"),
    }
}

#[tokio::test]
async fn space_grant_resolves_dominates_and_revokes() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let space = user();
    let alice = user();
    pg.insert_space_binding(&space_grant(
        &space,
        Relation::Editor,
        GrantSubject::Principal(alice.clone()),
    ))
    .await?;

    let rels = pg.resolve_space_relations(&space, &alice).await?;
    assert_eq!(rels.len(), 1, "one active grant");
    assert_eq!(rels[0].relation, Relation::Editor);
    assert!(
        rels[0].relation.dominates(Relation::Viewer),
        "editor ⊒ viewer"
    );
    assert!(
        !rels[0].relation.dominates(Relation::Admin),
        "editor ⋡ admin"
    );

    // Re-grant is idempotent (ON CONFLICT DO NOTHING).
    pg.insert_space_binding(&space_grant(
        &space,
        Relation::Editor,
        GrantSubject::Principal(alice.clone()),
    ))
    .await?;
    assert_eq!(pg.resolve_space_relations(&space, &alice).await?.len(), 1);

    // A stranger sees nothing.
    assert!(
        pg.resolve_space_relations(&space, &user())
            .await?
            .is_empty()
    );

    let revoked = pg
        .revoke_access_grants(&GrantSelector {
            space_owner: space.clone(),
            resource: GrantResource::Space,
            relation: RelationSelector::AllGrantable,
            subject: GrantSubject::Principal(alice.clone()),
        })
        .await?;
    assert_eq!(revoked, 1);
    assert!(pg.resolve_space_relations(&space, &alice).await?.is_empty());

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn member_inherits_group_space_binding() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let space = user(); // some space X
    let eng = group();
    let bob = user();

    // (space:X, editor, group:eng) and (space:eng, member, principal:bob)
    pg.insert_space_binding(&space_grant(&space, Relation::Editor, group_subject(&eng)))
        .await?;
    pg.insert_space_binding(&space_grant(
        &eng,
        Relation::Member,
        GrantSubject::Principal(bob.clone()),
    ))
    .await?;

    let rels = pg.resolve_space_relations(&space, &bob).await?;
    assert!(
        rels.iter().any(|r| r.relation == Relation::Editor),
        "member of eng inherits eng's editor binding on X"
    );

    // A non-member gets nothing from the group binding.
    assert!(
        pg.resolve_space_relations(&space, &user())
            .await?
            .is_empty()
    );

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn owner_ops_with_orphan_guard() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let eng = group();
    let p1 = user();
    let p2 = user();

    pg.init_space_owner(&eng, &p1, pers()).await?;
    assert!(
        pg.resolve_space_relations(&eng, &p1)
            .await?
            .iter()
            .any(|r| r.relation == Relation::Owner),
        "init_space_owner bootstraps p1 as owner"
    );

    pg.add_space_owner(&eng, &p2, pers()).await?;
    assert_eq!(
        pg.remove_space_owner(&eng, &p1).await?,
        RemoveOwnerOutcome::Removed,
        "p1 removable while p2 remains"
    );
    assert_eq!(
        pg.remove_space_owner(&eng, &p2).await?,
        RemoveOwnerOutcome::RefusedLastOwner,
        "last owner cannot be removed"
    );
    assert_eq!(
        pg.remove_space_owner(&eng, &p1).await?,
        RemoveOwnerOutcome::NotFound,
        "already-removed owner is NotFound"
    );

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn entry_grant_visibility_and_existence() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "shared note").await;
    let friend = user();

    pg.share_entry_atomic(
        &NewAccessGrant {
            space_owner: owner.clone(),
            resource: GrantResource::Memory(entry),
            relation: Relation::Viewer,
            subject: GrantSubject::Principal(friend.clone()),
            granted_by: pers(),
        },
        false,
    )
    .await?;

    let rels = pg.resolve_entry_relations(entry, &friend).await?;
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].relation, Relation::Viewer);

    let facts = pg
        .resolve_entry_owner(entry)
        .await?
        .expect("live entry resolves");
    assert_eq!(facts.owner, owner);
    assert_eq!(facts.visibility, Visibility::Private);
    assert_eq!(pg.count_active_entry_grants(&owner, entry).await?, 1);

    pg.set_memory_visibility(&owner, entry, Visibility::Public)
        .await?;
    assert_eq!(
        pg.resolve_entry_owner(entry).await?.unwrap().visibility,
        Visibility::Public
    );

    pg.revoke_access_grants(&GrantSelector {
        space_owner: owner.clone(),
        resource: GrantResource::Memory(entry),
        relation: RelationSelector::AllGrantable,
        subject: GrantSubject::Principal(friend.clone()),
    })
    .await?;
    assert_eq!(pg.count_active_entry_grants(&owner, entry).await?, 0);
    assert!(pg.resolve_entry_relations(entry, &friend).await?.is_empty());

    // Existence trigger: a grant on a nonexistent memory is rejected, and the
    // FK violation maps to a clean NotFound (not an internal 500).
    let bogus = MemoryId::new(Uuid::now_v7());
    let err = pg
        .share_entry_atomic(
            &NewAccessGrant {
                space_owner: owner.clone(),
                resource: GrantResource::Memory(bogus),
                relation: Relation::Viewer,
                subject: GrantSubject::Principal(friend.clone()),
                granted_by: pers(),
            },
            false,
        )
        .await
        .expect_err("grant on absent memory rejected by existence trigger");
    assert!(
        matches!(err, proxima_core::StorageError::NotFound),
        "absent-memory grant maps to NotFound, got {err:?}"
    );

    drop_db(&db).await?;
    Ok(())
}
