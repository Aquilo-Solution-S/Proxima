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
  type RepoIngestEventTs,
  type RepoEraseReceiptTs,
  type RepoRecordTs,
} from "../../bindings";
import { formatCommandError } from "../../format-error";
import { LoadingSurface } from "../../primitives";

type IngestState =
  | { kind: "idle" }
  | { kind: "running"; repoId: string; latest: IngestProgressTs | null }
  | { kind: "done"; repoId: string; report: IndexReportTs }
  | { kind: "error"; repoId: string; message: string };

type EraseState =
  | { kind: "idle" }
  | { kind: "running"; repoId: string; displayName: string }
  | { kind: "done"; receipt: RepoEraseReceiptTs }
  | { kind: "error"; repoId: string; message: string };

async function loadRepos(): Promise<RepoRecordTs[]> {
  const r = await commands.reposList();
  if (r.status === "error") throw r.error;
  return r.data;
}

export const ReposPanel: Component = () => {
  const [repos, { mutate, refetch }] = createResource(loadRepos);
  const [globalError, setGlobalError] = createSignal<string | null>(null);
  const [ingest, setIngest] = createSignal<IngestState>({ kind: "idle" });
  const [erase, setErase] = createSignal<EraseState>({ kind: "idle" });

  const isRunning = (repoId: string): boolean => {
    const s = ingest();
    return s.kind === "running" && s.repoId === repoId;
  };
  const isErasing = (repoId: string): boolean => {
    const s = erase();
    return s.kind === "running" && s.repoId === repoId;
  };
  const eraseDoneReceipt = (): RepoEraseReceiptTs | null => {
    const s = erase();
    return s.kind === "done" ? s.receipt : null;
  };
  const anyBusy = (): boolean =>
    ingest().kind === "running" || erase().kind === "running";

  const handleAdd = async (): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
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
    setErase({ kind: "idle" });
    setErase({
      kind: "running",
      repoId: repo.repo_id,
      displayName: repo.display_name,
    });
    const r = await commands.reposErase(repo.repo_id);
    if (r.status === "error") {
      const message = formatCommandError(r.error);
      setErase({ kind: "error", repoId: repo.repo_id, message });
      setGlobalError(message);
      return;
    }
    mutate((current) => current?.filter((r) => r.repo_id !== repo.repo_id));
    const ingestState = ingest();
    if ("repoId" in ingestState && ingestState.repoId === repo.repo_id) {
      setIngest({ kind: "idle" });
    }
    setErase({ kind: "done", receipt: r.data });
    void refetch();
  };

  const handleIngest = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    setIngest({ kind: "running", repoId: repo.repo_id, latest: null });

    const onEvent = new Channel<RepoIngestEventTs>();
    onEvent.onmessage = (event) => {
      if (event.kind === "progress") {
        setIngest({
          kind: "running",
          repoId: repo.repo_id,
          latest: event.data,
        });
        return;
      }
      if (event.kind === "done") {
        setIngest({ kind: "done", repoId: repo.repo_id, report: event.data });
        refetch();
        return;
      }
      setIngest({
        kind: "error",
        repoId: repo.repo_id,
        message: event.data.message,
      });
    };

    const r = await commands.repoIngest(repo.repo_id, onEvent);
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
          disabled={anyBusy()}
          onClick={handleAdd}
        >
          Add Repo
        </button>
      </header>

      <Show when={globalError()}>
        {(msg) => <p class="proxima-error">{msg()}</p>}
      </Show>
      <Show when={erase().kind === "running"}>
        <LoadingSurface mode="inline" label="Deleting" size={36} />
      </Show>
      <Show when={eraseDoneReceipt()}>
        {(r) => (
          <p class="proxima-dim proxima-mono">
            deleted {r().facts_deleted.toString()} facts,{" "}
            {r().abstractions_deleted.toString()} abstractions,{" "}
            {r().edges_deleted.toString()} edges,{" "}
            {r().embeddings_deleted.toString()} embeddings
          </p>
        )}
      </Show>

      <Show
        when={!repos.loading}
        fallback={<LoadingSurface label="Loading repos" />}
      >
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
                  isErasing={isErasing(repo.repo_id)}
                  anyBusy={anyBusy()}
                  ingest={ingest()}
                  erase={erase()}
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
  isErasing: boolean;
  anyBusy: boolean;
  ingest: IngestState;
  erase: EraseState;
  onIngest: () => void;
  onDelete: () => void;
}> = (props) => {
  const [confirmingDelete, setConfirmingDelete] = createSignal(false);
  const [confirmText, setConfirmText] = createSignal("");

  const status = (): string => {
    if (props.isErasing) return "deleting repo data...";
    if (confirmingDelete()) return "type repo name to confirm deletion";
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
    const e = props.erase;
    if (e.kind === "error" && e.repoId === props.repo.repo_id) {
      return `error: ${e.message}`;
    }
    if (props.repo.has_been_polled && props.repo.last_polled_at) {
      return `last polled ${new Date(props.repo.last_polled_at).toLocaleString()}`;
    }
    return "never polled";
  };
  const canConfirmDelete = (): boolean =>
    confirmText() === props.repo.display_name && !props.anyBusy;
  const openDeleteConfirm = (): void => {
    setConfirmText("");
    setConfirmingDelete(true);
  };
  const cancelDeleteConfirm = (): void => {
    setConfirmText("");
    setConfirmingDelete(false);
  };
  const confirmDelete = (): void => {
    if (!canConfirmDelete()) return;
    props.onDelete();
  };

  return (
    <article class="proxima-repo-row">
      <div class="proxima-repo-head">
        <span class="proxima-repo-name">{props.repo.display_name}</span>
        <span class="proxima-dim proxima-mono">{props.repo.canonical_path}</span>
      </div>
      <div class="proxima-repo-status">{status()}</div>
      <div class="proxima-repo-actions">
        <Show
          when={confirmingDelete()}
          fallback={
            <>
              <button
                type="button"
                class="proxima-btn"
                disabled={props.anyBusy}
                onClick={props.onIngest}
              >
                {props.isRunning ? "Ingesting…" : "Ingest"}
              </button>
              <button
                type="button"
                class="proxima-btn proxima-btn-danger"
                disabled={props.anyBusy}
                onClick={openDeleteConfirm}
              >
                {props.isErasing ? "Deleting..." : "Delete"}
              </button>
            </>
          }
        >
          <input
            type="text"
            class="proxima-repo-delete-confirm"
            value={confirmText()}
            placeholder={props.repo.display_name}
            disabled={props.anyBusy}
            onInput={(e) => setConfirmText(e.currentTarget.value)}
          />
          <button
            type="button"
            class="proxima-btn proxima-btn-danger"
            disabled={!canConfirmDelete()}
            onClick={confirmDelete}
          >
            Confirm
          </button>
          <button
            type="button"
            class="proxima-btn"
            disabled={props.anyBusy}
            onClick={cancelDeleteConfirm}
          >
            Cancel
          </button>
        </Show>
      </div>
    </article>
  );
};
