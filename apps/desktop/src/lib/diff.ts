// Unified-diff summariser. The daemon's `git-diff` returns a unified diff
// *string* (FileDiff.diff); this walks it to produce the cheap header facts the
// diff view needs without spinning up the syntax highlighter.
//
// The diff view itself renders through @pierre/diffs (Shiki syntax highlighting;
// see DiffTab.svelte). This module is retained for the things it does well
// without the highlighter: the +N/−N add/del counts in the header and the
// binary / empty (mode/rename-only) detection that drives the fallback states.
// Kept pure and framework-agnostic so it can be unit-reasoned without a DOM.

export type ParsedDiff = {
  additions: number;
  deletions: number;
  // No hunk content parsed: empty diff, a mode/rename-only change, or a binary
  // file.
  isEmpty: boolean;
  isBinary: boolean;
};

// File-header noise that precedes the hunks; not counted. `\ No newline at end
// of file` is dropped the same way.
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

// `@@ -oldStart,oldLen +newStart,newLen @@ context` → matches a hunk header.
const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseDiff(diff: string): ParsedDiff {
  let additions = 0;
  let deletions = 0;
  let isBinary = false;
  let inHunk = false;
  // Counts the meaningful rows inside hunks (hunk headers, adds, dels, context).
  // A trailing empty context row — the empty element a trailing "\n" leaves
  // behind in the split — is not counted, matching the old trailing-row trim.
  let contentRows = 0;

  for (const raw of diff.split("\n")) {
    if (raw.startsWith("Binary files") || raw.includes("GIT binary patch")) {
      isBinary = true;
      continue;
    }

    const hunk = HUNK_RE.exec(raw);
    if (hunk) {
      inHunk = true;
      contentRows += 1;
      continue;
    }

    if (!inHunk || isHeaderLine(raw)) continue;

    const marker = raw[0];
    if (marker === "+") {
      additions += 1;
      contentRows += 1;
    } else if (marker === "-") {
      deletions += 1;
      contentRows += 1;
    } else if (raw === "") {
      // A trailing empty split element (or a blank context line). The old
      // classifier trimmed a trailing empty context row before computing
      // emptiness; reproduce that by not counting it.
    } else {
      // A context line (leading space).
      contentRows += 1;
    }
  }

  return {
    additions,
    deletions,
    isEmpty: contentRows === 0,
    isBinary,
  };
}
