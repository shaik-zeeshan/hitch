// Overlay open-state — the small bit of cross-cutting UI state that several
// surfaces drive (the command palette opens the same create-worktree dialog the
// tree's per-project "+" does, etc.). Kept out of daemon.ts (that owns the
// daemon contract, not view chrome) and shared as stores rather than
// prop-drilled, mirroring how components read daemon state directly.

import { writable } from "svelte/store";
import type { DaemonScopeId, Project, SshHost, Worktree } from "./types";

// Command palette.
export const commandOpen = writable(false);

// Local add-project fallback dialog. The primary flow still goes straight to
// the native folder picker (`pickAndAddProject`); this store is the explicit
// manual path-entry fallback for when the picker is unavailable or unsuitable.
export const addProjectOpen = writable(false);

// Clone-remote dialog. Remote clone stays separate from local add-project.
export const cloneProjectOpen = writable(false);

// Remote folder browser dialog (issue #28, ADR 0014). Adding a Project inside an
// SSH Host scope opens a folders-first directory browser backed by requests to
// that host's Daemon. The store holds the INITIAL target scope id the dialog
// opens at (`null` = closed). Opened from a host row's "Add project…" context
// menu (locked to that host) and from the global add menu / palette (defaulting
// to the selected scope, with a scope select). Local stays on the native picker.
export const remoteBrowserScope = writable<DaemonScopeId | null>(null);

// Add SSH Host dialog (issue #26). One required OpenSSH target field with a
// Test Connection affordance; opened from the left-rail add menu and the command
// palette. The dialog owns its own form state.
export const addSshHostOpen = writable(false);

// Remove SSH Host confirmation, scoped to its target host (null = closed).
// Removing forgets only the GUI-local entry (ADR 0014).
export const removeSshHostTarget = writable<SshHost | null>(null);

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

// Rail-toggle requests. The left/right rail visibility lives in +layout.svelte
// ($state, so a /settings round-trip doesn't reset it), and the Cmd+B / Cmd+⌥B
// chords flip it directly there. The command palette has no other path to that
// layout-local state, so it bumps these counters; +layout watches them and
// flips the matching rail. A monotonic counter (not a boolean) means every
// invocation is a distinct toggle event even when the value would repeat.
export const toggleLeftRailRequest = writable(0);
export const toggleRightRailRequest = writable(0);
