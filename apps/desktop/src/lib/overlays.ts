// Overlay open-state — the small bit of cross-cutting UI state that several
// surfaces drive (the ⌘K palette opens the same create-worktree dialog the
// tree's "New worktree…" row does, etc.). Kept out of daemon.ts (that owns the
// daemon contract, not view chrome) and shared as stores rather than
// prop-drilled, mirroring how components read daemon state directly.

import { writable } from "svelte/store";
import type { Project, Worktree } from "./types";

// ⌘K command palette.
export const commandOpen = writable(false);

// Add-project dialog (local folder / clone remote).
export const addProjectOpen = writable(false);

// Create-worktree dialog, scoped to the project it creates under (null = closed).
export const createWorktreeFor = writable<Project | null>(null);

// Remove-worktree confirmation, scoped to its target worktree (null = closed).
export const removeWorktreeTarget = writable<Worktree | null>(null);

// Create-PR dialog (also openable from its button in the Changes panel).
export const createPrOpen = writable(false);

// Settings dialog (opened from the left-rail footer "Settings" button).
export const settingsOpen = writable(false);
