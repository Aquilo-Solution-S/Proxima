//! The `retention` rows follow their owner: owner export carries them and
//! owner erase takes them, leaving every other owner's rows.

use proxiima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxiima_core::storage_ports::OwnerInversePort;
use proxiima_core::{SpiiceRegistry, UserId};
use proxiima_pg_testkit::{db_url, drop_db};
use proxiima_storage_pg::PgStorage;
use proxiima_storage_pg::test_fixtures::create_core_db;
use uuid::Uuid;

fn surfaces() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&SpiiceRegistry::new().freeze_or_panic_for_tests())
}

async fn person_with_rows(pool: &sqlx::PgPool) -> Result<UserId, sqlx::Error> {
    let person = Uuid::now_v7();
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(person)
        .execute(pool)
        .await?;
    for (schema, days) in [("s.mail-v1", 30), ("s.page-v1", 365)] {
        sqlx::query(
            "INSERT INTO proxiima_core.retention (owner_id, schema_id, keep_days)
             VALUES ($1, $2, $3)",
        )
        .bind(person)
        .bind(schema)
        .bind(days)
        .execute(pool)
        .await?;
    }
    Ok(UserId::new(person))
}

async fn rows_of(pool: &sqlx::PgPool, owner: UserId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxiima_core.retention WHERE owner_id = $1")
        .bind(owner.into_inner())
        .fetch_one(pool)
        .await
}

#[tokio::test]
async fn owner_export_carries_the_rows_and_owner_erase_takes_them()
-> Result<(), Box<dyn std::error::Error>> {
    let name = format!("proxiima_test_retention_{}", Uuid::now_v7().simple());
    create_core_db(&name).await.expect("create test database");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&name)).await?;
        let pool = pg.pool_for_tests();
        let person = person_with_rows(pool).await?;
        let neighbour = person_with_rows(pool).await?;

        let export = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: person,
        });
        let bundle = pg.export_owner_bundle(&export, &surfaces()).await?;
        let mut exported: Vec<(String, i64)> = bundle.tables["proxiima_core.retention"]
            .iter()
            .map(|row| {
                assert_eq!(row["owner_id"], person.into_inner().to_string());
                (
                    row["schema_id"].as_str().unwrap_or_default().to_owned(),
                    row["keep_days"].as_i64().unwrap_or_default(),
                )
            })
            .collect();
        exported.sort();
        assert_eq!(
            exported,
            [("s.mail-v1".to_owned(), 30), ("s.page-v1".to_owned(), 365)],
            "the bundle carries the owner's rows and no neighbour's"
        );

        let erase = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: person,
            drop_event_id: "retention-test".into(),
        });
        let erased = pg.erase_personal_owner(&erase, person, &surfaces()).await?;
        assert!(
            matches!(erased, OwnerEraseOutcome::Completed { .. }),
            "{erased:?}"
        );
        assert_eq!(rows_of(pool, person).await?, 0, "the erase takes the rows");
        assert_eq!(
            rows_of(pool, neighbour).await?,
            2,
            "a neighbour keeps its rows"
        );
        Ok(())
    }
    .await;
    drop_db(&name).await.ok();
    result
}
