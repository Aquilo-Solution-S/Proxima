//! Derived authoring supersedes: a revision of an Abstraction becomes its
//! series' live head, and carries its origin and reference edges with it.
//! Through the example flavor's digest.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima_core::{
    AuthPath, AuthzContext, DerivationIdentity, DerivedMemory, FactWrite, InputContractId,
    MemoryId, MemoryTarget, OperatorId, OwnerId, SeriesHandle, UserId,
};
use proxima_example::{DigestV1, NoteV1};
use uuid::Uuid;

fn identity() -> DerivationIdentity {
    DerivationIdentity::new(
        OperatorId::new(Uuid::now_v7()),
        InputContractId::new(Uuid::now_v7()),
    )
}

async fn note(
    h: &ExampleEngine,
    authz: &AuthzContext,
    owner: OwnerId,
    notebook_id: Uuid,
    key: &str,
) -> MemoryId {
    h.engine
        .ingest_fact(
            authz,
            FactWrite::new(
                owner,
                "derived-supersession",
                &NoteV1 {
                    notebook_id,
                    key: key.to_owned(),
                    body: key.to_owned(),
                    written_at: time::OffsetDateTime::now_utc(),
                },
            ),
        )
        .await
        .expect("the note is admitted")
        .memory_id
}

fn digest(notebook_id: Uuid, summary: &str) -> DigestV1 {
    DigestV1 {
        notebook_id,
        summary: summary.to_owned(),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn digest_can_use_core_superseding_derived_authoring() {
    let h = ExampleEngine::new("proxima_derived_supersession", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        user,
        AuthPath::HostBearer,
    ));
    let notebook = h.notebook(&owner).await;

    // The Abstraction the digest derives from, itself derived from a note.
    let source_note = note(&h, &authz, owner, notebook, "source").await;
    let source = h
        .engine
        .derive_memory(
            &authz,
            DerivedMemory::abstraction(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "source digest",
                digest(notebook, "source digest"),
                [source_note],
                identity(),
            )
            .expect("typed source digest request"),
        )
        .await
        .expect("source digest authored")
        .memory_id;

    // The Facts the digest references. Both exist before it is written —
    // an edge cannot point at a node that is not there.
    let activation = note(&h, &authz, owner, notebook, "goal activation").await;
    let request = note(&h, &authz, owner, notebook, "work requested").await;

    // A→A originates from an Abstraction; the two Facts arrive as
    // references.
    let old_handle = SeriesHandle::new(Uuid::now_v7());
    let old = h
        .engine
        .derive_memory(
            &authz,
            DerivedMemory::abstraction(
                MemoryTarget::Series(old_handle),
                owner,
                "old digest",
                digest(notebook, "old digest"),
                [source],
                identity(),
            )
            .expect("typed old digest request")
            .refs([activation, request]),
        )
        .await
        .expect("old digest authored")
        .memory_id;
    let new = h
        .engine
        .derive_memory(
            &authz,
            DerivedMemory::abstraction(
                MemoryTarget::Revision(old),
                owner,
                "new digest",
                digest(notebook, "new digest"),
                [source],
                identity(),
            )
            .expect("typed revised digest request")
            .refs([activation, request]),
        )
        .await
        .expect("new digest authored")
        .memory_id;

    let current: Vec<Uuid> = sqlx::query_scalar(
        "SELECT d.t
           FROM proxima_example.digest_v1 d
           JOIN proxima_core.memory_head h ON h.t = d.t
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND m.handle = $2
          ORDER BY m.t DESC",
    )
    .bind(owner.into_inner())
    .bind(old_handle.into_inner())
    .fetch_all(&h.pool)
    .await
    .expect("query current digests");
    assert_eq!(
        current,
        vec![new.into_inner()],
        "the revision is the live head and the superseded digest is not"
    );

    // A read of the superseded version at its series head answers with the
    // revision; the exact read still answers with the version as written.
    for (at, expected) in [
        (proxima::host::ReadAt::SeriesHead, new),
        (proxima::host::ReadAt::Version, old),
    ] {
        let read = h
            .engine
            .get_memory(
                &authz,
                &proxima_core::GetMemoryReadRequest {
                    memory_id: old,
                    include_neighbor_edges: false,
                    at,
                },
            )
            .await
            .expect("memory read");
        let snapshot = read.memory.expect("the version is readable");
        assert_eq!(snapshot.memory_id, expected);
        assert_eq!(snapshot.series, old_handle);
    }

    // The revision keeps its edges: one origin, two references.
    let (origins, refs): (Vec<Uuid>, Vec<Uuid>) =
        sqlx::query_as("SELECT origins, refs FROM proxima_core.memory WHERE t = $1")
            .bind(new.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the revision's edges");
    assert_eq!(origins, vec![source.into_inner()]);
    for referenced in [activation.into_inner(), request.into_inner()] {
        assert!(
            refs.contains(&referenced),
            "the revision references {referenced}: {refs:?}"
        );
    }
    h.pool.close().await;
}
