import { describe, expect, it } from "vitest";
import type { CodeChunkV1, FileRevisionV1 } from "@proxima/core";
import { codeChunkCodec, fileRevisionCodec } from "./codecs";

describe("code payload codecs", () => {
  it("fileRevisionCodec.naturalKey returns [repo_id, file_path]", () => {
    const value: FileRevisionV1 = {
      repo_id: "019dfa00-0000-7000-8000-000000000100",
      file_path: "src/x.rs",
      language: "rust",
      content_sha256: "00".repeat(32),
      size_bytes: 1,
      indexed_commit_sha: "0".repeat(40),
      state: "Present",
    };
    expect(fileRevisionCodec.naturalKey?.(value)).toEqual([
      value.repo_id,
      value.file_path,
    ]);
  });

  it("codeChunkCodec.naturalKey returns [repo_id, file_path, chunk_index]", () => {
    const value: CodeChunkV1 = {
      repo_id: "019dfa00-0000-7000-8000-000000000100",
      file_path: "src/x.rs",
      chunk_index: 4,
      text: "fn x() {}",
      language: "rust",
      chunk_type: "function",
      byte_range_start: 0,
      byte_range_end: 9,
      line_range_start: 1,
      line_range_end: 1,
      state: "Present",
    };
    expect(codeChunkCodec.naturalKey?.(value)).toEqual([
      value.repo_id,
      value.file_path,
      value.chunk_index,
    ]);
  });
});
