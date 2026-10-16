//! Temporary v0.0.35 migration-reset adoption.
//!
//! This module is deleted at the next migration reset after the old ledger is
//! no longer present in supported databases.

use proxima_core::StorageError;
use sqlx::PgConnection;

const BRIDGE: &str = include_str!("v0035_bridge.sql");
pub const PRE_RESET: [(i64, &str); 32] = [
    (
        1,
        "23857ae4efef5e870133d2443b48debd4eb4e67972bc87a5ffdce5c0867d98ed5d11655b7c16f152908b1e4dd34bed83",
    ),
    (
        2,
        "e06b7cae608d7cf0b3d37aad6ce668540ffd042a4981d5e7ff01114d4d4a2d92a3c3d1de9bbf7f90623c3644e917aac4",
    ),
    (
        3,
        "12f6791f63499f45a6af1233b3702a838af7ee8c06b23564a683bf3a6a363f743cc10c922427e3dfc1c2c1c37fd7fab2",
    ),
    (
        4,
        "625f7bf3e2fff4064df91be7be755b9455914f8366bcaa5b80b32afa2d802cdc487ba558851074d6602a1877494db119",
    ),
    (
        5,
        "16f0ec8cbb1f72475d94cf96b0277a56d9fc001ca6c0f2a61a5dc11441d154df2d9448ce5c9a4853b2b8d2bf840ad907",
    ),
    (
        6,
        "7993390bb493b8418a75097fa7090389a8825bdd45131211c65b49512fbe0b9072d93ec324aaaeec64713572baa0da5a",
    ),
    (
        7,
        "71b66c3617c2c6ec4e464e341e92baf89190845d039aa56cac0568a23ec9e157e4678900f91515c421fb63ae4e624436",
    ),
    (
        8,
        "7bb456cbd19b42c25c25cc858dc9d3351da84a65ada27e619a00a99f46f9bae154997a34ae392775388020044dfaced4",
    ),
    (
        9,
        "955f218ceb621cd5c2059927ed784cdcb1d69ebcf2de33aa25a192d6e19eef7b81499312929448ebd60dcafd49248221",
    ),
    (
        10,
        "92c5aedd6502dad5e543f86eb442ce2ccc4b8f741f3eacd91b44115ef2cd3d92aa619a6c805914adeeaf03375c60ef90",
    ),
    (
        11,
        "f1a416c78e9b742c6e082b4b4a7fefaf2884acd5b223d4f9924773286d7991aea52287478a71d475c762df95c4de4f00",
    ),
    (
        12,
        "0d4919414ac221e6414f6a5bfc9dfc7c702ac9ba57c32b2c50dee05f21c8926e1aa7fd3532ed8f19cce7b542bfa9ebb5",
    ),
    (
        13,
        "e36526c79760ef70452448489b5759f5f73fe2665d4666cf1dfd9c6369c59c0248e3c1e15d4d80b56bad83a003b2a828",
    ),
    (
        14,
        "da4c9936f550e401f45b62590e83977369b4702bcb7acf1f74b65adee9c93c6f16f877ee5f90f654fb3affa902e4c353",
    ),
    (
        15,
        "7e8304995ec12cc30a9035a16b5133001e6aa849f8ef7693104b215e057e7d1b62d72641f70d794928f9e3431f2aec2e",
    ),
    (
        16,
        "bc4981a38753b000967dfde7d806c972d51151c4765689aa6464cc64291bcbe0ad9cdb8cea6563f1c41efdb2f7fa46a3",
    ),
    (
        17,
        "e1d3e19c9cf86b07986188f284930144ef76fb622eb933284d6fc7ccbc1955aacd7e52adc3caf2e2fea2415368e3eb4f",
    ),
    (
        18,
        "c4ad7df632d687b9bafe4376301647dfeec7883ebdafe415f301ec64c8b421c956bb068748aa44827d14f3cbfc5eb8ca",
    ),
    (
        19,
        "9613fcef8a00aff7520f5fef49410caa76e6f209f7705e11a8c6aa44fee584cdb74ec191a324322ec79dccb44b7b0b8e",
    ),
    (
        20,
        "0350e5e02630685ba2599bb39dae5b0fc71e8bc025c5f0772ebda115682676abd02ad6cf72c83a6ff46b087ed4b94ce4",
    ),
    (
        21,
        "5a3bdf4910308f9f52bae7032d3b178bae0a2919d54d86ab41bc0377f717ff3e3864cb9989db7a84d88f7dac73969561",
    ),
    (
        22,
        "4539c2b78f5ca23cb9c230a5599dba1449ebb4a70ff84f9bf6a9852e62950e2017cd208d2f4da1dd73d6ce4a9b0aac8b",
    ),
    (
        23,
        "b0f171e86d916b22523284645e26f841f254c90450bf9ce5be0fbb7cd7a2b755dee1c4711140a9ad28a229a8671d57cf",
    ),
    (
        24,
        "64ce636ac755b907ce239dfe65c26f90a040a7797b0ba466632f285a410e6027d61331efeae90b865cdf91f110984a22",
    ),
    (
        25,
        "634012e250701b9f6ade809f3710ab842f762e3877cb2fc3edfb8474387d80b22baa057b74cc08a40562d955d4fddf1d",
    ),
    (
        26,
        "156fd156c499ea43d7d65e5325966ca1d96099d3c0e7830303eb7b1537f188781b830455900e1e545dbac8cfbb75d855",
    ),
    (
        27,
        "2f702be96f1420615f777fb3322caaa8a8d69dec2c2fc0601aff20ec6f6507d649fba3f8ff94168c99d5c83f6195f2f3",
    ),
    (
        28,
        "95f464f1c66d60bdcfcde83bec609bd27b91468703d8ff1c9b19d2a6b71b6bcbac4e4d5f5f364cae5a989cd5f057330f",
    ),
    (
        29,
        "91993346160a9a21a92dbbfcd74402cc74b2b1f7686360ec0d0b6b17c2095aeb44df7cca1402470425cbb812d0f29682",
    ),
    (
        30,
        "eacb811b7abdcc336c4ef95c7ea499855f173071ff7f2e1ab1e36fa39c27c7a9e07e8096838b06f9d37edc33e7feb623",
    ),
    (
        31,
        "96d4d92b454ca8931b7dd7071b0f93cb9cd58d326ca482bd61e699b3dc8525574490beb9ab6103dd1205e5a986b9c1b5",
    ),
    (
        32,
        "e61e479a003a40966553287d491663aa3e656090fc652683fc58ecdff398d62809cf9a1532e03ec18beb9f8b37dac9ce",
    ),
];

#[derive(Debug, Clone, Copy)]
pub(crate) enum BaselineState {
    Fresh,
    Recorded,
    Adopt(crate::CoreAdoption),
}

pub(crate) fn classify(
    rows: &[(i64, Vec<u8>)],
    has_schema: bool,
    baseline_version: i64,
) -> Result<BaselineState, StorageError> {
    let historical: Vec<_> = rows
        .iter()
        .filter(|(version, _)| *version < baseline_version)
        .collect();
    let historical_adoption = (historical.len() == 30 || historical.len() == PRE_RESET.len())
        && historical.iter().zip(PRE_RESET).all(
            |((version, checksum), (expected_version, expected_checksum))| {
                *version == expected_version && *checksum == decode_hex(expected_checksum)
            },
        );
    if !historical.is_empty() && !historical_adoption {
        return Err(StorageError::SchemaResetRequired {
            details: "this release upgrades databases at v0.0.35 or later; upgrade to v0.0.35 first, or reset a development database".to_owned(),
        });
    }
    if rows.iter().any(|(version, _)| *version == baseline_version) {
        return Ok(BaselineState::Recorded);
    }
    if historical.is_empty() && !has_schema {
        return Ok(BaselineState::Fresh);
    }
    if historical_adoption {
        return Ok(BaselineState::Adopt(if historical.len() == 30 {
            crate::CoreAdoption::Bridge
        } else {
            crate::CoreAdoption::Stamp
        }));
    }
    Err(StorageError::SchemaResetRequired {
        details: "this release upgrades databases at v0.0.35 or later; upgrade to v0.0.35 first, or reset a development database".to_owned(),
    })
}

pub(crate) async fn apply(
    connection: &mut PgConnection,
    baseline: &sqlx::migrate::Migration,
    adoption: crate::CoreAdoption,
) -> Result<(), StorageError> {
    let started = std::time::Instant::now();
    match adoption {
        crate::CoreAdoption::Bridge => {
            sqlx::raw_sql(sqlx::AssertSqlSafe(BRIDGE))
                .execute(&mut *connection)
                .await
                .map_err(crate::error::internal)?;
        }
        crate::CoreAdoption::Stamp => {}
    }
    let execution_time = i64::try_from(started.elapsed().as_nanos()).unwrap_or(i64::MAX);
    sqlx::query(
        "INSERT INTO public._sqlx_migrations
             (version, description, success, checksum, execution_time)
         VALUES ($1, $2, true, $3, $4)",
    )
    .bind(baseline.version)
    .bind(baseline.description.as_ref())
    .bind(baseline.checksum.as_ref())
    .bind(execution_time)
    .execute(&mut *connection)
    .await
    .map_err(crate::error::internal)?;
    Ok(())
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("hex"), 16).expect("hex"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{BaselineState, PRE_RESET, classify, decode_hex};
    use crate::CoreAdoption;
    use proxima_core::StorageError;

    fn ledger(count: usize) -> Vec<(i64, Vec<u8>)> {
        PRE_RESET[..count]
            .iter()
            .map(|(version, checksum)| (*version, decode_hex(checksum)))
            .collect()
    }

    #[test]
    fn adoption_classifies_only_supported_ledgers() {
        let baseline = crate::core_migrator()
            .iter()
            .next()
            .expect("baseline")
            .version;
        assert!(matches!(
            classify(&[], false, baseline),
            Ok(BaselineState::Fresh)
        ));
        assert!(matches!(
            classify(&ledger(30), true, baseline),
            Ok(BaselineState::Adopt(CoreAdoption::Bridge))
        ));
        assert!(matches!(
            classify(&ledger(32), true, baseline),
            Ok(BaselineState::Adopt(CoreAdoption::Stamp))
        ));
        assert!(matches!(
            classify(&ledger(29), true, baseline),
            Err(proxima_core::StorageError::SchemaResetRequired { .. })
        ));
        assert!(classify(&[], true, baseline).is_err());
        let mut amended = ledger(30);
        amended[0].1[0] ^= 1;
        assert!(classify(&amended, true, baseline).is_err());
    }

    #[test]
    fn recorded_baseline_still_validates_historical_prefix() {
        let baseline = crate::core_migrator()
            .iter()
            .next()
            .expect("baseline")
            .version;
        let mut valid = ledger(30);
        valid.push((baseline, vec![0]));
        valid.push((baseline + 1, vec![1]));
        assert!(matches!(
            classify(&valid, true, baseline),
            Ok(BaselineState::Recorded)
        ));

        let mut adopted_with_newer = ledger(30);
        adopted_with_newer.push((baseline + 1, vec![1]));
        assert!(matches!(
            classify(&adopted_with_newer, true, baseline),
            Ok(BaselineState::Adopt(CoreAdoption::Bridge))
        ));

        let mut truncated = ledger(29);
        truncated.push((baseline, vec![0]));
        assert!(matches!(
            classify(&truncated, true, baseline),
            Err(StorageError::SchemaResetRequired { .. })
        ));

        let mut amended = valid;
        amended[0].1[0] ^= 1;
        assert!(matches!(
            classify(&amended, true, baseline),
            Err(StorageError::SchemaResetRequired { .. })
        ));
    }

    #[test]
    fn above_baseline_rows_are_fresh_only_without_a_schema() {
        let baseline = crate::core_migrator()
            .iter()
            .next()
            .expect("baseline")
            .version;
        let rows = vec![(baseline + 1, vec![1])];
        assert!(matches!(
            classify(&rows, false, baseline),
            Ok(BaselineState::Fresh)
        ));
        assert!(matches!(
            classify(&rows, true, baseline),
            Err(StorageError::SchemaResetRequired { .. })
        ));
    }
}
