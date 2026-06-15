// Covers the structured success-toast content builder. The content shape is the
// contract the worktreeToast() wrapper + AppToast.svelte render against, so it is
// locked to the mockup toast (pane 10): subject headline + sha · pushed ↑N · M
// files.
import { describe, it, expect } from "vitest";
import { autoToastContent, autoErrorMessage } from "./composerToast";
import type { CommitAndPushResult } from "./types";

const result: CommitAndPushResult = {
  subject: "feat: add inline composer to right rail",
  short_sha: "3f2c1a9",
  pushed_commits: 1,
  file_count: 4,
};

describe("autoToastContent", () => {
  it("puts the subject on the headline and sha/push/files as toned meta", () => {
    expect(autoToastContent(result)).toEqual({
      message: "feat: add inline composer to right rail",
      meta: [
        { text: "3f2c1a9", tone: "strong" },
        { text: "pushed ↑1", tone: "ok" },
        { text: "4 files" },
      ],
    });
  });

  it("singularizes the file count", () => {
    expect(autoToastContent({ ...result, file_count: 1 }).meta[2]).toEqual({
      text: "1 file",
    });
  });
});

describe("autoErrorMessage", () => {
  it("takes the first line and caps at 80 chars", () => {
    expect(autoErrorMessage(new Error("boom\nsecond line"))).toBe("boom");
    const long = "x".repeat(100);
    expect(autoErrorMessage(new Error(long))).toBe("x".repeat(77) + "…");
  });

  it("truncates on a grapheme boundary, never splitting a surrogate pair", () => {
    // A run of emoji (each a surrogate pair) longer than the cap. A naive
    // slice(0, 77) would cut a code unit and leave a lone surrogate; the result
    // must contain no unpaired surrogate (U+D800–U+DFFF) before the ellipsis.
    const out = autoErrorMessage(new Error("😀".repeat(90)));
    expect(out.endsWith("…")).toBe(true);
    expect(/[\uD800-\uDFFF]/.test(out.replace(/[\uD800-\uDBFF][\uDC00-\uDFFF]/g, ""))).toBe(false);
  });
});
