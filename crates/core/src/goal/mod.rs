//! Core Goal schemas and relations.

pub mod payloads;
pub mod relations;

pub use payloads::{
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalPausedV1, SimpleTextGoalV1, TaskGoalV1,
    TaskPriority,
};
pub use relations::{CORE_MOTIVATED_BY_RELATION, motivated_by_descriptor};

pub(crate) fn register_all(
    registry: &mut crate::FlavorRegistry,
) -> Result<(), crate::FlavorRegistryError> {
    registry.try_add_goal_schema::<SimpleTextGoalV1>()?;
    registry.try_add_goal_schema::<TaskGoalV1>()?;
    registry.try_add_fact_schema::<GoalActivatedV1>()?;
    registry.try_add_fact_schema::<GoalPausedV1>()?;
    registry.try_add_fact_schema::<GoalAchievedV1>()?;
    registry.try_add_fact_schema::<GoalAbandonedV1>()?;
    registry.try_add_relation(motivated_by_descriptor())?;
    Ok(())
}
