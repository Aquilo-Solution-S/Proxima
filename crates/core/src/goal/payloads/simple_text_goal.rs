use crate::{
    DurablePayloadBuilder, GoalPayload, KeyField, PayloadDisplay, PayloadProjection,
    schema_only_key,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SimpleTextGoalV1 {}

impl GoalPayload for SimpleTextGoalV1 {
    const SCHEMA_ID: &'static str = "core/simple-text-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        schema_only_key(Self::SCHEMA_ID, Self::SCHEMA_VERSION)
    }
}

impl PayloadProjection for SimpleTextGoalV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay::default()
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        Ok(DurablePayloadBuilder::new().finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn durable_is_the_empty_wire_object() {
        let value = SimpleTextGoalV1 {};
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&serde_json::to_value(&value).expect("wire"))
        );
    }
}
