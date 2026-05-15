import { describe, expect, it, vi } from "vitest";
import type { Owner } from "./bindings";
import { uploadCitedBlob, type CitedBlobUploadCommands } from "./cited-blob-upload";

const owner: Owner = {
  principal: { User: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa10" },
  org_id: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa11",
};

function commands(): CitedBlobUploadCommands {
  return {
    citedBlobUploadPrepare: vi.fn(async () => ({
      status: "ok" as const,
      data: {
        upload_id: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa12",
        upload_url: "https://s3.example/upload",
        expires_at: "2026-05-15T12:00:00Z",
        headers: [{ name: "content-type", value: "text/plain" }],
      },
    })),
    citedBlobUploadComplete: vi.fn(async () => ({
      status: "ok" as const,
      data: {
        cited_object_id: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa13",
        schema: "proxima-core/uploaded-blob-v1",
        content_hash: "a".repeat(64),
        sha256: "b".repeat(64),
        byte_len: 5,
        mime: "text/plain",
        filename: "note.txt",
        idempotent_replay: false,
      },
    })),
    citedBlobUploadAbort: vi.fn(async () => ({
      status: "ok" as const,
      data: { aborted: true },
    })),
  };
}

describe("uploadCitedBlob", () => {
  it("prepares, uploads with returned headers, then completes", async () => {
    const api = commands();
    const fetchImpl = vi.fn(async () => new Response(null, { status: 200 }));

    const result = await uploadCitedBlob({
      owner,
      blob: new Blob(["hello"], { type: "text/plain" }),
      filename: "note.txt",
      commands: api,
      fetchImpl,
    });

    expect(api.citedBlobUploadPrepare).toHaveBeenCalledWith({
      owner,
      filename: "note.txt",
      mime: "text/plain",
      byte_len: 5,
    });
    expect(fetchImpl).toHaveBeenCalledWith(
      "https://s3.example/upload",
      expect.objectContaining({
        method: "PUT",
        body: expect.any(Blob),
      }),
    );
    expect(api.citedBlobUploadComplete).toHaveBeenCalledWith({
      owner,
      upload_id: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa12",
    });
    expect(api.citedBlobUploadAbort).not.toHaveBeenCalled();
    expect(result.cited_object_id).toBe("019e2de0-c4e7-7e9e-8f22-94fd25fbfa13");
  });

  it("aborts the pending upload when the direct PUT fails", async () => {
    const api = commands();
    const fetchImpl = vi.fn(async () => new Response(null, { status: 503 }));

    await expect(
      uploadCitedBlob({
        owner,
        blob: new Blob(["hello"], { type: "text/plain" }),
        filename: "note.txt",
        commands: api,
        fetchImpl,
      }),
    ).rejects.toThrow("HTTP 503");

    expect(api.citedBlobUploadAbort).toHaveBeenCalledWith({
      owner,
      upload_id: "019e2de0-c4e7-7e9e-8f22-94fd25fbfa12",
    });
    expect(api.citedBlobUploadComplete).not.toHaveBeenCalled();
  });
});
