import {
  commands as tauriCommands,
  type CitedBlobUploadAbortOutcomeTs,
  type CitedBlobUploadAbortTs,
  type CitedBlobUploadCompleteOutcomeTs,
  type CitedBlobUploadCompleteTs,
  type CitedBlobUploadPrepareOutcomeTs,
  type CitedBlobUploadPrepareTs,
  type CommandError,
  type Owner,
} from "./bindings";

type CommandResult<T> =
  | { status: "ok"; data: T }
  | { status: "error"; error: CommandError };

export type CitedBlobUploadCommands = {
  citedBlobUploadPrepare: (
    req: CitedBlobUploadPrepareTs,
  ) => Promise<CommandResult<CitedBlobUploadPrepareOutcomeTs>>;
  citedBlobUploadComplete: (
    req: CitedBlobUploadCompleteTs,
  ) => Promise<CommandResult<CitedBlobUploadCompleteOutcomeTs>>;
  citedBlobUploadAbort: (
    req: CitedBlobUploadAbortTs,
  ) => Promise<CommandResult<CitedBlobUploadAbortOutcomeTs>>;
};

export type CitedBlobUploadInput = {
  owner: Owner;
  blob: Blob;
  filename: string;
  mime?: string;
  commands?: CitedBlobUploadCommands;
  fetchImpl?: typeof fetch;
};

export async function uploadCitedBlob({
  owner,
  blob,
  filename,
  mime,
  commands = tauriCommands,
  fetchImpl = fetch,
}: CitedBlobUploadInput): Promise<CitedBlobUploadCompleteOutcomeTs> {
  const principal = owner.principal;
  const prepared = await unwrap(
    commands.citedBlobUploadPrepare({
      principal,
      filename,
      mime: mime || blob.type || "application/octet-stream",
      byte_len: blob.size,
    }),
    "cited_blob_upload_prepare",
  );

  try {
    const headers = new Headers();
    for (const header of prepared.headers) {
      headers.set(header.name, header.value);
    }
    const response = await fetchImpl(prepared.upload_url, {
      method: "PUT",
      headers,
      body: blob,
    });
    if (!response.ok) {
      throw new Error(`S3 upload failed: HTTP ${response.status}`);
    }
    return await unwrap(
      commands.citedBlobUploadComplete({
        principal,
        upload_id: prepared.upload_id,
      }),
      "cited_blob_upload_complete",
    );
  } catch (error) {
    await commands
      .citedBlobUploadAbort({ principal, upload_id: prepared.upload_id })
      .catch(() => undefined);
    throw error;
  }
}

async function unwrap<T>(result: Promise<CommandResult<T>>, command: string): Promise<T> {
  const resolved = await result;
  if (resolved.status === "ok") {
    return resolved.data;
  }
  throw new Error(`${command}: ${formatCommandError(resolved.error)}`);
}

function formatCommandError(error: CommandError): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "data" in error &&
    typeof error.data === "object" &&
    error.data !== null &&
    "message" in error.data
  ) {
    return String(error.data.message);
  }
  return JSON.stringify(error);
}
