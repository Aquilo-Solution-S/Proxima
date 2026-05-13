use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AuthDotJsonPath(pub PathBuf);

impl AuthDotJsonPath {
    pub fn from_home(home: &Path) -> Self {
        Self(home.join(".codex/auth.json"))
    }
}
