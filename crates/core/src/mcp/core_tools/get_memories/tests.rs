use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{GetMemoriesArgs, GetMemoriesOutput, MAX_BATCH_GET_MEMORIES, get_memories};
use crate::mcp::core_tools::get_memory::GetMemoryOutput;
use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::{MemorySchemaSpec, MemorySnapshot};
use crate::storage_ports::other_stores::{
    DEFAULT_REMOTE_BATCH_DEADLINE, MAX_REMOTE_READS_PER_STORE, OtherStores, OtherStoresError,
    OtherStoresService, RemoteMemory, RemoteRead, RemoteSearch, RemoteSearchResult, StoreCaller,
    StoreInfo,
};
use crate::{EntityKind, MemoryId, MemoryInspectPort, OwnerId, OwnerScope, StorageError, UserId};

const PROGRESS_BOUND: Duration = Duration::from_secs(30);
const REMOTE_WRITER: crate::WriterAttribution = crate::WriterAttribution {
    by: Uuid::from_u128(3),
    via: Some(Uuid::from_u128(4)),
};
type StartedRead = (Uuid, oneshot::Sender<RemoteRead>);

struct ControlledStore {
    owner: OwnerId,
    started: mpsc::UnboundedSender<StartedRead>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

struct ActiveRead<'a>(&'a AtomicUsize);

impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl OtherStores for ControlledStore {
    fn stores<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
    ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn search<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        _store: u16,
        _args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
        Box::pin(async {
            Ok(RemoteSearchResult {
                hits: Vec::new(),
                degraded_to_lexical: false,
            })
        })
    }

    fn read<'a>(
        &'a self,
        caller: StoreCaller<'a>,
        id: Uuid,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
        Box::pin(async move {
            assert_eq!(caller.owner, self.owner, "remote reads retain the caller");
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveRead(&self.active);
            let (answer, reply) = oneshot::channel();
            self.started
                .send((id, answer))
                .expect("read observer lives");
            reply
                .await
                .map_err(|_| OtherStoresError::Unavailable("reply channel closed".into()))
        })
    }
}

struct LocalStore {
    memory: MemorySnapshot,
    requested: Mutex<Vec<Vec<MemoryId>>>,
}

#[async_trait::async_trait]
impl MemoryInspectPort for LocalStore {
    async fn load_memory_by_id(
        &self,
        _scope: Option<&OwnerScope>,
        id: MemoryId,
        _schemas: &[MemorySchemaSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok((id == self.memory.memory_id).then(|| self.memory.clone()))
    }

    async fn load_memories_by_ids(
        &self,
        _scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        ids: &[MemoryId],
        _schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        self.requested
            .lock()
            .expect("local request log")
            .push(ids.to_vec());
        Ok(ids
            .iter()
            .filter(|id| **id == self.memory.memory_id)
            .filter(|_| read_owners.contains(&self.memory.owner))
            .map(|_| self.memory.clone())
            .collect())
    }
}

struct Fixture {
    ctx: McpToolCtx,
    port: Arc<ControlledStore>,
    receiver: mpsc::UnboundedReceiver<StartedRead>,
    local: Arc<LocalStore>,
}

fn fixture() -> Fixture {
    let mut ctx = ctx_for(UserId::new(Uuid::now_v7()), Vec::new());
    ctx.authz = crate::test_fixtures::authenticated_context(ctx.authz);
    let local = Arc::new(LocalStore {
        memory: MemorySnapshot {
            memory_id: MemoryId::new(crate::new_memory_uuid(0)),
            kind: EntityKind::Fact,
            schema_id: crate::SchemaId::new("test/local-note".into()),
            schema_version: crate::SchemaVersion::new(1),
            owner: ctx.owner,
            writer: crate::WriterAttribution {
                by: Uuid::from_u128(2),
                via: None,
            },
            occurred_at: Some(time::OffsetDateTime::UNIX_EPOCH),
            text: Some("local body".into()),
            tags: Vec::new(),
            payload: None,
        },
        requested: Mutex::new(Vec::new()),
    });
    ctx.engine = Some(Arc::new(
        crate::Engine::new(ctx.registry.as_ref().clone()).with_storage_ports(
            crate::StoragePorts::rejecting_with_memory_inspect(local.clone()),
        ),
    ));
    let (started, receiver) = mpsc::unbounded_channel();
    let port = Arc::new(ControlledStore {
        owner: ctx.owner,
        started,
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    // Keep the individual timeout beyond the call-wide deadline so only the batch bound can win.
    let service =
        OtherStoresService::new(port.clone()).with_timeout(DEFAULT_REMOTE_BATCH_DEADLINE * 2);
    ctx.services = crate::FlavorServices::with(service);
    Fixture {
        ctx,
        port,
        receiver,
        local,
    }
}

async fn started_reads(
    call: &mut (impl Future<Output = Result<GetMemoriesOutput, McpToolError>> + Unpin),
    receiver: &mut mpsc::UnboundedReceiver<StartedRead>,
    count: usize,
) -> Vec<StartedRead> {
    tokio::time::timeout(PROGRESS_BOUND, async {
        let mut started = Vec::with_capacity(count);
        while started.len() < count {
            tokio::select! {
                next = receiver.recv() => started.push(next.expect("read request")),
                result = &mut *call => panic!("batch ended before its reads started: {result:?}"),
            }
        }
        started
    })
    .await
    .expect("remote reads start concurrently")
}

fn memory(id: Uuid) -> RemoteMemory {
    RemoteMemory {
        handle: id,
        writer: REMOTE_WRITER,
        kind: EntityKind::Fact,
        schema_id: "remote/note".into(),
        title: Some("remote title".into()),
        body: "remote body".into(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        tags: vec!["remote".into()],
    }
}

#[tokio::test(start_paused = true)]
async fn slow_remote_batch_finishes_at_one_deadline() {
    let Fixture {
        ctx,
        port,
        mut receiver,
        local,
    } = fixture();
    let requested = (0..MAX_BATCH_GET_MEMORIES)
        .map(|_| crate::new_memory_uuid(1).to_string())
        .collect::<Vec<_>>();
    let call = get_memories(
        ctx,
        GetMemoriesArgs {
            memories: requested.clone(),
        },
    );
    tokio::pin!(call);
    let started_at = tokio::time::Instant::now();
    let pending = started_reads(&mut call, &mut receiver, MAX_REMOTE_READS_PER_STORE).await;
    assert_eq!(
        port.active.load(Ordering::SeqCst),
        MAX_REMOTE_READS_PER_STORE
    );

    // Every started read stays pending; expire the batch after its positive start markers.
    tokio::time::advance(DEFAULT_REMOTE_BATCH_DEADLINE).await;
    let output = tokio::time::timeout(PROGRESS_BOUND, call)
        .await
        .expect("one deadline bounds the entire batch")
        .expect("unanswered reads become missing");
    assert_eq!(started_at.elapsed(), DEFAULT_REMOTE_BATCH_DEADLINE);
    assert!(output.memories.is_empty());
    assert_eq!(output.missing, requested);
    assert_eq!(port.peak.load(Ordering::SeqCst), MAX_REMOTE_READS_PER_STORE);
    assert_eq!(
        port.active.load(Ordering::SeqCst),
        0,
        "pending reads cancelled"
    );
    assert_eq!(
        *local.requested.lock().expect("local request log"),
        vec![Vec::<MemoryId>::new()]
    );
    drop(pending);
}

#[tokio::test(start_paused = true)]
async fn batch_preserves_request_order_and_omits_erased_and_mismatched_answers() {
    let Fixture {
        ctx,
        port,
        mut receiver,
        local,
    } = fixture();
    let first = crate::new_memory_uuid(1);
    let local_found = local.memory.memory_id.into_inner();
    let local_missing = crate::new_memory_uuid(0);
    let second = crate::new_memory_uuid(2);
    let third = crate::new_memory_uuid(1);
    let mismatched = crate::new_memory_uuid(2);
    let erased = crate::new_memory_uuid(1);
    let requested = [
        first,
        local_found,
        local_missing,
        second,
        third,
        mismatched,
        first,
        erased,
    ];
    let call = get_memories(
        ctx,
        GetMemoriesArgs {
            memories: requested.iter().map(ToString::to_string).collect(),
        },
    );
    tokio::pin!(call);
    let started = started_reads(&mut call, &mut receiver, 5).await;
    for (id, answer) in started.into_iter().rev() {
        let reply = if id == erased {
            RemoteRead::Erased
        } else if id == mismatched {
            RemoteRead::Found(memory(crate::new_memory_uuid(2)))
        } else {
            RemoteRead::Found(memory(id))
        };
        answer.send(reply).expect("waiting read");
    }
    let output = tokio::time::timeout(PROGRESS_BOUND, call)
        .await
        .expect("all replies were sent")
        .expect("batch read");
    assert_eq!(
        output
            .memories
            .iter()
            .map(|row| row.memory.as_str())
            .collect::<Vec<_>>(),
        [first, local_found, second, third]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        output.missing,
        [local_missing, mismatched, erased]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        output
            .memories
            .iter()
            .map(|row| row.space.as_str())
            .collect::<Vec<_>>(),
        ["1", "current", "2", "1"]
    );
    for row in output.memories {
        assert_read_projection(&row, &local.memory);
    }
    assert_eq!(port.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        *local.requested.lock().expect("local request log"),
        vec![vec![
            MemoryId::new(local_found),
            MemoryId::new(local_missing)
        ]]
    );
}

fn assert_read_projection(row: &GetMemoryOutput, local: &MemorySnapshot) {
    assert_eq!(row.handle, row.memory);
    assert_eq!(row.kind, "Fact");
    let writer = if row.memory == local.memory_id.into_inner().to_string() {
        assert_eq!(row.body.as_deref(), Some("local body"));
        assert_eq!(row.occurred_at, Some(time::OffsetDateTime::UNIX_EPOCH));
        local.writer
    } else {
        assert_eq!(row.title.as_deref(), Some("remote title"));
        assert_eq!(row.body.as_deref(), Some("remote body"));
        assert_eq!(row.tags, ["remote"]);
        REMOTE_WRITER
    };
    assert_eq!((row.by, row.via), (writer.by, writer.via));
}
