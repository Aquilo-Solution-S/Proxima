use std::time::Duration;
use uuid::Uuid;

use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use std::sync::Arc;

use proxima_core::{
    HandleTable, MemoryHandleClass, MemoryId, OrgId, Owner, Principal, UserId, WakeChainDepth,
};

fn make_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn make_ctx(owner: Owner) -> WakeTokenContext {
    WakeTokenContext {
        invocation_id: Uuid::new_v4(),
        personality_instance_id: Uuid::new_v4(),
        wake_entry_id: Uuid::new_v4(),
        change_event_seq: Uuid::new_v4(),
        owner,
        palette: vec!["core/emit_abstraction".into()],
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
        current_root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        current_root_perspective_memory_class: MemoryHandleClass::Perspective,
        triggering_event_memory_id: MemoryId::new(Uuid::now_v7()),
        triggering_event_memory_class: MemoryHandleClass::Fact,
        triggering_event_depth: WakeChainDepth::new(0),
        read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: Arc::new(HandleTable::new()),
    }
}

#[tokio::test]
async fn mint_then_resolve_returns_same_context() {
    let store = WakeTokenStore::new(Duration::from_secs(60));
    let owner = make_owner();
    let ctx = make_ctx(owner.clone());
    let token = store.mint(ctx.clone()).await;
    let resolved = store.resolve(token).await.expect("resolves");
    assert_eq!(resolved.invocation_id, ctx.invocation_id);
    assert_eq!(resolved.palette, ctx.palette);
}

#[tokio::test]
async fn resolve_before_idle_expiry_renews_idle_lease() {
    let store = WakeTokenStore::new(Duration::from_millis(80));
    let token = store
        .mint_with_max_lifetime(make_ctx(make_owner()), Duration::from_millis(400))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(store.resolve(token).await.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        store.resolve(token).await.is_some(),
        "successful resolve should renew the idle lease"
    );
}

#[tokio::test]
async fn unused_token_expires_after_idle_timeout() {
    let store = WakeTokenStore::new(Duration::from_millis(50));
    let token = store
        .mint_with_max_lifetime(make_ctx(make_owner()), Duration::from_secs(1))
        .await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(store.resolve(token).await.is_none());
}

#[tokio::test]
async fn resolve_cannot_renew_past_max_lifetime() {
    let store = WakeTokenStore::new(Duration::from_millis(80));
    let token = store
        .mint_with_max_lifetime(make_ctx(make_owner()), Duration::from_millis(130))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(store.resolve(token).await.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(store.resolve(token).await.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(store.resolve(token).await.is_none());
}

#[tokio::test]
async fn revoke_removes_token() {
    let store = WakeTokenStore::new(Duration::from_secs(60));
    let token = store.mint(make_ctx(make_owner())).await;
    assert!(store.resolve(token).await.is_some());
    store.revoke(token).await;
    assert!(store.resolve(token).await.is_none());
}

#[tokio::test]
async fn sweep_expired_drops_old_tokens() {
    let store = WakeTokenStore::new(Duration::from_millis(50));
    let token = store.mint(make_ctx(make_owner())).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let removed = store.sweep_expired().await;
    assert_eq!(removed, 1);
    assert!(store.resolve(token).await.is_none());
}

#[tokio::test]
async fn unknown_token_resolves_none() {
    let store = WakeTokenStore::new(Duration::from_secs(60));
    let bogus = Uuid::new_v4();
    assert!(store.resolve(bogus).await.is_none());
}
