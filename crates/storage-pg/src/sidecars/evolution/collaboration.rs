use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use proxiima_core::StorageError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::refused;
use crate::error::map_err;

#[derive(Default)]
struct Mapping {
    forward: HashMap<Uuid, HashSet<Uuid>>,
    backward: HashMap<Uuid, HashSet<Uuid>>,
}
impl Mapping {
    fn add(&mut self, source: Uuid, target: Uuid) {
        self.forward.entry(source).or_default().insert(target);
        self.backward.entry(target).or_default().insert(source);
    }
    fn targets(&self, source: Uuid) -> Vec<Uuid> {
        self.forward
            .get(&source)
            .map_or_else(|| vec![source], |ids| ids.iter().copied().collect())
    }
    fn sources(&self, target: Uuid) -> Vec<Uuid> {
        self.backward
            .get(&target)
            .map_or_else(|| vec![target], |ids| ids.iter().copied().collect())
    }
}

/// A merge can carry an authority only if every contributor had that exact
/// authority. A split carries the same authority to its explicit successors.
/// Differing audiences need an ordinary authorization decision before migration.
fn conserve<K: Copy + Eq + Hash, V: Clone + Eq>(
    rows: &HashMap<K, V>,
    targets: impl Fn(K) -> Vec<K>,
    sources: impl Fn(K) -> Vec<K>,
) -> Result<HashMap<K, V>, StorageError> {
    let mut result = HashMap::new();
    for (&key, value) in rows {
        for target in targets(key) {
            if sources(target)
                .iter()
                .any(|source| rows.get(source) != Some(value))
            {
                return Err(refused(
                    "identity migration cannot merge different collaboration authority",
                ));
            }
            if result
                .insert(target, value.clone())
                .is_some_and(|prior| prior != *value)
            {
                return Err(refused(
                    "identity migration has conflicting collaboration authority",
                ));
            }
        }
    }
    Ok(result)
}

fn pairs(left: &[Uuid], right: &[Uuid]) -> Vec<(Uuid, Uuid)> {
    left.iter()
        .flat_map(|a| right.iter().map(move |b| (*a, *b)))
        .collect()
}

#[derive(Clone, PartialEq, Eq, sqlx::FromRow)]
struct Grant {
    read_ceiling: i16,
    write_ceiling: i16,
    manage: bool,
    controller_generation: i64,
    expires_at: Option<time::OffsetDateTime>,
    granted_by: Uuid,
}
#[derive(sqlx::FromRow)]
struct GrantRow {
    goal_handle: Uuid,
    subject_id: Uuid,
    #[sqlx(flatten)]
    value: Grant,
}
#[derive(Clone, PartialEq, Eq, sqlx::FromRow)]
struct Publication {
    source_owner: Uuid,
    source_generation: i64,
    controller_generation: i64,
    permitted_use: String,
    cited_upload_id: Option<Uuid>,
    published_by: Uuid,
}
#[derive(sqlx::FromRow)]
struct PublicationRow {
    goal_handle: Uuid,
    memory_t: Uuid,
    #[sqlx(flatten)]
    value: Publication,
}
#[derive(Clone, PartialEq, Eq, sqlx::FromRow)]
struct Binding {
    source_owner: Uuid,
    source_generation: i64,
    controller_generation: i64,
    bound_by: Uuid,
}
#[derive(sqlx::FromRow)]
struct BindingRow {
    goal_handle: Uuid,
    memory_handle: Uuid,
    #[sqlx(flatten)]
    value: Binding,
}
#[derive(sqlx::FromRow)]
struct MappedRow {
    record_kind: String,
    source_id: Uuid,
    source_handle: Option<Uuid>,
    target_id: Uuid,
    target_handle: Option<Uuid>,
}

pub(super) async fn preserve(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    sqlx::query("LOCK TABLE proxiima_core.goal_access_grant,proxiima_core.goal_memory_publication,proxiima_core.goal_work_binding IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut **tx).await.map_err(map_err)?;
    let rows: Vec<MappedRow> = sqlx::query_as("WITH RECURSIVE mappings AS (
        SELECT a.record_kind,a.source_id,a.source_handle,a.target_id,a.target_handle,a.target_schema
          FROM proxiima_core.schema_evolution_authority a WHERE a.xact=pg_current_xact_id()
           AND NOT EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority prior
               WHERE prior.xact=a.xact AND prior.record_kind=a.record_kind AND prior.target_id=a.source_id AND prior.target_schema=a.source_schema)
        UNION SELECT p.record_kind,p.source_id,p.source_handle,n.target_id,n.target_handle,n.target_schema
          FROM mappings p JOIN proxiima_core.schema_evolution_authority n ON n.xact=pg_current_xact_id()
           AND n.record_kind=p.record_kind AND n.source_id=p.target_id AND n.source_schema=p.target_schema
    ) SELECT DISTINCT record_kind,source_id,source_handle,target_id,target_handle FROM mappings p
      WHERE NOT EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority n WHERE n.xact=pg_current_xact_id()
        AND n.record_kind=p.record_kind AND n.source_id=p.target_id AND n.source_schema=p.target_schema)")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let mut memory_ids = Mapping::default();
    let mut memory_series = Mapping::default();
    let mut goal_series = Mapping::default();
    for row in rows {
        match row.record_kind.as_str() {
            "memory" => {
                memory_ids.add(row.source_id, row.target_id);
                if let (Some(source), Some(target)) = (row.source_handle, row.target_handle) {
                    memory_series.add(source, target);
                }
            }
            "goal" => {
                if let (Some(source), Some(target)) = (row.source_handle, row.target_handle) {
                    goal_series.add(source, target);
                }
            }
            "blob" => {}
            _ => return Err(refused("unknown migration record kind")),
        }
    }
    validate_generations(tx).await?;
    preserve_closed_series(tx, &memory_series).await?;
    preserve_grants(tx, &goal_series).await?;
    preserve_publications(tx, &goal_series, &memory_ids).await?;
    preserve_bindings(tx, &goal_series, &memory_series).await?;
    Ok(())
}

async fn validate_generations(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    let changed: bool = sqlx::query_scalar("SELECT EXISTS (
        SELECT 1 FROM proxiima_core.schema_evolution_authority a
        JOIN proxiima_core.memory_head source ON source.handle=a.source_handle
        JOIN proxiima_core.memory_head target ON target.handle=a.target_handle
        WHERE a.xact=pg_current_xact_id() AND a.record_kind='memory' AND source.authority_generation<>target.authority_generation
          AND (EXISTS (SELECT 1 FROM proxiima_core.goal_memory_publication p WHERE p.memory_t=a.source_id)
            OR EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding b WHERE b.memory_handle=a.source_handle)))
        OR EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority a
        JOIN proxiima_core.goal_head source ON source.handle=a.source_handle
        JOIN proxiima_core.goal_head target ON target.handle=a.target_handle
        WHERE a.xact=pg_current_xact_id() AND a.record_kind='goal' AND source.authority_generation<>target.authority_generation
          AND (EXISTS (SELECT 1 FROM proxiima_core.goal_access_grant g WHERE g.goal_handle=a.source_handle)
            OR EXISTS (SELECT 1 FROM proxiima_core.goal_memory_publication p WHERE p.goal_handle=a.source_handle)
            OR EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding b WHERE b.goal_handle=a.source_handle)))")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if changed {
        return Err(refused(
            "schema migration cannot change the validity of collaboration grants",
        ));
    }
    Ok(())
}

async fn preserve_closed_series(
    tx: &mut Transaction<'_, Postgres>,
    memories: &Mapping,
) -> Result<(), StorageError> {
    let rows: Vec<(Uuid, time::OffsetDateTime)> =
        sqlx::query_as("SELECT handle,closed_at FROM proxiima_core.closed_handle")
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    let before: HashMap<_, _> = rows.into_iter().collect();
    let after = conserve(&before, |h| memories.targets(h), |h| memories.sources(h))?;
    for (handle, closed_at) in after
        .iter()
        .filter(|(key, value)| before.get(key) != Some(*value))
    {
        sqlx::query("INSERT INTO proxiima_core.closed_handle(handle,closed_at) VALUES($1,$2)")
            .bind(handle)
            .bind(closed_at)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    for handle in before.keys().filter(|key| !after.contains_key(key)) {
        sqlx::query("DELETE FROM proxiima_core.closed_handle WHERE handle=$1")
            .bind(handle)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn preserve_grants(
    tx: &mut Transaction<'_, Postgres>,
    goals: &Mapping,
) -> Result<(), StorageError> {
    let rows: Vec<GrantRow> = sqlx::query_as("SELECT goal_handle,subject_id,read_ceiling,write_ceiling,manage,controller_generation,expires_at,granted_by FROM proxiima_core.goal_access_grant")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let before: HashMap<_, _> = rows
        .into_iter()
        .map(|r| ((r.goal_handle, r.subject_id), r.value))
        .collect();
    let after = conserve(
        &before,
        |(g, s)| goals.targets(g).into_iter().map(|g| (g, s)).collect(),
        |(g, s)| goals.sources(g).into_iter().map(|g| (g, s)).collect(),
    )?;
    for ((goal, subject), row) in after
        .iter()
        .filter(|(key, value)| before.get(key) != Some(*value))
    {
        sqlx::query("INSERT INTO proxiima_core.goal_access_grant(goal_handle,subject_id,read_ceiling,write_ceiling,manage,controller_generation,expires_at,granted_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(goal).bind(subject).bind(row.read_ceiling).bind(row.write_ceiling).bind(row.manage).bind(row.controller_generation).bind(row.expires_at).bind(row.granted_by)
            .execute(&mut **tx).await.map_err(map_err)?;
    }
    for (goal, subject) in before.keys().filter(|key| !after.contains_key(key)) {
        sqlx::query(
            "DELETE FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2",
        )
        .bind(goal)
        .bind(subject)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

async fn preserve_publications(
    tx: &mut Transaction<'_, Postgres>,
    goals: &Mapping,
    memories: &Mapping,
) -> Result<(), StorageError> {
    let rows: Vec<PublicationRow> = sqlx::query_as("SELECT goal_handle,memory_t,source_owner,source_generation,controller_generation,permitted_use::text,cited_upload_id,published_by FROM proxiima_core.goal_memory_publication")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let before: HashMap<_, _> = rows
        .into_iter()
        .map(|r| ((r.goal_handle, r.memory_t), r.value))
        .collect();
    let after = conserve(
        &before,
        |(g, m)| pairs(&goals.targets(g), &memories.targets(m)),
        |(g, m)| pairs(&goals.sources(g), &memories.sources(m)),
    )?;
    for ((goal, memory), row) in after
        .iter()
        .filter(|(key, value)| before.get(key) != Some(*value))
    {
        sqlx::query("INSERT INTO proxiima_core.goal_memory_publication(goal_handle,memory_t,source_owner,source_generation,controller_generation,permitted_use,cited_upload_id,published_by) VALUES($1,$2,$3,$4,$5,$6::text::proxiima_core.goal_publication_use,$7,$8)")
            .bind(goal).bind(memory).bind(row.source_owner).bind(row.source_generation).bind(row.controller_generation).bind(&row.permitted_use).bind(row.cited_upload_id).bind(row.published_by)
            .execute(&mut **tx).await.map_err(map_err)?;
    }
    for (goal, memory) in before.keys().filter(|key| !after.contains_key(key)) {
        sqlx::query("DELETE FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 AND memory_t=$2").bind(goal).bind(memory).execute(&mut **tx).await.map_err(map_err)?;
    }
    Ok(())
}

async fn preserve_bindings(
    tx: &mut Transaction<'_, Postgres>,
    goals: &Mapping,
    memories: &Mapping,
) -> Result<(), StorageError> {
    let rows: Vec<BindingRow> = sqlx::query_as("SELECT goal_handle,memory_handle,source_owner,source_generation,controller_generation,bound_by FROM proxiima_core.goal_work_binding")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let before: HashMap<_, _> = rows
        .into_iter()
        .map(|r| ((r.goal_handle, r.memory_handle), r.value))
        .collect();
    let after = conserve(
        &before,
        |(g, m)| pairs(&goals.targets(g), &memories.targets(m)),
        |(g, m)| pairs(&goals.sources(g), &memories.sources(m)),
    )?;
    for ((goal, memory), row) in after
        .iter()
        .filter(|(key, value)| before.get(key) != Some(*value))
    {
        sqlx::query("INSERT INTO proxiima_core.goal_work_binding(goal_handle,memory_handle,source_owner,source_generation,controller_generation,bound_by) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(goal).bind(memory).bind(row.source_owner).bind(row.source_generation).bind(row.controller_generation).bind(row.bound_by)
            .execute(&mut **tx).await.map_err(map_err)?;
    }
    for (goal, memory) in before.keys().filter(|key| !after.contains_key(key)) {
        sqlx::query(
            "DELETE FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 AND memory_handle=$2",
        )
        .bind(goal)
        .bind(memory)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_requires_the_same_authority_on_every_contributor() {
        let rows = HashMap::from([(1, "read"), (2, "read")]);
        assert_eq!(
            conserve(&rows, |_| vec![3], |_| vec![1, 2]).unwrap(),
            HashMap::from([(3, "read")])
        );
        assert!(conserve(&HashMap::from([(1, "read")]), |_| vec![3], |_| vec![1, 2]).is_err());
        assert!(
            conserve(
                &HashMap::from([(1, "read"), (2, "fork")]),
                |_| vec![3],
                |_| vec![1, 2]
            )
            .is_err()
        );
    }

    #[test]
    fn splitting_preserves_the_exact_authority() {
        let rows = HashMap::from([(1, "read")]);
        assert_eq!(
            conserve(&rows, |_| vec![2, 3], |_| vec![1]).unwrap(),
            HashMap::from([(2, "read"), (3, "read")])
        );
    }
}
