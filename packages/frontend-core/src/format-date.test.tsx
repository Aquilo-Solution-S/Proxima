import { describe, expect, it } from "vitest";
import { formatPolledAt } from "./format-date";

describe("formatPolledAt", () => {
  it("renders RFC3339 timestamps", () => {
    const out = formatPolledAt("2026-05-05T22:03:34Z");
    expect(out).toMatch(/^last polled /);
    expect(out).not.toMatch(/Invalid Date/);
  });

  it("falls back on null", () => {
    expect(formatPolledAt(null)).toBe("never polled");
  });

  it("falls back on garbage", () => {
    expect(formatPolledAt("2026-05-05 22:03:34.0 +00:00:00")).toBe(
      "never polled",
    );
  });

  it("falls back on empty string", () => {
    expect(formatPolledAt("")).toBe("never polled");
  });
});
