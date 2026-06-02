import { describe, expect, it } from "vitest";
import { parseDiff } from "./diff";

describe("parseDiff", () => {
  it("keeps three-digit added line gutters as single line numbers", () => {
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

    expect(parsed.lines.map((line) => line.gutter)).toEqual([null, 120, 121, 122, 123]);
    expect(parsed.lines.map((line) => line.text)).toEqual([
      "@@ -120,2 +120,4 @@ function demo() {",
      "   keep();",
      "+  changedOne();",
      "+  changedTwo();",
      "   keepAgain();",
    ]);
  });
});
