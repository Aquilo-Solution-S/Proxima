use proxima_core::{
    DependencySatisfactionRule, FactPayload, MemoryId, Owner, Storage, StorageError,
};

use crate::payloads::TestRequestV1;

#[derive(Debug, Default)]
pub struct TestRequestSatisfied;

#[async_trait::async_trait]
impl DependencySatisfactionRule for TestRequestSatisfied {
    fn target_schema_id(&self) -> &'static str {
        TestRequestV1::SCHEMA_ID
    }

    async fn is_satisfied(
        &self,
        storage: &dyn Storage,
        owner: &Owner,
        dependency_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        storage
            .has_satisfied_code_test_request(owner, dependency_memory_id)
            .await
    }
}
