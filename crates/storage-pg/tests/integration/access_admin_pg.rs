//! Slice C end-to-end: the owner-gated grant-management verbs + marketplace
//! browse, through the engine. Owners may share/unshare/publish/bind/transfer;
//! non-owners (editor/viewer/stranger) cannot; `init_space_owner` requires the
//! unrestricted provisioning capability; the last owner cannot be removed.

use std::collections::HashSet;

use crate::common::personality::ingest_test_fact;
use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::access::{EntryVisibilityTarget, GrantResource, GrantSubject, Relation};
use proxima_core::engine::GetMemoryReadRequest;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::{
    AccessScope, AuthPath, AuthzContext, CapabilitySet, Engine, GroupId, Identity, MemoryId,
    Principal, Storage, ToolScope, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::new()).with_storage(pg.clone().into_handle())
}

fn user() -> Principal {
    Principal::User(UserId::new(Uuid::now_v7()))
}

fn group() -> Principal {
    Principal::Group(GroupId::new(Uuid::now_v7()))
}

fn owner_authz(owner: &Principal) -> AuthzContext {
    AuthzContext::single_owner(owner, AuthPath::System)
}

/// `Granted` context for `principal` whose access is decided purely by persisted
/// grants (used for non-owner callers and persisted Group owners).
fn granted_authz(principal: &Principal) -> AuthzContext {
    AuthzContext {
        identity: Identity {
            principal: principal.clone(),
            accessible_principals: HashSet::new(),
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            access: AccessScope::Granted,
        },
        auth_path: AuthPath::HostBearer,
    }
}

fn read(memory_id: MemoryId) -> GetMemoryReadRequest {
    GetMemoryReadRequest {
        memory_id,
        reader_personality_instance_id: None,
        include_neighbor_edges: false,
    }
}

#[tokio::test]
async fn owner_shares_entry_then_unshares() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "share me").await;
    let friend = user();

    // Owner shares a viewer grant; the friend can read it.
    engine
        .share_entry(
            &owner_authz(&owner),
            entry,
            GrantSubject::Principal(friend.clone()),
            Relation::Viewer,
        )
        .await?;
    assert!(
        engine
            .get_memory(&granted_authz(&friend), &read(entry))
            .await?
            .memory
            .is_some()
    );

    // Owner unshares; the friend is denied again.
    engine
        .unshare_entry(
            &owner_authz(&owner),
            entry,
            GrantSubject::Principal(friend.clone()),
        )
        .await?;
    let err = engine
        .get_memory(&granted_authz(&friend), &read(entry))
        .await
        .expect_err("unshared friend denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn non_owner_cannot_manage_grants() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "guarded").await;

    // An editor on the owner's space is NOT an owner — grant management denies.
    let editor = user();
    engine
        .set_space_binding(
            &owner_authz(&owner),
            owner.clone(),
            GrantSubject::Principal(editor.clone()),
            Relation::Editor,
        )
        .await?;

    let victim = user();
    let err = engine
        .share_entry(
            &granted_authz(&editor),
            entry,
            GrantSubject::Principal(victim.clone()),
            Relation::Viewer,
        )
        .await
        .expect_err("editor cannot share");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .set_entry_visibility(
            &granted_authz(&editor),
            entry,
            EntryVisibilityTarget::Public,
        )
        .await
        .expect_err("editor cannot publish");
    assert_eq!(err.code, ErrorCode::Forbidden);

    // A total stranger likewise cannot.
    let err = engine
        .set_space_binding(
            &granted_authz(&user()),
            owner.clone(),
            GrantSubject::Principal(victim),
            Relation::Editor,
        )
        .await
        .expect_err("stranger cannot bind");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn share_rejects_owner_relation() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "no owner grant").await;
    let err = engine
        .share_entry(
            &owner_authz(&owner),
            entry,
            GrantSubject::Principal(user()),
            Relation::Owner,
        )
        .await
        .expect_err("owner relation is not grantable");
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn publish_lists_in_browse_then_unpublish_removes() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "for sale").await;
    let browser = user();

    // Not public yet: browse is empty.
    assert!(
        engine
            .browse_marketplace(&granted_authz(&browser), 50)
            .await?
            .is_empty()
    );

    // Publish via the owner-gated verb; browse now lists it.
    engine
        .set_entry_visibility(&owner_authz(&owner), entry, EntryVisibilityTarget::Public)
        .await?;
    let listed = engine
        .browse_marketplace(&granted_authz(&browser), 50)
        .await?;
    assert!(
        listed.iter().any(|m| m.memory_id == entry),
        "published entry appears in marketplace browse"
    );

    // Unpublish; browse no longer lists it.
    engine
        .set_entry_visibility(&owner_authz(&owner), entry, EntryVisibilityTarget::Private)
        .await?;
    assert!(
        engine
            .browse_marketplace(&granted_authz(&browser), 50)
            .await?
            .iter()
            .all(|m| m.memory_id != entry)
    );

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn space_binding_grants_then_revokes_entry_access() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "space scoped").await;
    let member = user();

    // A viewer space-binding lets the member read any entry in the space.
    engine
        .set_space_binding(
            &owner_authz(&owner),
            owner.clone(),
            GrantSubject::Principal(member.clone()),
            Relation::Viewer,
        )
        .await?;
    assert!(
        engine
            .get_memory(&granted_authz(&member), &read(entry))
            .await?
            .memory
            .is_some()
    );

    // Revoking the binding denies access.
    engine
        .revoke_space_binding(
            &owner_authz(&owner),
            owner.clone(),
            GrantSubject::Principal(member.clone()),
        )
        .await?;
    let err = engine
        .get_memory(&granted_authz(&member), &read(entry))
        .await
        .expect_err("revoked member denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn group_owner_provisioning_and_orphan_guard() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let space = group();
    let p1 = user();
    let p2 = user();

    // init_space_owner needs the unrestricted provisioning capability.
    let err = engine
        .init_space_owner(&granted_authz(&p1), space.clone(), p1.clone())
        .await
        .expect_err("non-unrestricted cannot provision");
    assert_eq!(err.code, ErrorCode::Forbidden);

    // A host/system (unrestricted, can-access the space) bootstraps the first owner.
    engine
        .init_space_owner(&owner_authz(&space), space.clone(), p1.clone())
        .await?;

    let err = engine
        .init_space_owner(&owner_authz(&space), space.clone(), p2.clone())
        .await
        .expect_err("second bootstrap is rejected");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(
        err.message.contains("space owner already provisioned"),
        "unexpected error message: {}",
        err.message
    );

    // p1 is now a persisted owner and may add a co-owner.
    engine
        .add_owner(&granted_authz(&p1), space.clone(), p2.clone())
        .await?;

    // p1 is removable while p2 remains; removing the last owner is refused.
    engine
        .remove_owner(&granted_authz(&p2), space.clone(), p1.clone())
        .await?;
    let err = engine
        .remove_owner(&granted_authz(&p2), space.clone(), p2.clone())
        .await
        .expect_err("cannot remove the last owner");
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    // list_grants (owner-gated) shows the surviving owner.
    let grants = engine
        .list_grants(&granted_authz(&p2), space.clone(), GrantResource::Space)
        .await?;
    assert!(
        grants
            .iter()
            .any(|g| g.relation == Relation::Owner
                && g.subject == GrantSubject::Principal(p2.clone()))
    );

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn init_space_owner_rejects_user_space() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let space = user();
    let owner = user();
    let err = engine
        .init_space_owner(&owner_authz(&space), space.clone(), owner)
        .await
        .expect_err("User spaces are identity-owned");
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    let grants = pg.list_access_grants(&space, GrantResource::Space).await?;
    assert!(
        grants.is_empty(),
        "User-space init must not write owner rows"
    );

    drop_db(&db).await?;
    Ok(())
}

/// Regression (security): the generic `revoke_space_binding` must NOT be able to
/// strip an `owner` row — that would bypass the `remove_space_owner` last-owner
/// orphan guard. Owner rows are managed solely by `add_owner`/`remove_owner`.
#[tokio::test]
async fn revoke_space_binding_cannot_remove_owner_rows() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let space = group();
    let p1 = user();
    let p2 = user();
    engine
        .init_space_owner(&owner_authz(&space), space.clone(), p1.clone())
        .await?;
    engine
        .add_owner(&granted_authz(&p1), space.clone(), p2.clone())
        .await?;

    // Attempt to revoke p1 via the generic binding verb — it must leave the owner
    // row intact (the verb only manages grantable relations).
    engine
        .revoke_space_binding(
            &granted_authz(&p2),
            space.clone(),
            GrantSubject::Principal(p1.clone()),
        )
        .await?;
    let grants = engine
        .list_grants(&granted_authz(&p2), space.clone(), GrantResource::Space)
        .await?;
    assert_eq!(
        grants
            .iter()
            .filter(|g| g.relation == Relation::Owner)
            .count(),
        2,
        "revoke_space_binding must not remove owner rows"
    );
    assert!(
        grants
            .iter()
            .any(|g| g.relation == Relation::Owner
                && g.subject == GrantSubject::Principal(p1.clone())),
        "p1 is still an owner after revoke_space_binding"
    );

    // The proper path still works and still guards the last owner.
    engine
        .remove_owner(&granted_authz(&p2), space.clone(), p1.clone())
        .await?;
    let err = engine
        .remove_owner(&granted_authz(&p2), space.clone(), p2.clone())
        .await
        .expect_err("last owner still guarded");
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    drop_db(&db).await?;
    Ok(())
}
