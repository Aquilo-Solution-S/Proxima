import { describe, expect, it } from "vitest";
import { ulidTimestampMs } from "./ulid";

describe("ulidTimestampMs", () => {
  it("decodes the spec example", () => {
    // 01ARYZ6S41 → 1469918176385 (ULID spec vector)
    expect(ulidTimestampMs("01ARYZ6S41TS5G7QFC0V44N5KH")).toBe(1469918176385);
  });

  it("decodes a min-time ULID", () => {
    expect(ulidTimestampMs("00000000000000000000000000")).toBe(0);
  });

  it("rejects too-short input", () => {
    expect(() => ulidTimestampMs("01ARYZ6S")).toThrow(/26 characters/);
  });

  it("rejects invalid Crockford characters", () => {
    expect(() => ulidTimestampMs("01ARYZ6S41ULOI0000000000000")).toThrow();
  });
});
