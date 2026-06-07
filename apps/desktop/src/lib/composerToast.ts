// Toast content for the auto commit-and-push chain (ADR 0013 amendment). The
// success toast carries the commit info the daemon returns in
// `CommitAndPushResult` — subject · short sha · pushed count · file count — in
// the app's existing svelte-french-toast recipe (the success/error helpers are
// called by both the RightRail start handler and the Composer retry). Kept out of
// daemon.ts (data layer) and the components (so the two call sites can't drift).
import type { CommitAndPushResult } from "./types";

// One-line success message: `<subject> · <sha> · pushed ↑N · M files`. The
// subject leads (it is the headline); the metadata trails as a muted-looking
// middot run, matching the mockup toast (feat: … · 3f2c1a9 · pushed ↑1 · 4 files).
export function autoToastMessage(result: CommitAndPushResult): string {
  const files = `${result.file_count} file${result.file_count === 1 ? "" : "s"}`;
  return `${result.subject} · ${result.short_sha} · pushed ↑${result.pushed_commits} · ${files}`;
}

// Short error line for a failed chain toast (mirrors RightRail.shortError: first
// line, capped). The full reason also sits under the oxide button via the chain
// store, so the toast only needs the gist.
export function autoErrorMessage(err: unknown): string {
  const msg = err instanceof Error ? err.message : String(err);
  const first = msg.split("\n")[0].trim();
  return first.length > 80 ? first.slice(0, 77) + "…" : first;
}
