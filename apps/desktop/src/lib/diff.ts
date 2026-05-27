// Unified-diff line classifier. The daemon's `git-diff` returns a unified diff
// *string* (FileDiff.diff); this turns it into the classified rows the locked
// mockup renders — hunk / add / del / ctx with a line-number gutter (.dl/.gut).
//
// This is the deliberate diff renderer for the shell, not a stopgap: the locked
// design (hitch-shell-mockup.html) shows a flat, un-highlighted classified
// diff, so a Shiki-highlighted view would deviate from the lock. Kept pure and
// framework-agnostic so it can be unit-reasoned without a DOM.

export type DiffLineKind = "hunk" | "add" | "del" | "ctx";

export type DiffLine = {
  kind: DiffLineKind;
  // The full line including its leading +/-/space, matching the mockup, which
  // renders e.g. `87` in the gutter and `+    let status = …` as the text.
  text: string;
  // The gutter number: new-side for add/ctx, old-side for del, none for hunks.
  gutter: number | null;
};

export type ParsedDiff = {
  lines: DiffLine[];
  additions: number;
  deletions: number;
  // No hunks parsed: empty diff, a mode/rename-only change, or a binary file.
  isEmpty: boolean;
  isBinary: boolean;
};

// File-header noise that precedes the hunks; not shown (the mockup starts at
// the first `@@`). `\ No newline at end of file` is dropped the same way.
function isHeaderLine(line: string): boolean {
  return (
    line.startsWith("diff --git") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ") ||
    line.startsWith("old mode ") ||
    line.startsWith("new mode ") ||
    line.startsWith("deleted file mode ") ||
    line.startsWith("new file mode ") ||
    line.startsWith("similarity index ") ||
    line.startsWith("rename ") ||
    line.startsWith("copy ") ||
    line.startsWith("\\ ")
  );
}

// `@@ -oldStart,oldLen +newStart,newLen @@ context` → the two starting numbers.
const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseDiff(diff: string): ParsedDiff {
  const lines: DiffLine[] = [];
  let additions = 0;
  let deletions = 0;
  let isBinary = false;
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;

  for (const raw of diff.split("\n")) {
    if (raw.startsWith("Binary files") || raw.includes("GIT binary patch")) {
      isBinary = true;
      continue;
    }

    const hunk = HUNK_RE.exec(raw);
    if (hunk) {
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[2]);
      inHunk = true;
      lines.push({ kind: "hunk", text: raw, gutter: null });
      continue;
    }

    if (!inHunk || isHeaderLine(raw)) continue;

    const marker = raw[0];
    if (marker === "+") {
      additions += 1;
      lines.push({ kind: "add", text: raw, gutter: newNo });
      newNo += 1;
    } else if (marker === "-") {
      deletions += 1;
      lines.push({ kind: "del", text: raw, gutter: oldNo });
      oldNo += 1;
    } else {
      // A context line (leading space) or the trailing empty split element.
      lines.push({ kind: "ctx", text: raw, gutter: newNo });
      oldNo += 1;
      newNo += 1;
    }
  }

  // A trailing newline in the diff yields one empty ctx row; drop it.
  while (lines.length > 0 && lines[lines.length - 1].kind === "ctx" && lines[lines.length - 1].text === "") {
    lines.pop();
  }

  return {
    lines,
    additions,
    deletions,
    isEmpty: lines.length === 0,
    isBinary,
  };
}
