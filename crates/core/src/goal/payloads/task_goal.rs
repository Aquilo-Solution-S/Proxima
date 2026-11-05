use crate::{
    DurablePayloadBuilder, GoalPayload, KeyField, PayloadDisplay, PayloadKeyBuilder,
    PayloadProjection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxiima_core.task_priority")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

impl TaskPriority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskGoalV1 {
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<time::OffsetDateTime>,
    pub priority: Option<TaskPriority>,
}

impl GoalPayload for TaskGoalV1 {
    const SCHEMA_ID: &'static str = "core/task-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_option_time("due_at", self.due_at);
        key.field_option_str("priority", self.priority.map(TaskPriority::as_str));
        key.finish()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_core.task_goal_v1")
    }
}

impl PayloadProjection for TaskGoalV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay::default()
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field(
                "due_at",
                &self
                    .due_at
                    .map(|value| value.format(&time::format_description::well_known::Rfc3339))
                    .transpose()
                    .map_err(|error| error.to_string())?,
            )?
            .field("priority", &self.priority.map(TaskPriority::as_str))?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_matches_the_rfc3339_wire_shape() {
        let value = TaskGoalV1 {
            due_at: Some(time::OffsetDateTime::UNIX_EPOCH),
            priority: Some(TaskPriority::High),
        };
        let durable = value.durable().expect("durable").canonical_bytes();
        let wire = serde_json::to_vec(&value).expect("wire");
        assert_eq!(
            durable,
            crate::canonical_json_bytes(&serde_json::from_slice(&wire).expect("json"))
        );
    }
}
