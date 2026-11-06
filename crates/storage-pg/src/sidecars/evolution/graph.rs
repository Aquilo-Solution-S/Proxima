use proxiima_core::StorageError;
use sqlx::{Postgres, Transaction};

use super::refused;
use crate::error::map_err;

/// Validate the committed representation after every participating spiice has
/// rewritten its own references. A retained witness supplies kind, never new
/// grounding: its pin must have existed in the captured source declaration.
pub(super) async fn validate(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    let invalid: bool = sqlx::query_scalar("SELECT EXISTS (
        SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (migration validates retained history) */
        WHERE EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority a WHERE a.xact=pg_current_xact_id() AND a.record_kind='memory' AND a.target_id=m.t)
        AND (
          (m.kind='abstraction' AND cardinality(m.origins)=0)
          OR (m.kind<>'fact' AND cardinality(m.origins||m.refs)=0)
          OR (m.blob_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxiima_core.blob b WHERE b.blob_id=m.blob_id AND b.owner_id=m.owner_id))
          OR EXISTS (
            SELECT 1 FROM (
              SELECT id,'origin' role FROM unnest(m.origins) p(id)
              UNION ALL SELECT id,'reference' FROM unnest(m.refs) p(id)
              UNION ALL SELECT id,'goal' FROM unnest(m.goal_refs) p(id)
              UNION ALL SELECT id,'guidance' FROM unnest(m.guided_by) p(id)
              UNION ALL SELECT m.forked_from,'fork' WHERE m.forked_from IS NOT NULL
            ) pin
            LEFT JOIN proxiima_core.memory target /* FORGET-POLICY: includes-forgotten (retained pin targets) */ ON target.t=pin.id
            LEFT JOIN proxiima_core.goal target_goal ON target_goal.t=pin.id
            LEFT JOIN proxiima_core.erased_pin_target witness ON witness.t=pin.id
            CROSS JOIN LATERAL (SELECT COALESCE(target.kind::text,CASE WHEN target_goal.t IS NOT NULL THEN 'goal' END,witness.kind::text) kind) actual
            WHERE (target.t IS NULL AND target_goal.t IS NULL AND NOT (
                    witness.t IS NOT NULL AND proxiima_core.schema_evolution_existing_pin_permits('memory',m.t,pin.id)))
               OR (pin.role IN ('origin','reference','fork') AND pin.id>=m.t)
               OR NOT COALESCE(CASE pin.role
                    WHEN 'origin' THEN (m.kind='abstraction' AND actual.kind IN ('fact','abstraction')) OR (m.kind='perspective' AND actual.kind='abstraction')
                    WHEN 'reference' THEN actual.kind IN ('fact','abstraction','perspective') AND (m.kind<>'fact' OR actual.kind='fact')
                    WHEN 'goal' THEN actual.kind='goal'
                    WHEN 'guidance' THEN m.kind='perspective' AND actual.kind='goal' AND (target_goal.t IS NULL OR target_goal.owner_id=m.owner_id)
                    WHEN 'fork' THEN actual.kind=m.kind::text
                  END,false)
          )
        ))")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if invalid {
        return Err(refused(
            "migration target violates provenance, layer, citation, or pin time",
        ));
    }
    validate_goal_pins(tx).await?;
    validate_goal_parents(tx).await
}

async fn validate_goal_pins(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    let invalid: bool = sqlx::query_scalar("SELECT EXISTS (
        SELECT 1 FROM proxiima_core.goal g
        CROSS JOIN LATERAL (
          SELECT g.assignment_t id,'assignment' role WHERE g.assignment_t IS NOT NULL
          UNION ALL SELECT g.close_fact_t,'close' WHERE g.close_fact_t IS NOT NULL
          UNION ALL SELECT g.write_act_t,'write' WHERE g.write_act_t IS NOT NULL
          UNION ALL SELECT id,'evidence' FROM unnest(g.evidence_t) p(id)
          UNION ALL SELECT id,'dependency' FROM unnest(g.dependency_t) p(id)
          UNION ALL SELECT id,'motivation' FROM unnest(g.motivated_by) p(id)
        ) pin
        LEFT JOIN proxiima_core.memory target /* FORGET-POLICY: includes-forgotten (historical Goal pins) */ ON target.t=pin.id
        LEFT JOIN proxiima_core.goal target_goal ON target_goal.t=pin.id
        LEFT JOIN proxiima_core.erased_pin_target witness ON witness.t=pin.id
        CROSS JOIN LATERAL (SELECT COALESCE(target.kind::text,CASE WHEN target_goal.t IS NOT NULL THEN 'goal' END,witness.kind::text) kind) actual
        WHERE EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority a WHERE a.xact=pg_current_xact_id() AND a.record_kind='goal' AND a.target_id=g.t)
          AND ((target.t IS NULL AND target_goal.t IS NULL AND NOT (
                 witness.t IS NOT NULL AND proxiima_core.schema_evolution_existing_pin_permits('goal',g.t,pin.id)))
            OR NOT COALESCE(CASE pin.role
                 WHEN 'assignment' THEN actual.kind='perspective' AND (target.t IS NULL OR target.owner_id=g.owner_id)
                 WHEN 'motivation' THEN actual.kind='perspective' AND (target.t IS NULL OR target.owner_id=g.owner_id)
                 WHEN 'close' THEN actual.kind='fact' AND (target.t IS NULL OR target.owner_id=g.owner_id)
                 WHEN 'write' THEN actual.kind='fact' AND (target.t IS NULL OR target.owner_id=g.owner_id)
                 WHEN 'evidence' THEN actual.kind IN ('fact','abstraction')
                 WHEN 'dependency' THEN actual.kind='goal'
               END,false)))")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if invalid {
        return Err(refused(
            "migration target violates Goal pin kind, ownership, or retained provenance",
        ));
    }
    Ok(())
}

async fn validate_goal_parents(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    let invalid: bool = sqlx::query_scalar("SELECT
        EXISTS (SELECT 1 FROM proxiima_core.goal g LEFT JOIN proxiima_core.goal_head parent ON parent.handle=g.parent_handle
                   WHERE g.parent_handle IS NOT NULL AND (parent.handle IS NULL OR parent.owner_id<>g.owner_id))
        OR EXISTS (SELECT 1 FROM proxiima_core.goal GROUP BY handle HAVING count(DISTINCT parent_handle)>1 OR (count(parent_handle)>0 AND count(parent_handle)<count(*)))
        OR EXISTS (SELECT 1 FROM proxiima_core.goal terminal JOIN proxiima_core.goal later ON later.handle=terminal.handle AND later.t>terminal.t
                   WHERE terminal.state IN ('Achieved','Abandoned'))
        OR EXISTS (SELECT 1 FROM proxiima_core.goal child JOIN proxiima_core.goal_head ch ON ch.t=child.t
                   JOIN proxiima_core.goal_head ph ON ph.handle=child.parent_handle JOIN proxiima_core.goal parent ON parent.t=ph.t
                   WHERE child.state IN ('Active','Paused') AND parent.state IN ('Achieved','Abandoned'))")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if invalid {
        return Err(refused(
            "migration target violates Goal parents or lifecycle",
        ));
    }
    let cycle: bool = sqlx::query_scalar("WITH RECURSIVE ancestors AS (
        SELECT DISTINCT handle,parent_handle,ARRAY[handle] path,false cycle FROM proxiima_core.goal WHERE parent_handle IS NOT NULL
        UNION ALL SELECT a.handle,g.parent_handle,a.path||g.handle,g.handle=ANY(a.path)
          FROM ancestors a JOIN proxiima_core.goal_head h ON h.handle=a.parent_handle JOIN proxiima_core.goal g ON g.t=h.t
          WHERE NOT a.cycle AND a.parent_handle IS NOT NULL
    ) SELECT EXISTS (SELECT 1 FROM ancestors WHERE cycle)")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if cycle {
        return Err(refused("migration creates a Goal parent cycle"));
    }
    Ok(())
}
