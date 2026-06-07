import { describe, expect, it } from "vitest";
import { parseDiff } from "./diff";

describe("parseDiff", () => {
  it("counts adds and deletions within a hunk, ignoring headers", () => {
    const parsed = parseDiff([
      "diff --git a/file.ts b/file.ts",
      "index 1111111..2222222 100644",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -120,2 +120,4 @@ function demo() {",
      "   keep();",
      "+  changedOne();",
      "+  changedTwo();",
      "   keepAgain();",
      "",
    ].join("\n"));

    expect(parsed.additions).toBe(2);
    expect(parsed.deletions).toBe(0);
    expect(parsed.isEmpty).toBe(false);
    expect(parsed.isBinary).toBe(false);
  });

  it("counts deletions and added lines together", () => {
    const parsed = parseDiff([
      "diff --git a/file.ts b/file.ts",
      "index 1111111..2222222 100644",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -1,4 +1,3 @@",
      " ctx",
      "-removed one",
      "-removed two",
      "+added one",
      " ctxAgain",
      "",
    ].join("\n"));

    expect(parsed.additions).toBe(1);
    expect(parsed.deletions).toBe(2);
    expect(parsed.isEmpty).toBe(false);
    expect(parsed.isBinary).toBe(false);
  });

  it("treats an empty diff string as empty", () => {
    const parsed = parseDiff("");

    expect(parsed.additions).toBe(0);
    expect(parsed.deletions).toBe(0);
    expect(parsed.isEmpty).toBe(true);
    expect(parsed.isBinary).toBe(false);
  });

  it("treats a mode/rename-only change with no hunks as empty", () => {
    const parsed = parseDiff([
      "diff --git a/old.ts b/new.ts",
      "similarity index 100%",
      "rename from old.ts",
      "rename to new.ts",
      "",
    ].join("\n"));

    expect(parsed.additions).toBe(0);
    expect(parsed.deletions).toBe(0);
    expect(parsed.isEmpty).toBe(true);
    expect(parsed.isBinary).toBe(false);
  });

  it("flags a binary file change and reports it as empty of line content", () => {
    const parsed = parseDiff([
      "diff --git a/img.png b/img.png",
      "index 1111111..2222222 100644",
      "Binary files a/img.png and b/img.png differ",
      "",
    ].join("\n"));

    expect(parsed.isBinary).toBe(true);
    expect(parsed.additions).toBe(0);
    expect(parsed.deletions).toBe(0);
    expect(parsed.isEmpty).toBe(true);
  });

  it("flags a GIT binary patch", () => {
    const parsed = parseDiff([
      "diff --git a/img.png b/img.png",
      "index 1111111..2222222 100644",
      "GIT binary patch",
      "literal 12345",
      "",
    ].join("\n"));

    expect(parsed.isBinary).toBe(true);
  });

  it("does not count the '\\ No newline at end of file' marker", () => {
    const parsed = parseDiff([
      "diff --git a/file.ts b/file.ts",
      "index 1111111..2222222 100644",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -1 +1 @@",
      "-old",
      "\\ No newline at end of file",
      "+new",
      "\\ No newline at end of file",
    ].join("\n"));

    expect(parsed.additions).toBe(1);
    expect(parsed.deletions).toBe(1);
    expect(parsed.isEmpty).toBe(false);
  });

  it("does not count adds/dels that appear before any hunk header", () => {
    const parsed = parseDiff([
      "diff --git a/file.ts b/file.ts",
      "new file mode 100644",
      "index 0000000..2222222",
      "--- /dev/null",
      "+++ b/file.ts",
      "@@ -0,0 +1,2 @@",
      "+first",
      "+second",
      "",
    ].join("\n"));

    expect(parsed.additions).toBe(2);
    expect(parsed.deletions).toBe(0);
    expect(parsed.isEmpty).toBe(false);
  });
});
