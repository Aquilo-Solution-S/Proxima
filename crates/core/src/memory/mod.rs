//! Core long-term memory payloads.

pub mod payloads;

pub use payloads::{
    AgentDerivationV1, AgentNoteV1, GoalChange, GoalChangeColumnsError, GoalChangeKind,
    GoalChangeV1, InterpretationSubjectKind, InterpretationV1, PendingWork, Speaker, UploadV1,
    UtteranceV1, WriteActV1,
};

use crate::SpiiceRegistry;

pub(crate) fn register_all(
    registry: &mut SpiiceRegistry,
) -> Result<(), crate::SpiiceRegistryError> {
    registry.try_add_fact_schema::<WriteActV1>()?;
    registry.try_add_fact_schema::<GoalChangeV1>()?;
    registry.try_add_fact_schema::<AgentNoteV1>()?;
    registry.try_add_fact_schema::<UtteranceV1>()?;
    registry.try_add_fact_schema::<UploadV1>()?;
    registry.try_add_abstraction_schema::<AgentDerivationV1>()?;
    registry.try_add_perspective_schema::<AgentDerivationV1>()?;
    registry.try_add_perspective_schema::<InterpretationV1>()?;
    Ok(())
}
