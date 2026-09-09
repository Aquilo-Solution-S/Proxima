//! Event identities survive normal cooling and an authorized destination
//! owner erase; a retained source-lane event keeps its original owner.

use std::sync::Arc;

use proxima_core::owner_inverse::OwnerEraseOutcome;
use proxima_core::storage_ports::ChangeEventPort;
use proxima_core::{
    AgentDerivationV1, AuthPath, AuthzContext, ChangeEvent, ChangeEventKind, ColdObjectStore,
    DerivationIdentity, DerivedMemory, Engine, EntityId, EntityKind, EntityRef, FlavorRegistry,
    GroupId, InputContractId, MemoryId, MemoryTarget, OperatorId, OwnerRef, SeriesHandle,
    StorageError, UserId, cold_object_key,
};
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use uuid::Uuid;

use super::{Fixture, TestResult};

async fn lifecycle_fixture() -> TestResult<(Fixture, Arc<MemoryColdStore>)> {
    let mut fixture = Fixture::new().await?;
    let cold = Arc::new(MemoryColdStore::default());
    fixture.pg = fixture.pg.clone().with_cold(cold.clone());
    fixture.engine = Arc::new(
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(fixture.pg.clone()).storage_ports()),
    );
    Ok((fixture, cold))
}

#[derive(Clone, Copy, Debug)]
struct MemoryIdentity {
    id: MemoryId,
    kind: EntityKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventOperation {
    Append,
    Delete,
    Transfer,
}

fn event_identity(event: &ChangeEvent) -> (EventOperation, EntityKind, EntityRef) {
    match event.kind {
        ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            ..
        } => (EventOperation::Append, entity_kind, entity),
        ChangeEventKind::EntityDelete {
            entity_kind,
            entity,
            ..
        } => (EventOperation::Delete, entity_kind, entity),
        ChangeEventKind::EntityTransfer {
            entity_kind,
            entity,
            ..
        } => (EventOperation::Transfer, entity_kind, entity),
    }
}

fn assert_event_identities(
    events: &[ChangeEvent],
    memories: &[MemoryIdentity],
    owner: OwnerRef,
    operations: &[EventOperation],
) {
    assert_eq!(events.len(), memories.len() * operations.len());
    for memory in memories {
        for operation in operations {
            let matching: Vec<_> = events
                .iter()
                .filter(|event| {
                    let (actual_operation, _, entity) = event_identity(event);
                    actual_operation == *operation && entity == EntityRef::Memory(memory.id)
                })
                .collect();
            assert_eq!(matching.len(), 1, "one {operation:?} event for {memory:?}");
            let event = matching[0];
            assert_eq!(event.owner, owner, "historical event owner must not move");
            assert_eq!(
                event_identity(event).1,
                memory.kind,
                "event {} for {:?} must retain its original kind after lifecycle changes",
                event.seq,
                memory.id
            );
        }
    }
}

async fn read_event_paths(fixture: &Fixture) -> TestResult<(Vec<ChangeEvent>, Vec<ChangeEvent>)> {
    let forward = fixture
        .pg
        .list_change_events_after(&[fixture.owner], Uuid::nil(), 100)
        .await?
        .into_iter()
        .map(|row| row.event)
        .collect::<Vec<_>>();
    let replay = fixture
        .pg
        .list_change_events_for_replay(&fixture.owner, Uuid::nil(), None, 100)
        .await?
        .into_iter()
        .map(|row| row.event)
        .collect::<Vec<_>>();
    assert_eq!(
        forward.iter().map(|event| event.seq).collect::<Vec<_>>(),
        replay.iter().map(|event| event.seq).collect::<Vec<_>>(),
        "single-event replay and batch forward hydration cover the same durable events"
    );
    Ok((forward, replay))
}

async fn derive(
    fixture: &Fixture,
    origin: MemoryIdentity,
    kind: EntityKind,
) -> TestResult<MemoryIdentity> {
    let payload = AgentDerivationV1 {
        title: format!("{kind:?} from {:?}", origin.kind),
        body: "normal typed derivation for the event lifecycle".into(),
        tags: Vec::new(),
        idempotency_key: None,
        source_memory_ids: vec![origin.id.into_inner()],
        model_id: "test-model".into(),
        client_name: "event-lifecycle-test".into(),
        client_version: "1".into(),
    };
    let target = MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7()));
    let text = "normal typed derivation for the event lifecycle";
    let identity = DerivationIdentity::new(
        OperatorId::new(Uuid::now_v7()),
        InputContractId::new(Uuid::now_v7()),
    );
    let memory = match kind {
        EntityKind::Abstraction => {
            DerivedMemory::abstraction(target, fixture.owner, text, payload, [origin.id], identity)?
        }
        EntityKind::Perspective => {
            DerivedMemory::perspective(target, fixture.owner, text, payload, [origin.id], identity)?
        }
        _ => unreachable!("the fixture derives only A and P"),
    };
    let outcome = fixture.engine.derive_memory(&fixture.authz, memory).await?;
    assert!(!outcome.idempotent_replay);
    assert_eq!(
        outcome.edge_count, 1,
        "normal F-to-A/A-to-P origin admission"
    );
    Ok(MemoryIdentity {
        id: outcome.memory_id,
        kind,
    })
}

async fn chain(fixture: &Fixture) -> TestResult<[MemoryIdentity; 3]> {
    let fact = MemoryIdentity {
        id: fixture.initial_note().await?.memory_id,
        kind: EntityKind::Fact,
    };
    let abstraction = derive(fixture, fact, EntityKind::Abstraction).await?;
    let perspective = derive(fixture, abstraction, EntityKind::Perspective).await?;
    let memories = [fact, abstraction, perspective];
    let (forward, replay) = read_event_paths(fixture).await?;
    for events in [&forward, &replay] {
        assert_event_identities(events, &memories, fixture.owner, &[EventOperation::Append]);
    }
    Ok(memories)
}

async fn cool_chain(
    fixture: &Fixture,
    cold: &MemoryColdStore,
    memories: &[MemoryIdentity],
) -> TestResult<()> {
    for memory in memories.iter().rev() {
        fixture
            .engine
            .forget_memory(&fixture.authz, fixture.owner, memory.id)
            .await?;
        let (owner_id, kind, hot, erased): (Uuid, String, bool, bool) = sqlx::query_as(
            "SELECT c.owner_id, c.kind::text,
                    EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = c.t),
                    EXISTS (SELECT 1 FROM proxima_core.erased_pin_target e WHERE e.t = c.t)
               FROM proxima_core.cooled c WHERE c.t = $1",
        )
        .bind(memory.id.into_inner())
        .fetch_one(fixture.pg.pool_for_tests())
        .await?;
        assert_eq!(owner_id, fixture.owner.stored_owner_id());
        assert_eq!(kind, memory.kind.as_str().to_lowercase());
        assert!(
            !hot && !erased,
            "Forget preserves an exact cooled identity without an erase witness"
        );
        assert!(
            !cold
                .get(&cold_object_key(memory.id.into_inner()))
                .await?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn cooling_preserves_original_and_delete_event_kinds() {
    let (fixture, cold) = lifecycle_fixture().await.expect("isolated PG fixture");
    let result: TestResult<_> = async {
        let memories = chain(&fixture).await?;
        cool_chain(&fixture, &cold, &memories).await?;
        let paths = read_event_paths(&fixture).await?;
        Ok((memories, paths))
    }
    .await;
    let owner = fixture.owner;
    fixture.close().await;
    let (memories, (forward, replay)) = result.expect("normal typed F/A/P cooling");
    for events in [&forward, &replay] {
        assert_event_identities(
            events,
            &memories,
            owner,
            &[EventOperation::Append, EventOperation::Delete],
        );
    }
}

async fn erase_after_transfer(
    fixture: &Fixture,
    cold: &MemoryColdStore,
    memories: &[MemoryIdentity],
) -> TestResult<()> {
    let group = GroupId::new(Uuid::now_v7());
    let destination = OwnerRef::Group(group);
    let pool = fixture.pg.pool_for_tests();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
        .bind(destination.stored_owner_id())
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
         VALUES ($1, $2, 'admin')",
    )
    .bind(destination.stored_owner_id())
    .bind(fixture.owner.stored_owner_id())
    .execute(pool)
    .await?;
    for memory in memories.iter().rev() {
        fixture
            .engine
            .transfer_to_owner(&fixture.authz, EntityId::Memory(memory.id), destination)
            .await?;
    }
    let transferred: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.cooled WHERE owner_id = $1")
            .bind(destination.stored_owner_id())
            .fetch_one(pool)
            .await?;
    assert_eq!(transferred, i64::try_from(memories.len())?);

    // Abandon the destination using the same membership table checked under
    // lock by the public erase path. The source owner and its history remain.
    let removed = sqlx::query("DELETE FROM proxima_core.group_memberships WHERE group_id = $1")
        .bind(destination.stored_owner_id())
        .execute(pool)
        .await?
        .rows_affected();
    assert_eq!(removed, 1);
    let system = AuthzContext::for_subject(
        UserId::new(fixture.owner.stored_owner_id()),
        AuthPath::System,
    );
    let erased = fixture.engine.erase_group_owner(&system, group).await?;
    assert!(
        matches!(
            erased,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: false,
                ..
            }
        ),
        "{erased:?}"
    );
    for memory in memories {
        let (kind, hot, cooled): (String, bool, bool) = sqlx::query_as(
            "SELECT e.kind::text,
                    EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = e.t),
                    EXISTS (SELECT 1 FROM proxima_core.cooled c WHERE c.t = e.t)
               FROM proxima_core.erased_pin_target e WHERE e.t = $1",
        )
        .bind(memory.id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(kind, memory.kind.as_str().to_lowercase());
        assert!(
            !hot && !cooled,
            "only the exact erased-kind witness remains"
        );
        assert!(matches!(
            cold.get(&cold_object_key(memory.id.into_inner())).await,
            Err(StorageError::NotFound)
        ));
    }
    let destination_events = fixture
        .pg
        .list_change_events_after(&[destination], Uuid::nil(), 100)
        .await?;
    assert!(
        destination_events.is_empty(),
        "owner erase legitimately removes its own event lane"
    );
    Ok(())
}

#[tokio::test]
async fn destination_erase_preserves_retained_source_event_kinds() {
    let (fixture, cold) = lifecycle_fixture().await.expect("isolated PG fixture");
    let result: TestResult<_> = async {
        let memories = chain(&fixture).await?;
        cool_chain(&fixture, &cold, &memories).await?;
        erase_after_transfer(&fixture, &cold, &memories).await?;
        let paths = read_event_paths(&fixture).await?;
        Ok((memories, paths))
    }
    .await;
    let owner = fixture.owner;
    fixture.close().await;
    let (memories, (forward, replay)) = result.expect("normal transfer and abandoned-group erase");
    for events in [&forward, &replay] {
        assert_event_identities(
            events,
            &memories,
            owner,
            &[
                EventOperation::Append,
                EventOperation::Delete,
                EventOperation::Transfer,
            ],
        );
    }
}
