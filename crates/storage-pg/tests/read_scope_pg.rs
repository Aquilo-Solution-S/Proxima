mod common;

use common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::Storage;
use proxima_core::personality::{
    InstantiatePersonalityRequest, ListReadScopeRequest, SetReadScopeRequest,
};

#[tokio::test]
async fn read_scope_replace_lists_explicit_non_identity_grants()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let reader = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            display_name: "Reader".into(),
            purpose: "read scope test".into(),
        })
        .await?
        .instance_id;
    let readable = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            display_name: "Readable".into(),
            purpose: "read scope test".into(),
        })
        .await?
        .instance_id;

    let set = pg
        .set_read_scope(&SetReadScopeRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            reader_personality_instance_id: reader,
            readable_personality_instance_ids: vec![reader, readable, readable],
        })
        .await?;
    assert_eq!(set.readable_count, 1);

    let listed = pg
        .list_read_scope(&ListReadScopeRequest {
            principal: owner.principal,
            org_id: Some(owner.org_id),
            reader_personality_instance_id: reader,
        })
        .await?;
    assert_eq!(listed.readable_personality_instance_ids, vec![readable]);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
