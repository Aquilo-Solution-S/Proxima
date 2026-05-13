use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::CodexAuthError;

#[derive(Debug, Clone)]
pub struct AuthDotJsonPath(pub PathBuf);

impl AuthDotJsonPath {
    pub fn from_home(home: &Path) -> Self {
        Self(home.join(".codex/auth.json"))
    }

    pub fn from_explicit(path: PathBuf) -> Self {
        Self(path)
    }

    /// Read and parse `~/.codex/auth.json`.
    ///
    /// * Missing file → [`CodexAuthError::AuthJsonMissing`].
    /// * Any other IO error → [`CodexAuthError::AuthJsonInvalid`].
    /// * JSON parse error → [`CodexAuthError::AuthJsonInvalid`].
    pub fn read(&self) -> Result<serde_json::Value, CodexAuthError> {
        let bytes = std::fs::read(&self.0).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                CodexAuthError::AuthJsonMissing {
                    path: self.0.display().to_string(),
                }
            } else {
                CodexAuthError::AuthJsonInvalid(format!("read: {e}"))
            }
        })?;

        serde_json::from_slice(&bytes)
            .map_err(|e| CodexAuthError::AuthJsonInvalid(format!("parse: {e}")))
    }

    /// Atomically write `value` to `~/.codex/auth.json`.
    ///
    /// Writes to a sibling `.proxima.tmp` file first, syncs, sets `0o600`
    /// permissions on Unix, then renames into place.
    pub fn write_atomic(&self, value: &serde_json::Value) -> io::Result<()> {
        let dir = self.0.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "auth.json path has no parent directory",
            )
        })?;

        let target_filename = self
            .0
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "auth.json path has no filename"))?
            .to_string_lossy();

        let tmp_path = dir.join(format!("{}.proxima.tmp", target_filename));

        let result = (|| -> io::Result<()> {
            let bytes = serde_json::to_vec_pretty(value)
                .map_err(|e| io::Error::other(format!("serialize: {e}")))?;

            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)?;

            file.write_all(&bytes)?;
            file.sync_all()?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }

            std::fs::rename(&tmp_path, &self.0)?;
            Ok(())
        })();

        if result.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_returns_missing_for_nonexistent_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = AuthDotJsonPath(dir.path().join("no-such-file"));
        let err = path.read().unwrap_err();
        assert!(
            matches!(err, CodexAuthError::AuthJsonMissing { .. }),
            "expected AuthJsonMissing, got: {err:?}"
        );
    }

    #[test]
    fn read_returns_invalid_for_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("auth.json");
        std::fs::write(&file_path, b"{ not json").unwrap();

        let path = AuthDotJsonPath(file_path);
        let err = path.read().unwrap_err();
        assert!(
            matches!(err, CodexAuthError::AuthJsonInvalid(_)),
            "expected AuthJsonInvalid, got: {err:?}"
        );
    }

    #[test]
    fn read_succeeds_on_well_formed_json() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("auth.json");
        std::fs::write(&file_path, br#"{"foo":"bar","n":1}"#).unwrap();

        let path = AuthDotJsonPath(file_path);
        let value = path.read().unwrap();
        assert_eq!(value["foo"], "bar");
        assert_eq!(value["n"], 1);
    }

    #[test]
    fn write_atomic_round_trip_preserves_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("auth.json");
        let path = AuthDotJsonPath(file_path);

        let original = json!({
            "access_token": "tok-abc",
            "refresh_token": "ref-xyz",
            "expires_at": 9999999999i64,
            "nested": { "chatgpt_account_id": "user-123", "flag": true }
        });

        path.write_atomic(&original).unwrap();

        let read_back = path.read().unwrap();
        assert_eq!(read_back["access_token"], "tok-abc");
        assert_eq!(read_back["refresh_token"], "ref-xyz");
        assert_eq!(read_back["expires_at"], 9999999999i64);
        assert_eq!(read_back["nested"]["chatgpt_account_id"], "user-123");
        assert_eq!(read_back["nested"]["flag"], true);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_0600_perms() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("auth.json");
        let path = AuthDotJsonPath(file_path.clone());

        path.write_atomic(&json!({"key": "value"})).unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600, got 0o{mode:o}");
    }

    #[test]
    fn write_atomic_overwrites_existing_file_and_preserves_perms() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("auth.json");

        // Pre-create with old content
        std::fs::write(&file_path, br#"{"old_key":"old_value"}"#).unwrap();

        let path = AuthDotJsonPath(file_path.clone());
        let new_value = json!({"new_key": "new_value"});
        path.write_atomic(&new_value).unwrap();

        let read_back = path.read().unwrap();
        assert_eq!(read_back["new_key"], "new_value");
        assert!(
            read_back.get("old_key").is_none(),
            "old content should be gone"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let meta = std::fs::metadata(&file_path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected 0o600 after overwrite, got 0o{mode:o}");
        }
    }
}
