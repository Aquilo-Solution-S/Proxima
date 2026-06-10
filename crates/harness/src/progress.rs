use std::collections::{BTreeSet, HashMap};

use proxima_core::harness::{
    FULFILLMENT_REMINDER_INTERVAL_ROUNDS, FULFILLMENT_STALL_ROUND_LIMIT, TOOL_ERROR_STREAK_LIMIT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FulfillmentMatch {
    pub tool_name: String,
    pub produced_schema_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolErrorStreak {
    pub tool_name: String,
    pub message: String,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct FulfillmentProgress {
    required_schema_ids: BTreeSet<String>,
    tool_productions: HashMap<String, Vec<String>>,
    last_tool_error: Option<ToolErrorStreak>,
}

impl FulfillmentProgress {
    #[must_use]
    pub fn new(
        tool_productions: &HashMap<String, Vec<String>>,
        required_fulfillment_schema_ids: &[String],
    ) -> Self {
        let required_schema_ids = if required_fulfillment_schema_ids.is_empty() {
            tool_productions
                .values()
                .flat_map(|schemas| schemas.iter().cloned())
                .collect::<BTreeSet<_>>()
        } else {
            required_fulfillment_schema_ids.iter().cloned().collect()
        };
        Self {
            required_schema_ids,
            tool_productions: tool_productions.clone(),
            last_tool_error: None,
        }
    }

    #[must_use]
    pub fn durable_required(&self) -> bool {
        !self.required_schema_ids.is_empty()
    }

    #[must_use]
    pub fn required_schema_ids(&self) -> Vec<String> {
        self.required_schema_ids.iter().cloned().collect()
    }

    #[must_use]
    pub fn successful_tool_fulfills(&self, tool_name: &str) -> Option<FulfillmentMatch> {
        let produced = self.tool_productions.get(tool_name)?;
        let matched = produced
            .iter()
            .filter(|schema_id| self.required_schema_ids.contains(*schema_id))
            .cloned()
            .collect::<Vec<_>>();
        if matched.is_empty() {
            return None;
        }
        Some(FulfillmentMatch {
            tool_name: tool_name.to_string(),
            produced_schema_ids: matched,
        })
    }

    pub fn note_success(&mut self) {
        self.last_tool_error = None;
    }

    pub fn note_tool_error(&mut self, tool_name: &str, message: &str) -> Option<ToolErrorStreak> {
        let next = match self.last_tool_error.take() {
            Some(mut streak) if streak.tool_name == tool_name && streak.message == message => {
                streak.count = streak.count.saturating_add(1);
                streak
            }
            _ => ToolErrorStreak {
                tool_name: tool_name.to_string(),
                message: message.to_string(),
                count: 1,
            },
        };
        let failed = (next.count >= TOOL_ERROR_STREAK_LIMIT).then(|| next.clone());
        self.last_tool_error = Some(next);
        failed
    }

    #[must_use]
    pub fn should_remind(&self, round_idx: u32) -> bool {
        self.durable_required()
            && round_idx > 0
            && round_idx.is_multiple_of(FULFILLMENT_REMINDER_INTERVAL_ROUNDS)
    }

    #[must_use]
    pub fn is_stalled_after(&self, rounds_used: u32) -> bool {
        self.durable_required() && rounds_used >= FULFILLMENT_STALL_ROUND_LIMIT
    }

    #[must_use]
    pub fn reminder(&self, round_idx: u32) -> String {
        format!(
            "Fulfillment reminder: this wake requires one durable result with any required produced schema in [{}]. This is round {round_idx}; stop immediately after emitting the required durable artifact.",
            self.required_schema_ids().join(", ")
        )
    }

    #[must_use]
    pub fn stall_reason(&self) -> String {
        format!(
            "fulfillment_stalled:no durable result from required produced schemas [{}] after {FULFILLMENT_STALL_ROUND_LIMIT} rounds",
            self.required_schema_ids().join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn productions() -> HashMap<String, Vec<String>> {
        HashMap::from([
            (
                "core/emit_abstraction::test/derivation-v1::v1".into(),
                vec!["test/derivation-v1".into()],
            ),
            ("core/fetch_memory".into(), Vec::new()),
        ])
    }

    #[test]
    fn typed_emit_success_fulfills_required_schema() {
        let progress = FulfillmentProgress::new(&productions(), &[]);

        let matched = progress
            .successful_tool_fulfills("core/emit_abstraction::test/derivation-v1::v1")
            .expect("typed emit should fulfill");

        assert_eq!(matched.produced_schema_ids, vec!["test/derivation-v1"]);
        assert!(
            progress
                .successful_tool_fulfills("core/fetch_memory")
                .is_none()
        );
    }

    #[test]
    fn repeated_identical_tool_error_fails_on_third_attempt() {
        let mut progress = FulfillmentProgress::new(&productions(), &[]);

        assert!(
            progress
                .note_tool_error("core/fetch_memory", "bad")
                .is_none()
        );
        assert!(
            progress
                .note_tool_error("core/fetch_memory", "bad")
                .is_none()
        );
        let streak = progress
            .note_tool_error("core/fetch_memory", "bad")
            .expect("third identical error fails");

        assert_eq!(streak.count, 3);
        assert_eq!(streak.tool_name, "core/fetch_memory");
    }

    #[test]
    fn reminder_and_stall_apply_only_when_durable_required() {
        let progress = FulfillmentProgress::new(&productions(), &[]);
        assert!(progress.should_remind(4));
        assert!(progress.is_stalled_after(16));

        let empty =
            FulfillmentProgress::new(&HashMap::from([("core/fetch_memory".into(), vec![])]), &[]);
        assert!(!empty.should_remind(4));
        assert!(!empty.is_stalled_after(16));
    }

    #[test]
    fn explicit_required_schema_ignores_intermediate_producer() {
        let mut productions = productions();
        productions.insert(
            "test/intermediate".into(),
            vec!["test/intermediate-v1".into()],
        );
        let progress = FulfillmentProgress::new(&productions, &["test/derivation-v1".to_string()]);

        assert!(
            progress
                .successful_tool_fulfills("test/intermediate")
                .is_none()
        );
        assert!(
            progress
                .successful_tool_fulfills("core/emit_abstraction::test/derivation-v1::v1")
                .is_some()
        );
    }
}
