use super::*;

struct AppNewReplacement;

impl SpiiceBundle for AppNewReplacement {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register(r, false)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_goal::<GoalV2>();
        let mut schemas = PgMigrationSchemas::new();
        schemas.add_goal_source::<GoalV1>();
        schemas.add_goal::<GoalV2>();
        r.add_evolution(PgSchemaEvolution::new(&EV, schemas, evolve_new));
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator())]
    }
}

fn evolve_new(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.goals() {
        let old = source
            .payload::<GoalV1>()
            .ok_or_else(|| proxiima_core::StorageError::Internal("missing GoalV1".into()))?;
        let payload = GoalPayloadWrite::from_payload(
            source.title(),
            source.text().unwrap_or("goal"),
            GoalV2 {
                key: old.key.clone(),
                label: "v2".into(),
            },
        )
        .map_err(|e| proxiima_core::StorageError::Internal(e.to_string()))?;
        let id = plan.goal(
            source,
            payload,
            MigrationPlacement {
                admission: proxiima::spiice::MigrationAdmission::New,
                series: proxiima::spiice::MigrationSeries::Retain,
            },
            source.pins(),
        )?;
        plan.map_goal(source, id, &[])?;
    }
    Ok(())
}

struct ReplacementFixture {
    db: proxiima_pg_testkit::SplitRoleDb,
    pg: PgMigrationStorage,
    owner: OwnerId,
    authz: AuthzContext,
    topology: GoalTopologyWrite,
    request: IdempotencyKey,
    first: GoalWriteOutcome,
    modified: GoalWriteOutcome,
    abandoned: GoalWriteOutcome,
}

async fn seed_terminal_goal() -> ReplacementFixture {
    let db = split_core_db::clone_split_core_db("proxiima_goal_replacement")
        .await
        .expect("PG required");
    let platform = db.platform_url().to_owned();
    let runtime = db.runtime_url().to_owned();
    let mut ro = SpiiceRegistry::new();
    register(&mut ro, true).unwrap();
    let ro = ro.try_freeze().unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        &platform,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut so = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut so);
    so.add_goal::<GoalV1>();
    let fo = so.clone().freeze_against(&ro).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppOld::migrators(), &fo)
        .await
        .unwrap();
    pg.initialize_store_number(0).await.unwrap();
    let owner = OwnerId::new(Uuid::now_v7());
    let user = UserId::new(Uuid::now_v7());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        user,
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    insert_owner(&admin, owner).await;
    let old = storage(&runtime, &platform, &ro, so).await;
    let engine = proxiima_core::Engine::new(ro)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:goal:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(old).storage_ports());
    let topology = GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap();
    let request = IdempotencyKey::new("goal-replacement-create").unwrap();
    let (first, modified) =
        create_and_modify_old_goal(&engine, &authz, owner, &topology, &request).await;
    let abandoned = engine
        .transition_goal(
            &authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: modified.goal_id,
                transition: GoalTransitionKind::Abandon,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("goal-replacement-abandon").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();

    admin.close().await;
    ReplacementFixture {
        db,
        pg,
        owner,
        authz,
        topology,
        request,
        first,
        modified,
        abandoned,
    }
}

#[tokio::test]
async fn terminal_goal_history_survives_new_admission_identity_replacement() {
    let ReplacementFixture {
        db,
        pg,
        owner,
        authz,
        topology,
        request,
        first,
        modified,
        abandoned,
    } = seed_terminal_goal().await;
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    let platform = db.platform_url().to_owned();
    let runtime = db.runtime_url().to_owned();
    let mut rn = SpiiceRegistry::new();
    register(&mut rn, false).unwrap();
    let rn = rn.try_freeze().unwrap();
    let mut sn = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sn);
    AppNewReplacement::register_pg_sidecars(&mut sn);
    let fn_ = sn.clone().freeze_against(&rn).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppNewReplacement::migrators(), &fn_)
        .await
        .expect("authorized terminal history replacement");
    let terminal = assert_replacement_history(&admin, owner, &first, &modified, &abandoned).await;
    let new = storage(&runtime, &platform, &rn, sn).await;
    let engine2 = proxiima_core::Engine::new(rn)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:goal:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(new).storage_ports());
    let replay = engine2
        .create_goal(
            &authz,
            GoalCreateRequest {
                owner,
                topology,
                title: "goal".into(),
                text: "body".into(),
                payload: GoalV2 {
                    key: "same".into(),
                    label: "v2".into(),
                },
                request_id: request,
                authorship: GoalAuthorship::User,
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();
    assert_ne!(replay.goal_id, first.goal_id);
    assert!(replay.idempotent_replay);
    assert_eq!(replay.goal_handle, first.goal_handle);
    assert!(
        engine2
            .transition_goal(
                &authz,
                &proxiima_core::GoalTransitionRequest {
                    owner,
                    prior_goal_id: terminal,
                    transition: GoalTransitionKind::Pause {
                        pending: PendingWork::Keep
                    },
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("resurrect-terminal").unwrap(),
                    author_self_perspective_id: None
                }
            )
            .await
            .is_err()
    );
}

async fn assert_replacement_history(
    admin: &sqlx::PgPool,
    owner: OwnerId,
    first: &GoalWriteOutcome,
    modified: &GoalWriteOutcome,
    abandoned: &GoalWriteOutcome,
) -> GoalId {
    let states: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t,state::text FROM proxiima_core.goal WHERE owner_id=$1 ORDER BY t")
            .bind(owner.into_inner())
            .fetch_all(admin)
            .await
            .unwrap();
    assert_eq!(states.len(), 3);
    assert_eq!(states.iter().filter(|(_, s)| s == "Active").count(), 2);
    assert_eq!(states.iter().filter(|(_, s)| s == "Abandoned").count(), 1);
    assert_eq!(
        states
            .iter()
            .map(|(_, state)| state.as_str())
            .collect::<Vec<_>>(),
        ["Active", "Active", "Abandoned"]
    );
    let handles: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT handle FROM proxiima_core.goal WHERE owner_id=$1")
            .bind(owner.into_inner())
            .fetch_all(admin)
            .await
            .unwrap();
    assert_eq!(handles, [first.goal_handle.into_inner()]);
    assert!(
        states
            .iter()
            .all(|(id, _)| *id != first.goal_id.into_inner()
                && *id != modified.goal_id.into_inner()
                && *id != abandoned.goal_id.into_inner())
    );
    GoalId::new(states.last().unwrap().0)
}
