use async_trait::async_trait;
use proxima_core::{
    PersonalityContext, PersonalityFlavor, PersonalityId, PersonalitySnapshot, error::ProtocolError,
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
            captured_at: OffsetDateTime::now_utc(),
        })
    }
}
