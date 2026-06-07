// Unit tests for the destructive-confirmation copy builders (issue #30, ADR 0014):
// a remote action always includes the SSH Host name (and, where a path applies,
// the remote path) so a path that also exists locally cannot be mistaken for local
// state; local copy is unchanged. Pure string builders — no Tauri surface, no
// stores — so they load headless with no mocks.

import { describe, expect, it } from "vitest";
import {
  discardAllConfirm,
  discardFileConfirm,
  isRemoteScopeId,
  remotePathAttribution,
  removeProjectTitle,
  removeWorktreeTitle,
  scopeMetadataPrefix,
  scopedSearchMetadata,
  showsCollapsedScopeRollup,
  statusIsLive,
  statusIsStale,
  type ScopeAttribution,
} from "./scopeCopy";
import { LOCAL_SCOPE_ID } from "./types";

const HOST = "prod";
const REMOTE: ScopeAttribution = { scopeId: `ssh:${HOST}`, label: HOST, isRemote: true };
const LOCAL: ScopeAttribution = { scopeId: LOCAL_SCOPE_ID, label: "LOCAL", isRemote: false };

describe("isRemoteScopeId", () => {
  it("is false for the Local scope, true for an SSH Host scope", () => {
    expect(isRemoteScopeId(LOCAL_SCOPE_ID)).toBe(false);
    expect(isRemoteScopeId("ssh:prod")).toBe(true);
  });
});

describe("remove-worktree title", () => {
  it("names the SSH Host for a remote worktree (ADR 0014 example)", () => {
    expect(removeWorktreeTitle(REMOTE)).toBe("Remove worktree on prod?");
  });
  it("is unchanged for a local worktree", () => {
    expect(removeWorktreeTitle(LOCAL)).toBe("Remove worktree");
  });
});

describe("remove-project title", () => {
  it("names the SSH Host for a remote project", () => {
    expect(removeProjectTitle(REMOTE)).toBe("Remove project on prod?");
  });
  it("is unchanged for a local project", () => {
    expect(removeProjectTitle(LOCAL)).toBe("Remove project");
  });
});

describe("remote path attribution", () => {
  it("includes the host AND the remote path for a remote entity", () => {
    expect(remotePathAttribution(REMOTE, "/srv/app/worktrees/feature")).toBe(
      "on prod · /srv/app/worktrees/feature",
    );
  });
  it("renders no attribution line for a local entity", () => {
    expect(remotePathAttribution(LOCAL, "/Users/me/app")).toBeNull();
  });
});

describe("discard confirm copy", () => {
  it("names the host for a remote single-file discard", () => {
    expect(discardFileConfirm("src/app.ts", REMOTE)).toBe(
      "Discard changes to src/app.ts on prod?",
    );
  });
  it("is unchanged for a local single-file discard", () => {
    expect(discardFileConfirm("src/app.ts", LOCAL)).toBe("Discard changes to src/app.ts?");
  });
  it("names the host for a remote discard-all and pluralizes", () => {
    expect(discardAllConfirm(3, REMOTE)).toBe("Discard all 3 changed files on prod?");
    expect(discardAllConfirm(1, REMOTE)).toBe("Discard all 1 changed file on prod?");
  });
  it("is unchanged for a local discard-all", () => {
    expect(discardAllConfirm(2, LOCAL)).toBe("Discard all 2 changed files?");
    expect(discardAllConfirm(1, LOCAL)).toBe("Discard all 1 changed file?");
  });
});

// ---- issue #32 polish helpers ----

describe("scope liveness (statusIsLive / statusIsStale)", () => {
  it("is live only while running; stale for every other status", () => {
    expect(statusIsLive("running")).toBe(true);
    expect(statusIsStale("running")).toBe(false);
    for (const status of ["starting", "unreachable", "failed"] as const) {
      expect(statusIsLive(status)).toBe(false);
      expect(statusIsStale(status)).toBe(true);
    }
  });
});

describe("global-search scope metadata", () => {
  it("returns the host label as the muted prefix for a remote result", () => {
    expect(scopeMetadataPrefix(REMOTE)).toBe("prod");
  });
  it("returns null (no prefix) for a Local result", () => {
    expect(scopeMetadataPrefix(LOCAL)).toBeNull();
  });
  it("renders host-first `prod · project · branch` for a remote result", () => {
    expect(scopedSearchMetadata(REMOTE, "app · feature")).toBe("prod · app · feature");
  });
  it("leaves a Local result's context unchanged (no `Local ·`)", () => {
    expect(scopedSearchMetadata(LOCAL, "app · feature")).toBe("app · feature");
  });
});

describe("collapsed-host attention paging", () => {
  it("shows the rollup pill only when collapsed AND a rollup exists", () => {
    expect(showsCollapsedScopeRollup({ hasRollup: true, expanded: false })).toBe(true);
    expect(showsCollapsedScopeRollup({ hasRollup: true, expanded: true })).toBe(false);
    expect(showsCollapsedScopeRollup({ hasRollup: false, expanded: false })).toBe(false);
  });
  it("pages a collapsed host regardless of status (attention beats stale)", () => {
    // The predicate takes no status input by design: a STALE (unreachable) host
    // with an attention rollup still pages while collapsed.
    expect(showsCollapsedScopeRollup({ hasRollup: true, expanded: false })).toBe(true);
  });
});
