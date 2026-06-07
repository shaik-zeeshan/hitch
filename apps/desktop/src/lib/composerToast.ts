// Toast content for the auto commit-and-push chain (ADR 0013 amendment). The
// success toast carries the commit info the daemon returns in
// `CommitAndPushResult` — subject (headline) + a mono meta line of
// short sha · pushed ↑N · M files — in the app's existing svelte-french-toast
// recipe (the success/error helpers are called by both the RightRail start
// handler and the Composer retry). Kept out of daemon.ts (data layer) and the
// components (so the two call sites can't drift).
//
// As of the worktree-identity work (composer-redesign), the toast content is
// structured (subject + meta segments) rather than a single pre-joined string,
// so the worktreeToast() wrapper in appToast.ts can append the branch as a
// trailing meta segment and AppToast.svelte can render each segment with its own
// tone (sha → strong, pushed ↑N → ok). See the visual lock in
// doc-design/mockup-composer.html (pane 10, the .tmsg toast: tsub + tmeta).
import type { CommitAndPushResult } from "./types";
import type { MetaSegment } from "./appToast";

// Structured success content for the auto chain toast. `message` is the headline
// (the commit subject); `meta` is the mono middot run rendered under it. This
// matches the mockup toast EXACTLY (pane 10):
//   tsub:  "feat: add inline composer to right rail"
//   tmeta: <sha>3f2c1a9</sha> · <ok>pushed ↑1</ok> · 4 files
// The short sha gets the `strong` tone (mockup .sha → ink-1/600), the push count
// gets the `ok` tone (mockup .ok → st-ok/600), the file count is plain ink-2.
export function autoToastContent(result: CommitAndPushResult): {
  message: string;
  meta: MetaSegment[];
} {
  const files = `${result.file_count} file${result.file_count === 1 ? "" : "s"}`;
  return {
    message: result.subject,
    meta: [
      { text: result.short_sha, tone: "strong" },
      { text: `pushed ↑${result.pushed_commits}`, tone: "ok" },
      { text: files },
    ],
  };
}

// Short error line for a failed chain toast: first line, capped. The single source
// for the rail's error-toast wording — RightRail (commit/push/pull/fetch) and
// ProjectTree (open-in-editor) call this directly. The full reason also sits under
// the oxide button via the chain store, so the toast only needs the gist.
export function autoErrorMessage(err: unknown): string {
  const msg = err instanceof Error ? err.message : String(err);
  const first = msg.split("\n")[0].trim();
  return first.length > 80 ? first.slice(0, 77) + "…" : first;
}
