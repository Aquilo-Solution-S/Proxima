//! Core Goal schemas.

pub mod payloads;

pub use payloads::{
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalPausedV1, SimpleTextGoalV1, TaskGoalV1,
    TaskPriority,
};

pub(crate) fn register_all(
    registry: &mut crate::FlavorRegistry,
) -> Result<(), crate::FlavorRegistryError> {
    registry.try_add_goal_schema::<SimpleTextGoalV1>()?;
    registry.try_add_goal_schema::<TaskGoalV1>()?;
    Ok(())
}
