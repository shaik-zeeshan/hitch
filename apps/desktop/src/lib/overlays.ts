// Overlay open-state — the small bit of cross-cutting UI state that several
// surfaces drive (the ⌘K palette opens the same create-worktree dialog the
// tree's per-project "+" does, etc.). Kept out of daemon.ts (that owns the
// daemon contract, not view chrome) and shared as stores rather than
// prop-drilled, mirroring how components read daemon state directly.

import { writable } from "svelte/store";
import type { Project, Worktree } from "./types";

// ⌘K command palette.
export const commandOpen = writable(false);

// Clone-remote dialog. Adding a LOCAL project no longer uses a dialog — it
// opens the native folder picker directly (see `pickAndAddProject`); this
// store drives only the remote-clone form, reachable from the ⌘K palette and
// the left-rail Add-project menu.
export const cloneProjectOpen = writable(false);

// Create-worktree dialog, scoped to the project it creates under (null = closed).
export const createWorktreeFor = writable<Project | null>(null);

// Remove-project confirmation, scoped to its target project (null = closed).
export const removeProjectTarget = writable<Project | null>(null);

// Remove-worktree confirmation, scoped to its target worktree (null = closed).
export const removeWorktreeTarget = writable<Worktree | null>(null);

// Commit dialog.
export const commitOpen = writable(false);

// Create-PR dialog (also openable from its button in the Changes panel).
export const createPrOpen = writable(false);
