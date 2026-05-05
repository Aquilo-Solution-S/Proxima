import {
  For,
  Show,
  createResource,
  createSignal,
  type Component,
} from "solid-js";
import { Channel } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  commands,
  type IndexReportTs,
  type IngestProgressTs,
  type RepoRecordTs,
} from "../../bindings";
import { formatCommandError } from "../../format-error";

type IngestState =
  | { kind: "idle" }
  | { kind: "running"; repoId: string; latest: IngestProgressTs | null }
  | { kind: "done"; repoId: string; report: IndexReportTs }
  | { kind: "error"; repoId: string; message: string };

async function loadRepos(): Promise<RepoRecordTs[]> {
  const r = await commands.reposList();
  if (r.status === "error") throw r.error;
  return r.data;
}

export const ReposPanel: Component = () => {
  const [repos, { refetch }] = createResource(loadRepos);
  const [globalError, setGlobalError] = createSignal<string | null>(null);
  const [ingest, setIngest] = createSignal<IngestState>({ kind: "idle" });

  const isRunning = (repoId: string): boolean => {
    const s = ingest();
    return s.kind === "running" && s.repoId === repoId;
  };
  const anyRunning = (): boolean => ingest().kind === "running";

  const handleAdd = async (): Promise<void> => {
    setGlobalError(null);
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "Select a git repository",
    });
    if (selected === null) return;
    const path = Array.isArray(selected) ? selected[0]! : selected;
    const r = await commands.reposRegister(path, null);
    if (r.status === "error") {
      setGlobalError(formatCommandError(r.error));
      return;
    }
    refetch();
  };

  const handleDelete = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    const ok = window.confirm(
      `Remove "${repo.display_name}" from the registry? Ingested data is kept.`,
    );
    if (!ok) return;
    const r = await commands.reposDelete(repo.repo_id);
    if (r.status === "error") {
      setGlobalError(formatCommandError(r.error));
      return;
    }
    refetch();
  };

  const handleIngest = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    setIngest({ kind: "running", repoId: repo.repo_id, latest: null });

    const onProgress = new Channel<IngestProgressTs>();
    onProgress.onmessage = (p) => {
      setIngest({ kind: "running", repoId: repo.repo_id, latest: p });
    };

    const onDone = new Channel<IndexReportTs>();
    onDone.onmessage = (report) => {
      setIngest({ kind: "done", repoId: repo.repo_id, report });
      refetch();
    };

    const r = await commands.repoIngest(repo.repo_id, onProgress, onDone);
    if (r.status === "error") {
      setIngest({
        kind: "error",
        repoId: repo.repo_id,
        message: formatCommandError(r.error),
      });
    }
  };

  return (
    <div class="proxima-code-panel">
      <header class="proxima-repos-header">
        <h2>Repos</h2>
        <button
          type="button"
          class="proxima-btn"
          disabled={anyRunning()}
          onClick={handleAdd}
        >
          Add Repo
        </button>
      </header>

      <Show when={globalError()}>
        {(msg) => <p class="proxima-error">{msg()}</p>}
      </Show>

      <Show when={!repos.loading} fallback={<p class="proxima-dim">Loading…</p>}>
        <Show
          when={(repos() ?? []).length > 0}
          fallback={
            <p class="proxima-dim">
              No repos registered yet. Click "Add Repo" to point Proxima at a
              local git working tree.
            </p>
          }
        >
          <div class="proxima-repos-list">
            <For each={repos()}>
              {(repo) => (
                <RepoRow
                  repo={repo}
                  isRunning={isRunning(repo.repo_id)}
                  anyRunning={anyRunning()}
                  ingest={ingest()}
                  onIngest={() => handleIngest(repo)}
                  onDelete={() => handleDelete(repo)}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
};

const RepoRow: Component<{
  repo: RepoRecordTs;
  isRunning: boolean;
  anyRunning: boolean;
  ingest: IngestState;
  onIngest: () => void;
  onDelete: () => void;
}> = (props) => {
  const status = (): string => {
    if (props.isRunning) {
      const s = props.ingest;
      if (s.kind !== "running") return "starting…";
      if (s.latest === null) return "starting…";
      const { commit_index, total_commits, commit_sha } = s.latest;
      return `commit ${commit_index + 1}/${total_commits} · ${commit_sha.slice(0, 7)}`;
    }
    const s = props.ingest;
    if (s.kind === "done" && s.repoId === props.repo.repo_id) {
      const r = s.report;
      return `done — +${r.commits_emitted} commits, +${r.chunks_emitted} chunks (${r.chunks_reused} reused, ${r.chunks_tombstoned} tombstoned)`;
    }
    if (s.kind === "error" && s.repoId === props.repo.repo_id) {
      return `error: ${s.message}`;
    }
    if (props.repo.has_been_polled && props.repo.last_polled_at) {
      return `last polled ${new Date(props.repo.last_polled_at).toLocaleString()}`;
    }
    return "never polled";
  };

  return (
    <article class="proxima-repo-row">
      <div class="proxima-repo-head">
        <span class="proxima-repo-name">{props.repo.display_name}</span>
        <span class="proxima-dim proxima-mono">{props.repo.canonical_path}</span>
      </div>
      <div class="proxima-repo-status">{status()}</div>
      <div class="proxima-repo-actions">
        <button
          type="button"
          class="proxima-btn"
          disabled={props.anyRunning}
          onClick={props.onIngest}
        >
          {props.isRunning ? "Ingesting…" : "Ingest"}
        </button>
        <button
          type="button"
          class="proxima-btn proxima-btn-danger"
          disabled={props.anyRunning}
          onClick={props.onDelete}
        >
          Remove
        </button>
      </div>
    </article>
  );
};
