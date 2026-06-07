// Worktree-aware toast wrapper (composer-redesign). Every git toast — commit,
// push, pull, fetch — must say WHICH worktree it belongs to, because the user
// can run chains in several worktrees at once and bottom-right toasts otherwise
// look identical. The branch name is how the app labels worktrees everywhere
// (ProjectTree, RightRail header), so it is the identity we put on the toast.
//
// This module is the single seam over svelte-french-toast for app toasts: it
// renders the custom AppToast.svelte component (so we get the two-line tsub/tmeta
// layout from the mockup) and carries the structured payload on the toast's
// custom `hitch` field. Call sites (RightRail/Composer, Phase 2) use
// worktreeToast(id).loading/success/error instead of toast.* directly.
import { get } from "svelte/store";
import toast from "svelte-french-toast";
import AppToast from "./components/AppToast.svelte";
import { worktrees } from "./daemon";
import type { Id } from "./types";

// A single segment of the toast's mono meta line. No tone = plain ink-2; `strong`
// = ink-1/600 (mockup .sha), `ok` = st-ok/600 (mockup .ok). Shared with
// AppToast.svelte (the renderer) and composerToast.ts (the content builder) so
// the three stay in lockstep.
export type MetaSegment = { text: string; tone?: "strong" | "ok" };

// What rides on the toast's custom `hitch` field — read back by AppToast.svelte.
// `message` is the headline (tsub); `meta` is the mono middot run (tmeta).
export type AppToastPayload = { message: string; meta: MetaSegment[] };

// svelte-french-toast's ToastOptions doesn't know about our custom `hitch` field,
// but createToast does `...opts`, so the field lands on the toast object intact.
// We cast through this locally-defined shape (kept narrow + commented) rather
// than augmenting the library's global types.
type HitchToastOptions = { id?: string; hitch: AppToastPayload };

// Build the payload for a helper call. `content` is either a bare headline string
// (loading/error and simple successes) or a structured { message, meta } (the
// rich commit-and-push success). The branch, if known, is appended as the
// TRAILING meta segment: rich successes read "subject" / "sha · pushed ↑1 · 4
// files · wild-dune-78"; simple toasts read "Staging files…" / "wild-dune-78".
// A null branch (worktree gone, or non-worktree context) omits it entirely, so
// the toast degrades to its old single-headline behavior.
function buildPayload(
  content: string | { message: string; meta?: MetaSegment[] },
  branch: string | null,
): AppToastPayload {
  const base =
    typeof content === "string"
      ? { message: content, meta: [] as MetaSegment[] }
      : { message: content.message, meta: content.meta ?? [] };
  const meta = branch ? [...base.meta, { text: branch }] : base.meta;
  return { message: base.message, meta };
}

// Returns toast helpers bound to one worktree's identity. The branch is resolved
// ONCE, here, at bind time — NOT lazily at completion. This matters: a chain can
// outlive its worktree (the user can remove the worktree mid-push), and we want
// the toast to keep showing the branch it started against rather than going blank
// or throwing when the store entry is gone by the time .success() fires.
export function worktreeToast(worktreeId: Id | null) {
  const branch =
    (worktreeId && get(worktrees).find((w) => w.id === worktreeId)?.branch) ||
    null;

  // The component class is what svelte-french-toast renders as the message; the
  // payload travels on the `hitch` option field. Both casts are needed because
  // the lib types Renderable narrowly and ToastOptions has no `hitch`.
  const opts = (id: string | undefined, payload: AppToastPayload) =>
    ({ id, hitch: payload }) as HitchToastOptions;

  return {
    loading(message: string, o?: { id?: string }): string {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return toast.loading(
        AppToast as any,
        opts(o?.id, buildPayload(message, branch)) as any,
      );
    },
    success(
      content: string | { message: string; meta?: MetaSegment[] },
      o?: { id?: string },
    ): string {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return toast.success(
        AppToast as any,
        opts(o?.id, buildPayload(content, branch)) as any,
      );
    },
    error(message: string, o?: { id?: string }): string {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return toast.error(
        AppToast as any,
        opts(o?.id, buildPayload(message, branch)) as any,
      );
    },
  };
}
