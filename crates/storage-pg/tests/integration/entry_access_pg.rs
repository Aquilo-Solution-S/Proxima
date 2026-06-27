//! Slice B end-to-end: resource-scoped entry reads through `get_memory` /
//! `authorize_entry_request`. A friend with a viewer entry-grant reads a shared
//! entry; a stranger cannot; revoking the grant denies; a `public` entry is
//! readable by any stranger; unpublishing denies; tombstoning revokes grants and
//! resets visibility (so the entry leaves both the shared and public surfaces).

use std::collections::HashSet;

use crate::common::personality::ingest_test_fact;
use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::access::{
    GrantResource, GrantSelector, GrantSubject, NewAccessGrant, Relation, RelationSelector,
    ShareVisibilityUpdate, Visibility,
};
use proxima_core::engine::GetMemoryReadRequest;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::{
    AccessScope, AuthPath, AuthzContext, CapabilitySet, Engine, Identity, MemoryId,
    PersonalityInstanceId, Principal, Storage, ToolScope, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::new()).with_storage(pg.clone().into_handle())
}

fn user() -> Principal {
    Principal::User(UserId::new(Uuid::now_v7()))
}

/// A `Granted` (non-Unrestricted) context for a caller distinct from any owner —
/// access is decided purely by persisted grants + entry visibility.
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

fn read_req(memory_id: MemoryId, neighbors: bool) -> GetMemoryReadRequest {
    GetMemoryReadRequest {
        memory_id,
        reader_personality_instance_id: None,
        include_neighbor_edges: neighbors,
    }
}

async fn seed_entry_grant(
    pg: &PgStorage,
    owner: &Principal,
    memory_id: MemoryId,
    subject: &Principal,
) {
    pg.share_entry_atomic(
        &NewAccessGrant {
            space_owner: owner.clone(),
            resource: GrantResource::Memory(memory_id),
            relation: Relation::Viewer,
            subject: GrantSubject::Principal(subject.clone()),
            granted_by: PersonalityInstanceId::new(Uuid::now_v7()),
        },
        ShareVisibilityUpdate::LeaveVisibility,
    )
    .await
    .expect("seed entry grant");
}

#[tokio::test]
async fn friend_with_viewer_grant_reads_shared_entry() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "shared note").await;
    let friend = user();
    let stranger = user();

    // Before any grant, the friend is denied (no relation on the entry).
    let err = engine
        .get_memory(&granted_authz(&friend), &read_req(entry, false))
        .await
        .expect_err("ungranted friend denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    // Grant the friend a viewer entry-grant — now the friend reads it. Even when
    // the friend requests neighbor edges, an EntryScoped (single-entry) grant is
    // body-only: it must NOT hydrate neighbors and leak the entry's graph
    // neighborhood (adjacent memory ids) the friend has no grant for.
    seed_entry_grant(&pg, &owner, entry, &friend).await;
    let resp = engine
        .get_memory(&granted_authz(&friend), &read_req(entry, true))
        .await?;
    assert!(
        resp.memory.is_some(),
        "friend with viewer grant reads the entry"
    );
    assert!(
        resp.neighbor_edges.is_empty(),
        "EntryScoped read must not hydrate neighbor edges"
    );

    // A stranger (no grant) is still denied.
    let err = engine
        .get_memory(&granted_authz(&stranger), &read_req(entry, false))
        .await
        .expect_err("stranger denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    // Revoke the friend's grant — read is denied again.
    pg.revoke_access_grants(&GrantSelector {
        space_owner: owner.clone(),
        resource: GrantResource::Memory(entry),
        relation: RelationSelector::AllGrantable,
        subject: GrantSubject::Principal(friend.clone()),
    })
    .await?;
    let err = engine
        .get_memory(&granted_authz(&friend), &read_req(entry, false))
        .await
        .expect_err("revoked friend denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn public_entry_readable_by_stranger_then_unpublish_denies()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "marketplace entry").await;
    let stranger = user();

    // Private by default: a stranger is denied.
    let err = engine
        .get_memory(&granted_authz(&stranger), &read_req(entry, true))
        .await
        .expect_err("private entry denied to stranger");
    assert_eq!(err.code, ErrorCode::Forbidden);

    // Publish: any stranger may read it, and a public read NEVER hydrates neighbors.
    pg.set_memory_visibility(&owner, entry, Visibility::Public)
        .await?;
    let resp = engine
        .get_memory(&granted_authz(&stranger), &read_req(entry, true))
        .await?;
    assert!(resp.memory.is_some(), "public entry readable by stranger");
    assert!(
        resp.neighbor_edges.is_empty(),
        "PublicRead must not hydrate neighbor edges"
    );

    // Unpublish: denied again.
    pg.set_memory_visibility(&owner, entry, Visibility::Private)
        .await?;
    let err = engine
        .get_memory(&granted_authz(&stranger), &read_req(entry, true))
        .await
        .expect_err("unpublished entry denied to stranger");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test]
async fn tombstone_revokes_grants_and_denies_read() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let engine = engine_for(&pg);

    let owner = owner_fixture();
    let entry = ingest_test_fact(&pg, &owner, "to be erased").await;
    let friend = user();

    seed_entry_grant(&pg, &owner, entry, &friend).await;
    pg.set_memory_visibility(&owner, entry, Visibility::Public)
        .await?;
    assert_eq!(pg.count_active_entry_grants(&owner, entry).await?, 1);

    // Tombstone via the owner (Unrestricted authorizes the ingest-gated verb).
    engine
        .tombstone_fact(
            &AuthzContext::single_owner(&owner, AuthPath::System),
            &owner,
            entry,
        )
        .await?;

    // The same-transaction trigger revoked the grant and reset visibility.
    assert_eq!(
        pg.count_active_entry_grants(&owner, entry).await?,
        0,
        "tombstone revokes active grants"
    );
    assert!(
        pg.resolve_entry_owner(entry).await?.is_none(),
        "tombstoned entry no longer resolves (not served on any surface)"
    );

    // The friend can no longer read it; a public read is also gone.
    let err = engine
        .get_memory(&granted_authz(&friend), &read_req(entry, false))
        .await
        .expect_err("tombstoned entry denied");
    assert_eq!(err.code, ErrorCode::Forbidden);

    drop_db(&db).await?;
    Ok(())
}
