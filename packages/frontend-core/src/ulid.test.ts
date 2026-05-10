import { describe, expect, it } from "vitest";
import {
  orderedIdTimestampMs,
  ulidTimestampMs,
  uuidV7TimestampMs,
} from "./ulid";

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
    // 'U' at position 4 is not in Crockford-base32 (which omits I, L, O, U)
    expect(() => ulidTimestampMs("01ARUZ6S41TS5G7QFC0V44N5KH")).toThrow(/not Crockford/);
  });

  it("decodes a max-time ULID", () => {
    // ULID encodes 48-bit ms timestamps; max valid prefix is 7ZZZZZZZZZ = 2^48 - 1 = 281474976710655
    expect(ulidTimestampMs("7ZZZZZZZZZ0000000000000000")).toBe(281474976710655);
  });
});

describe("uuidV7TimestampMs", () => {
  it("parses the leading 48-bit UUIDv7 timestamp", () => {
    expect(uuidV7TimestampMs("019e12c2-2ba6-73c3-83a3-6ba8f0db1d00")).toBe(
      1778431175590,
    );
  });

  it("rejects non-v7 UUIDs", () => {
    expect(() =>
      uuidV7TimestampMs("019e12c2-2ba6-63c3-83a3-6ba8f0db1d00"),
    ).toThrow(/UUIDv7 expected/);
  });
});

describe("orderedIdTimestampMs", () => {
  it("accepts legacy ULID and UUIDv7 ordered ids", () => {
    expect(orderedIdTimestampMs("01ARYZ6S41TS5G7QFC0V44N5KH")).toBe(
      1469918176385,
    );
    expect(orderedIdTimestampMs("019e12c2-2ba6-73c3-83a3-6ba8f0db1d00")).toBe(
      1778431175590,
    );
  });
});
