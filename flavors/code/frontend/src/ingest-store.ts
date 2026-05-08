import { Channel } from "@tauri-apps/api/core";
import { batch, createRoot } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import {
  commands,
  type IndexReportTs,
  type IngestProgressTs,
  type RepoIngestEventTs,
  type RepoIngestionRunTs,
} from "@proxima/core";

export type RunRecord = {
  run: RepoIngestionRunTs | null;
  liveProgress: IngestProgressTs | null;
  terminalReport: IndexReportTs | null;
  terminalError: string | null;
  subscribed: boolean;
};

const initial = (): RunRecord => ({
  run: null,
  liveProgress: null,
  terminalReport: null,
  terminalError: null,
  subscribed: false,
});

const root = createRoot(() => {
  const [state, setState] = createStore<Record<string, RunRecord>>({});

  function ensure(repoId: string): void {
    if (!state[repoId]) setState(repoId, initial());
  }

  async function start(
    repoId: string,
    maxCommits: number | null = null,
  ): Promise<void> {
    ensure(repoId);
    const r = await commands.repoIngestStart(repoId, maxCommits);
    if (r.status === "error") {
      setState(repoId, "terminalError", String(r.error));
      return;
    }
    setState(
      repoId,
      produce((rec) => {
        rec.run = r.data;
        rec.liveProgress = null;
        rec.terminalReport = null;
        rec.terminalError = null;
      }),
    );
    void subscribe(repoId);
  }

  async function subscribe(repoId: string): Promise<void> {
    ensure(repoId);
    if (state[repoId]?.subscribed) return;
    setState(repoId, "subscribed", true);

    const ch = new Channel<RepoIngestEventTs>();
    ch.onmessage = (event) =>
      batch(() => {
        if (event.kind === "snapshot") {
          setState(repoId, "run", event.data);
        } else if (event.kind === "progress") {
          setState(repoId, "liveProgress", event.data);
        } else if (event.kind === "done") {
          setState(
            repoId,
            produce((rec) => {
              rec.terminalReport = event.data;
              rec.subscribed = false;
            }),
          );
        } else {
          setState(
            repoId,
            produce((rec) => {
              rec.terminalError = event.data.message;
              rec.subscribed = false;
            }),
          );
        }
      });

    const r = await commands.repoIngestSubscribe(repoId, ch);
    if (r.status === "error") {
      setState(
        repoId,
        produce((rec) => {
          rec.terminalError = String(r.error);
          rec.subscribed = false;
        }),
      );
    }
  }

  async function rehydrate(repoId: string): Promise<void> {
    ensure(repoId);
    if (state[repoId]?.subscribed) return;
    const r = await commands.repoIngestStatus(repoId);
    if (r.status === "error") return;
    if (r.data) {
      setState(repoId, "run", r.data);
      void subscribe(repoId);
    }
  }

  function isRunning(repoId: string): boolean {
    const rec = state[repoId];
    if (!rec?.run) return false;
    return rec.run.status === "queued" || rec.run.status === "running";
  }

  function resetForTests(): void {
    setState(reconcile({}));
  }

  return { state, start, subscribe, rehydrate, isRunning, resetForTests };
});

export const ingestStore = root;
