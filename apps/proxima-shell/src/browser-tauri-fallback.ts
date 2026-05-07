type BrowserTauriInternals = {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: BrowserTauriInternals;
    __PROXIMA_BROWSER_TAURI_FALLBACK__?: true;
  }
}

type BrowserRepo = {
  repo_id: string;
  canonical_path: string;
  display_name: string;
  has_been_polled: boolean;
  last_polled_at: string | null;
  created_at: string;
};

const browserRepos: BrowserRepo[] = [];

const repoNameFromPath = (path: string): string => {
  const normalized = path.replace(/\/+$/, "");
  return normalized.split("/").pop() || normalized || "repository";
};

const makeBrowserRepo = (
  path: string,
  displayName: string | null | undefined,
): BrowserRepo => ({
  repo_id: crypto.randomUUID(),
  canonical_path: path,
  display_name: displayName ?? repoNameFromPath(path),
  has_been_polled: false,
  last_polled_at: null,
  created_at: new Date().toISOString(),
});

const invokeFallback = async (
  cmd: string,
  args?: Record<string, unknown>,
): Promise<unknown> => {
  switch (cmd) {
    case "plugin:dialog|open": {
      return window.prompt("Repository path");
    }
    case "schema":
      return { schemas: [] };
    case "query":
      return { memories: [], goals: [], edges: [], seq_high_water: null };
    case "subscribe":
    case "register_inference_target":
    case "models_register_embedding":
    case "bind_inference_tier":
    case "embedding_active_set":
    case "repo_ingest":
      return null;
    case "list_inference_targets":
    case "list_inference_tier_bindings":
    case "models_list_embedding":
      return [];
    case "repos_list":
      return browserRepos;
    case "repos_register": {
      const path = String(args?.path ?? "");
      const displayName =
        typeof args?.displayName === "string" ? args.displayName : null;
      if (path.length === 0) {
        throw {
          kind: "invalid_repo_path",
          data: { path, reason: "empty path" },
        };
      }
      const existing = browserRepos.find((r) => r.canonical_path === path);
      if (existing) {
        throw {
          kind: "duplicate_repo",
          data: { canonical_path: path },
        };
      }
      const repo = makeBrowserRepo(path, displayName);
      browserRepos.push(repo);
      return repo;
    }
    case "remove_inference_target":
    case "models_delete_embedding":
    case "embedding_active_clear":
    case "repos_delete":
      return false;
    case "repos_erase": {
      const repoId = String(args?.repoId ?? "");
      const index = browserRepos.findIndex((repo) => repo.repo_id === repoId);
      if (index >= 0) browserRepos.splice(index, 1);
      return {
        repo_id: repoId,
        completed_at: new Date().toISOString(),
        facts_deleted: 0,
        memories_deleted: 0,
        goals_deleted: 0,
        edges_deleted: 0,
        embeddings_deleted: 0,
        citations_deleted: 0,
        citation_mappings_deleted: 0,
        cited_objects_deleted: 0,
        source_batches_deleted: 0,
        f2a_rows_deleted: 0,
        repo_record_deleted: index >= 0,
      };
    }
    case "embedding_active_get":
      return null;
    default:
      throw {
        kind: "storage",
        data: {
          message: `${cmd} is unavailable outside the Tauri shell runtime`,
        },
      };
  }
};

if (
  import.meta.env.DEV &&
  typeof window !== "undefined" &&
  window.__TAURI_INTERNALS__ === undefined
) {
  window.__TAURI_INTERNALS__ = {
    invoke: invokeFallback,
  };
  window.__PROXIMA_BROWSER_TAURI_FALLBACK__ = true;
}

export {};
