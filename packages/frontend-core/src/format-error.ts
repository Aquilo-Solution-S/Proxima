import type { CommandError } from "./bindings";

export function formatCommandError(err: CommandError): string {
  switch (err.kind) {
    case "storage":
      return `Storage error: ${err.data.message}`;
    case "duplicate_embedding_model":
      return `Embedding model already registered: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "unknown_embedding_model":
      return `Unknown embedding model: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "config_io":
      return `Config read failed at ${err.data.path}: ${err.data.message}`;
    case "config_save_io":
      return `Config save failed at ${err.data.path}: ${err.data.message}`;
    case "config_parse":
      return `Config parse failed: ${err.data.message}`;
    case "config_serialize":
      return `Config serialize failed: ${err.data.message}`;
    case "invariant":
      return `Internal invariant violation: ${err.data.message}`;
    case "invalid_repo_path":
      return `Invalid repo path "${err.data.path}": ${err.data.reason}`;
    case "not_a_git_repo":
      return `Not a git repository: ${err.data.path}`;
    case "duplicate_repo":
      return `Repo already registered at: ${err.data.canonical_path}`;
    case "unknown_repo":
      return `Unknown repo: ${err.data.repo_id}`;
    case "invalid_repo_target_branch":
      return `Invalid target branch "${err.data.target_branch}" for repo ${err.data.repo_id}: ${err.data.reason}`;
    case "invalid_uuid":
      return `Invalid UUID: ${err.data.value}`;
    case "secret_store":
      return `Secret store error: ${err.data.message}`;
  }
}
