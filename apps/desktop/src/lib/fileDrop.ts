// Drag-and-drop of OS files onto a terminal: dropping files inserts their
// (shell-escaped) absolute paths at the active shell prompt — the same thing
// every native terminal (Terminal.app, iTerm2, WezTerm) does.
//
// Why a single global listener and not a DOM ondrop on each .term-host: in a
// Tauri webview the native window layer intercepts OS file drags BEFORE the
// webview sees them, so the browser drag events (ondrop/ondragover) never fire
// for real files. The only signal is Tauri's window-global `onDragDropEvent`,
// which fires ONCE per drag regardless of which pane it's over and carries no
// target element — just file paths and a physical cursor position. We turn
// that position back into a target by hit-testing the DOM (`elementFromPoint`)
// and walking up to the nearest `[data-session-id]` (set on each terminal's
// host). A drop that lands on a rail, the diff, or empty space resolves to no
// host and is ignored — "under-cursor" routing with a single listener.
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writable } from "svelte/store";
import toast from "svelte-french-toast";
import { scopeForSession, scopeLabel, sendInput } from "./daemon";
import { LOCAL_SCOPE_ID } from "./types";
import type { Id } from "./types";
import UploadToast from "./components/UploadToast.svelte";
import type { UploadToastPayload } from "./components/UploadToast.svelte";

// The terminal currently under a file drag (or null). Terminal.svelte subscribes
// and paints a drop-target ring so the user can see exactly where the paths will
// land before releasing.
export const dropTargetSession = writable<Id | null>(null);

// Windows uses backslash as the path separator, so POSIX backslash-escaping
// would corrupt paths there; quoting style is chosen per-platform. The webview
// UA string is the dependency-free way to tell — `@tauri-apps/plugin-os` would
// be the alternative but isn't installed.
function isWindows(): boolean {
  return (
    typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent)
  );
}

// POSIX: backslash-escape every character outside a conservative safe set so
// bash AND fish both treat the whole path as a single literal word — spaces,
// parens, `$`, quotes, globs, everything. This matches what dragging a file
// into Terminal.app produces and is safe against paths like `$(rm -rf ~)`.
function escapePosix(path: string): string {
  return path.replace(/[^A-Za-z0-9_./:@%+,=-]/g, (c) => `\\${c}`);
}

// Windows: a filename can't legally contain a double quote, so wrapping in
// double quotes is lossless. Only quote when the path holds a character that
// would otherwise break cmd/PowerShell tokenization; bare paths stay bare.
function quoteWindows(path: string): string {
  return /[\s&()[\]{}^=;!'+,`~%]/.test(path) ? `"${path}"` : path;
}

// Turn the dropped absolute paths into the text to inject at the prompt:
// each path escaped/quoted for the host shell, space-separated, with a trailing
// space so the next path (or typed argument) doesn't run into the last one.
export function formatDroppedPaths(
  paths: string[],
  windows = isWindows(),
): string {
  const escape = windows ? quoteWindows : escapePosix;
  return paths.map(escape).join(" ") + " ";
}

// The remote daemon's OS family, returned by the upload command so inserted
// remote paths get the right shell quoting (issue #31). Mirrors hitch-proto's
// `OsFamily` kebab-case serialization.
export type RemoteOsFamily = "unix" | "windows";

// Quote uploaded REMOTE paths for the remote host's shell — the same escapers as
// the local drop, but selected by the remote platform rather than this GUI's. So
// a Windows GUI dropping onto a Unix remote still POSIX-escapes, and vice versa.
export function formatRemotePaths(
  paths: string[],
  osFamily: RemoteOsFamily,
): string {
  return formatDroppedPaths(paths, osFamily === "windows");
}

// Resolve the terminal session under a physical (device-pixel) cursor position,
// or null if the point isn't over a terminal. Tauri reports the drop point in
// physical pixels relative to the window's top-left; `elementFromPoint` wants
// CSS pixels relative to the viewport top-left. The webview fills the window
// (Overlay titlebar — no native chrome offset), so dividing by devicePixelRatio
// is the only conversion needed. (Known Tauri caveat: the position can be off
// while devtools is docked open; fine in production.)
function sessionAtPosition(position: { x: number; y: number }): Id | null {
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(position.x / dpr, position.y / dpr);
  const host = el?.closest<HTMLElement>("[data-session-id]");
  return host?.dataset.sessionId ?? null;
}

// Register the single app-wide file-drop listener. Call once from the root
// layout's onMount and invoke the returned unlisten on teardown.
export async function initFileDrop(): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().onDragDropEvent((event) => {
    const payload = event.payload;
    switch (payload.type) {
      case "enter":
      case "over":
        // Track the hovered terminal so the highlight follows the cursor across
        // panes during the drag.
        dropTargetSession.set(sessionAtPosition(payload.position));
        break;
      case "leave":
        dropTargetSession.set(null);
        break;
      case "drop": {
        dropTargetSession.set(null);
        const sessionId = sessionAtPosition(payload.position);
        // Dropped outside any terminal (rail/diff/empty), or an empty drag.
        if (!sessionId || payload.paths.length === 0) return;
        handleDrop(sessionId, payload.paths);
        break;
      }
    }
  });
}

// Insert dropped paths at a session's prompt. A LOCAL session inserts the
// shell-quoted local absolute paths directly (unchanged). A REMOTE session can't
// see local paths, so it UPLOADS the files to the remote daemon and inserts the
// returned remote paths (issue #31, ADR 0014). Factored out of the listener so it
// is unit-testable without synthesizing a Tauri drag event.
function handleDrop(sessionId: Id, paths: string[]): void {
  if (scopeForSession(sessionId) === LOCAL_SCOPE_ID) {
    sendInput(sessionId, formatDroppedPaths(paths));
    return;
  }
  // Remote: stream the files up, then insert the actual remote paths. Fire and
  // forget from the listener's perspective; progress + errors surface as toasts.
  void uploadAndInsert(sessionId, paths);
}

// Mirrors the Rust `UploadFileResult` enum (ssh_pool.rs). One per dropped path.
type UploadFileResult =
  | { type: "uploaded"; name: string; remotePath: string }
  | { type: "rejected-directory"; name: string }
  | { type: "failed"; name: string; error: string };

type UploadBatchResult = {
  osFamily: RemoteOsFamily;
  cancelled: boolean;
  files: UploadFileResult[];
};

// The Rust `hitch-upload-progress` event payload.
type UploadProgress = {
  batchId: string;
  fileIndex: number;
  fileCount: number;
  fileName: string;
  sentBytes: number;
  totalBytes: number;
};

// Mint a unique batch id per drop so concurrent uploads (two panes) don't share a
// cancel flag or progress toast.
function newBatchId(): string {
  return `upload-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

// Drive a remote file-drop: stream the files to the remote daemon with a live
// progress toast that offers Cancel, then insert the uploaded remote paths
// (remote-platform-quoted) and toast any rejected directories / failures.
async function uploadAndInsert(sessionId: Id, paths: string[]): Promise<void> {
  const scope = scopeForSession(sessionId);
  const batchId = newBatchId();
  const host = scopeLabel(scope);

  // A loading toast (custom UploadToast component) that updates in place as bytes
  // flow and hosts a Cancel button wired to this batch (issue #31). The percent +
  // headline + batch id ride on the toast's custom `upload` field.
  const toastId = `upload-toast-${batchId}`;
  const showProgress = (payload: UploadToastPayload) =>
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    toast.loading(UploadToast as any, {
      id: toastId,
      duration: Infinity,
      upload: payload,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any);
  showProgress({ message: `Uploading to ${host}…`, percent: 0, batchId });

  const unlisten = await listen<UploadProgress>(
    "hitch-upload-progress",
    (event) => {
      const p = event.payload;
      if (p.batchId !== batchId) return;
      const pct =
        p.totalBytes > 0
          ? Math.floor((p.sentBytes / p.totalBytes) * 100)
          : 100;
      const fileLabel =
        p.fileCount > 1 ? ` (${p.fileIndex + 1}/${p.fileCount})` : "";
      showProgress({
        message: `Uploading ${p.fileName}${fileLabel}…`,
        percent: pct,
        batchId,
      });
    },
  );

  let result: UploadBatchResult;
  try {
    result = await invoke<UploadBatchResult>("upload_files_to_session", {
      scope,
      batchId,
      sessionId,
      paths,
    });
  } catch (err) {
    unlisten();
    toast.dismiss(toastId);
    toast.error(
      `Upload to ${host} failed: ${err instanceof Error ? err.message : String(err)}`,
      { duration: 5000 },
    );
    return;
  }
  unlisten();

  const uploaded = result.files.filter(
    (f): f is Extract<UploadFileResult, { type: "uploaded" }> =>
      f.type === "uploaded",
  );
  const dirs = result.files.filter((f) => f.type === "rejected-directory");
  const failed = result.files.filter(
    (f): f is Extract<UploadFileResult, { type: "failed" }> =>
      f.type === "failed",
  );

  // Cancelled before insertion: insert nothing (per AC), just confirm + report any
  // directory rejections seen before the cancel.
  if (result.cancelled) {
    toast(`Upload to ${host} cancelled`, { id: toastId, duration: 3000 });
    reportRejectedDirectories(dirs.length);
    return;
  }

  // Insert the actual remote paths, quoted for the remote shell, space-separated
  // with a trailing space — the same shape as the local drop.
  if (uploaded.length > 0) {
    sendInput(
      sessionId,
      formatRemotePaths(
        uploaded.map((f) => f.remotePath),
        result.osFamily,
      ),
    );
    toast.success(
      uploaded.length === 1
        ? `Uploaded ${uploaded[0].name} to ${host}`
        : `Uploaded ${uploaded.length} files to ${host}`,
      { id: toastId, duration: 3000 },
    );
  } else {
    toast.dismiss(toastId);
  }

  reportRejectedDirectories(dirs.length);
  for (const f of failed) {
    toast.error(`Couldn't upload ${f.name}: ${f.error}`, { duration: 5000 });
  }
}

// Toast the explicit "no recursive upload" copy for any dropped directories
// (ADR 0014). Mixed drops still upload the files; only the folders are rejected.
function reportRejectedDirectories(count: number): void {
  if (count <= 0) return;
  toast.error(
    count === 1
      ? "Folders can't be dropped onto remote sessions yet — recursive upload isn't supported."
      : `${count} folders can't be dropped onto remote sessions yet — recursive upload isn't supported.`,
    { duration: 5000 },
  );
}

// Cancel an upload batch by id (issue #31). The UploadToast Cancel button calls
// this; cancelling before insertion means no paths are inserted (per the AC).
export function cancelUploadBatch(batchId: string): void {
  void invoke("cancel_upload", { batchId });
}

// Test seam for the drop router (issues #29/#31). Exercised by the unit tests;
// not part of the runtime drop path (the listener calls `handleDrop` directly
// after hit-testing the cursor).
export function handleDropForTest(sessionId: Id, paths: string[]): void {
  handleDrop(sessionId, paths);
}
