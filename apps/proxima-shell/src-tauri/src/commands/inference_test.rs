use proxima_core::error::ProtocolError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceEnvStatusTs {
    pub env_var: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceEnvStatusOutcomeTs {
    pub present: bool,
}

fn env_status_with<F>(env_var: &str, lookup: F) -> InferenceEnvStatusOutcomeTs
where
    F: Fn(&str) -> bool,
{
    InferenceEnvStatusOutcomeTs {
        present: lookup(env_var),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn inference_env_status(
    req: InferenceEnvStatusTs,
) -> Result<InferenceEnvStatusOutcomeTs, ProtocolError> {
    Ok(env_status_with(&req.env_var, |key| {
        std::env::var(key).is_ok()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_status_present_when_lookup_returns_true() {
        let out = env_status_with("ANY_KEY", |_| true);
        assert!(out.present);
    }

    #[test]
    fn env_status_absent_when_lookup_returns_false() {
        let out = env_status_with("ANY_KEY", |_| false);
        assert!(!out.present);
    }
}
