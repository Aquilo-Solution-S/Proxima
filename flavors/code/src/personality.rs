use async_trait::async_trait;
use proxima_core::{
    PersonalityContext, PersonalityFlavor, PersonalityId, PersonalitySnapshot,
    PersonalityStateHash, error::ProtocolError,
};
use time::OffsetDateTime;

#[derive(Debug, Default, Clone)]
pub struct CodeEngineerPersonality;

#[async_trait]
impl PersonalityFlavor for CodeEngineerPersonality {
    fn personality_id(&self) -> &'static str {
        "proxima-code/engineer"
    }

    async fn snapshot(
        &self,
        _ctx: &PersonalityContext<'_>,
    ) -> Result<PersonalitySnapshot, ProtocolError> {
        Ok(PersonalitySnapshot {
            personality_id: PersonalityId::new(self.personality_id()),
            state_hash: PersonalityStateHash::new(
                blake3::hash(b"proxima-code/engineer/v1-empty").into(),
            ),
            captured_at: OffsetDateTime::now_utc(),
        })
    }
}
