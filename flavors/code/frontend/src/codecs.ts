import { decode, encode } from "cbor-x";
import type { CodeChunkV1, FileRevisionV1 } from "@proxima/core";
import type { PayloadCodec } from "@proxima/core/hub";

export const codePayloadCodec: PayloadCodec<unknown> = {
  decode(bytes: Uint8Array): unknown {
    return decode(bytes);
  },
  encode(value: unknown): Uint8Array {
    return encode(value);
  },
};

export const fileRevisionCodec: PayloadCodec<FileRevisionV1> = {
  decode(bytes: Uint8Array): FileRevisionV1 {
    return decode(bytes) as FileRevisionV1;
  },
  encode(value: FileRevisionV1): Uint8Array {
    return encode(value);
  },
  naturalKey(value: FileRevisionV1) {
    return [value.repo_id, value.file_path];
  },
};

export const codeChunkCodec: PayloadCodec<CodeChunkV1> = {
  decode(bytes: Uint8Array): CodeChunkV1 {
    return decode(bytes) as CodeChunkV1;
  },
  encode(value: CodeChunkV1): Uint8Array {
    return encode(value);
  },
  naturalKey(value: CodeChunkV1) {
    return [value.repo_id, value.file_path, value.chunk_index];
  },
};
