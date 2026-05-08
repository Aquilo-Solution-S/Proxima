use proxima_core::PersonalityFlavor;

#[derive(Debug, Default, Clone)]
pub struct CommitSummaryPersonality;

#[derive(Debug, Default, Clone)]
pub struct CodeEngineerPersonality;

impl PersonalityFlavor for CommitSummaryPersonality {
    fn personality_type_id(&self) -> &'static str {
        "proxima-code/commit-summary-v1"
    }

    fn default_display_name(&self) -> &'static str {
        "Commit Summarizer"
    }

    fn default_purpose(&self) -> &'static str {
        "Summarize commits as Abstractions"
    }
}

impl PersonalityFlavor for CodeEngineerPersonality {
    fn personality_type_id(&self) -> &'static str {
        "proxima-code/engineer-v1"
    }

    fn default_display_name(&self) -> &'static str {
        "Engineer"
    }

    fn default_purpose(&self) -> &'static str {
        "Develop perspectives on code changes"
    }
}
