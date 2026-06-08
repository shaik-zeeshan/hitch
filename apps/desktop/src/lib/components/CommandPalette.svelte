<script lang="ts">
  // Command palette (mockup #cmdk-modal). A bits-ui Dialog (focus trap,
  // escape, overlay) hosting a bits-ui Command (fuzzy filter + arrow nav).
  // "Jump to" lists every worktree and live session; "Actions" are one-shot
  // triggers — opening a dialog (new worktree, local-path fallback, create PR,
  // clone) or firing a session. Selecting any item closes the palette first.
  import { goto } from "$app/navigation";
  import { Command, Dialog } from "bits-ui";
  import {
    activeSessionId,
    agentStateByWorktree,
    diffActive,
    gitBusy,
    gitWorktreeId,
    liveScopes,
    openSession,
    pickAndAddProject,
    projects,
    scopeAttributionForProject,
    scopeAttributionForWorktree,
    scopeForParent,
    scopeForProject,
    selectedParent,
    selectedProject,
    selectedProjectId,
    selectedWorktreeId,
    selectedWorktreeIsMain,
    sessions,
    worktrees,
  } from "../daemon";
  import { scopeMetadataPrefix } from "../scopeCopy";
  import {
    addProjectOpen,
    addSshHostOpen,
    cloneProjectOpen,
    commandOpen,
    createPrOpen,
    createWorktreeFor,
    toggleLeftRailRequest,
    toggleRightRailRequest,
  } from "../overlays";
  import { diffIgnoreWhitespace, diffStyle, diffWrap } from "../settings";
  import { AGENT_LABEL, type Session, type Worktree } from "../types";
  import { LAUNCHABLE_AGENTS } from "../sessionDisplay";
  import { bindings, comboKeys } from "../keymap";
  import { currentDesktopPlatform } from "../desktopPlatform";
  import Search from "~icons/lucide/search";

  const projectName = (id: string) => $projects.find((p) => p.id === id)?.name ?? "";

  // Chord hints for the rail-toggle rows, formatted from the keymap so the
  // palette can't drift from the actual bindings (same source the Settings
  // reference panel reads). The keymap also owns the row labels (toggle.left /
  // toggle.right "Toggle left/right rail"), reused below as the value keywords.
  const platform = currentDesktopPlatform();
  const toggleLeft = bindings.find((b) => b.id === "toggle.left")!;
  const toggleRight = bindings.find((b) => b.id === "toggle.right")!;
  const toggleLeftKeys = comboKeys(toggleLeft.combo, platform);
  const toggleRightKeys = comboKeys(toggleRight.combo, platform);

  // The project a "New worktree…" lands under: the selected one if it's
  // git-backed, otherwise the first git-backed project (worktrees need git).
  const worktreeProject = $derived(
    $selectedProject?.kind === "git-backed"
      ? $selectedProject
      : ($projects.find((p) => p.kind === "git-backed") ?? null),
  );
  // Palette ACTIONS are daemon-backed, so an action whose target scope is stale
  // (unreachable SSH Host, ADR 0014) is hidden — firing it would hit a dead daemon
  // (issue #32). `worktreeProject`'s scope gates "New worktree…"; the selected
  // parent's scope gates "Launch agent" + "Create PR". Reading `$liveScopes`
  // recomputes these as hosts attach/detach. Local stays live, so local rows are
  // unchanged.
  const worktreeProjectLive = $derived(
    worktreeProject ? $liveScopes.has(scopeForProject(worktreeProject.id)) : false,
  );
  const selectedParentLive = $derived(
    $selectedParent ? $liveScopes.has(scopeForParent($selectedParent)) : false,
  );
  const canCreatePr = $derived(Boolean($gitWorktreeId && !$gitBusy && selectedParentLive && !$selectedWorktreeIsMain));

  function run(action: () => void) {
    commandOpen.set(false);
    action();
  }

  function jumpWorktree(w: Worktree) {
    selectedProjectId.set(w.project_id);
    selectedWorktreeId.set(w.id);
  }

  function jumpSession(s: Session) {
    if (s.parent.kind === "worktree") {
      const w = $worktrees.find((x) => x.id === s.parent.id);
      if (w) {
        selectedProjectId.set(w.project_id);
        selectedWorktreeId.set(w.id);
      }
    } else {
      selectedProjectId.set(s.parent.id);
      selectedWorktreeId.set(null);
    }
    diffActive.set(false);
    activeSessionId.set(s.id);
  }

  function sessionContext(s: Session): string {
    if (s.parent.kind === "worktree") {
      const w = $worktrees.find((x) => x.id === s.parent.id);
      return w ? `session in ${w.branch}` : "session";
    }
    return `session in ${projectName(s.parent.id)}`;
  }

  // Muted local/SSH Host scope metadata for a global-search result (ADR 0014):
  // the host label (`prod`) for a remote Project/Worktree/Session, `null` for a
  // Local result (no scope noise). Worktrees resolve through their own scope; a
  // session through its parent. Reading `$worktrees`/`$projects` makes these
  // recompute when remote scopes attach/detach.
  function worktreeScopePrefix(w: Worktree): string | null {
    void $worktrees;
    return scopeMetadataPrefix(scopeAttributionForWorktree(w.id));
  }
  function sessionScopePrefix(s: Session): string | null {
    void $worktrees;
    void $projects;
    return s.parent.kind === "worktree"
      ? scopeMetadataPrefix(scopeAttributionForWorktree(s.parent.id))
      : scopeMetadataPrefix(scopeAttributionForProject(s.parent.id));
  }
</script>

<Dialog.Root bind:open={$commandOpen}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back palette-back" />
    <Dialog.Content class="palette" aria-label="Command palette" aria-describedby={undefined}>
      <Command.Root label="Search worktrees, sessions, or run a command">
        <div class="p-input">
          <Search class="icon" />
          <Command.Input placeholder="Search worktrees, sessions, or run a command…" autofocus />
        </div>
        <Command.List class="p-list">
          <Command.Viewport>
            <Command.Empty class="p-empty">No matches.</Command.Empty>

            <Command.Group>
              <Command.GroupHeading class="p-group">Jump to</Command.GroupHeading>
              <Command.GroupItems>
                {#each $worktrees as w (w.id)}
                  {@const state = $agentStateByWorktree[w.id]}
                  {@const scopePrefix = worktreeScopePrefix(w)}
                  <Command.Item
                    class="p-item"
                    value={`worktree ${scopePrefix ? `${scopePrefix} ` : ""}${w.branch} ${projectName(w.project_id)}`}
                    keywords={[w.branch, projectName(w.project_id), ...(scopePrefix ? [scopePrefix] : [])]}
                    onSelect={() => run(() => jumpWorktree(w))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                      ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle
                        cx="12"
                        cy="5"
                        r="1.6"
                      /><path d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11" /></svg
                    >
                    <span class="pi-label"
                      >{#if scopePrefix}<span class="scope-meta">{scopePrefix} ·</span> {/if}<span class="mono">{w.branch}</span> <span class="ctx">· {projectName(w.project_id)}</span
                      ></span
                    >
                    {#if state}{@const label = AGENT_LABEL[state]}{#if label}<span class="status {label.cls}">{label.label}</span>{/if}{/if}
                  </Command.Item>
                {/each}

                {#each $sessions as s (s.id)}
                  {@const scopePrefix = sessionScopePrefix(s)}
                  <Command.Item
                    class="p-item"
                    value={`session ${scopePrefix ? `${scopePrefix} ` : ""}${s.name} ${sessionContext(s)}`}
                    keywords={scopePrefix ? [s.name, scopePrefix] : [s.name]}
                    onSelect={() => run(() => jumpSession(s))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
                    >
                    <span class="pi-label">{#if scopePrefix}<span class="scope-meta">{scopePrefix} ·</span> {/if}{s.name} <span class="ctx">· {sessionContext(s)}</span></span>
                  </Command.Item>
                {/each}
              </Command.GroupItems>
            </Command.Group>

            <Command.Group>
              <Command.GroupHeading class="p-group">Actions</Command.GroupHeading>
              <Command.GroupItems>
                {#if worktreeProject && worktreeProjectLive}
                  <Command.Item
                    class="p-item"
                    value="new worktree create branch"
                    onSelect={() => run(() => createWorktreeFor.set(worktreeProject))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
                      ><line x1="8" y1="3" x2="8" y2="13" /><line x1="3" y1="8" x2="13" y2="8" /></svg
                    >
                    <span class="pi-label">New worktree…</span>
                  </Command.Item>
                {/if}
                {#if $selectedParent && selectedParentLive}
                  {#each LAUNCHABLE_AGENTS as a (a.kind)}
                    {@const Mark = a.icon}
                    <Command.Item
                      class="p-item"
                      value={`launch ${a.title} agent`}
                      onSelect={() => run(() => void openSession($selectedParent!, a.kind, a.launchArgv))}
                    >
                      <Mark class="pi-ico pi-{a.kind}" />
                      <span class="pi-label">Launch {a.title} in this worktree</span>
                    </Command.Item>
                  {/each}
                {/if}
                {#if canCreatePr}
                  <Command.Item
                    class="p-item"
                    value="create pull request pr"
                    onSelect={() => run(() => createPrOpen.set(true))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                      ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle
                        cx="12"
                        cy="5"
                        r="1.6"
                      /><path d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11" /></svg
                    >
                    <span class="pi-label">Create pull request…</span>
                  </Command.Item>
                {/if}
                <Command.Item
                  class="p-item"
                  value="add project local folder open"
                  onSelect={() => run(() => void pickAndAddProject())}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
                  >
                  <span class="pi-label">Add local project…</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="add project local path paste manual"
                  onSelect={() => run(() => addProjectOpen.set(true))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M2.5 3.5h11v9h-11z" /><path d="M4.5 6.25h7M4.5 8h5M4.5 9.75h4" /></svg
                  >
                  <span class="pi-label">Add local project by path…</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="clone remote repository git project"
                  onSelect={() => run(() => cloneProjectOpen.set(true))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                    ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle cx="12" cy="5" r="1.6" /><path
                      d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11"
                    /></svg
                  >
                  <span class="pi-label">Clone remote repository…</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="add ssh host remote daemon connection server"
                  onSelect={() => run(() => addSshHostOpen.set(true))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                    ><rect x="2.5" y="2.5" width="11" height="4.5" /><rect x="2.5" y="9" width="11" height="4.5" /><path d="M5 4.75h0M5 11.25h0" /></svg
                  >
                  <span class="pi-label">Add SSH Host…</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="open settings preferences editor drafts git"
                  onSelect={() => run(() => void goto("/settings"))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M2.5 5h6M12.5 5h1M2.5 11h1M7.5 11h6" /><circle cx="10.5" cy="5" r="1.8" /><circle
                      cx="5.5"
                      cy="11"
                      r="1.8"
                    /></svg
                  >
                  <span class="pi-label">Open settings…</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="toggle left rail sidebar tree show hide"
                  keywords={[toggleLeft.description]}
                  onSelect={() => run(() => toggleLeftRailRequest.update((n) => n + 1))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><rect x="2.5" y="3" width="11" height="10" /><line x1="6" y1="3" x2="6" y2="13" /></svg
                  >
                  <span class="pi-label">{toggleLeft.description}</span>
                  <span class="keys">{#each toggleLeftKeys as k (k)}<b class="kbd">{k}</b>{/each}</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="toggle right rail changes git panel show hide"
                  keywords={[toggleRight.description]}
                  onSelect={() => run(() => toggleRightRailRequest.update((n) => n + 1))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><rect x="2.5" y="3" width="11" height="10" /><line x1="10" y1="3" x2="10" y2="13" /></svg
                  >
                  <span class="pi-label">{toggleRight.description}</span>
                  <span class="keys">{#each toggleRightKeys as k (k)}<b class="kbd">{k}</b>{/each}</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="diff toggle split unified view side by side"
                  onSelect={() => run(() => diffStyle.set($diffStyle === "split" ? "unified" : "split"))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><rect x="2.5" y="3" width="11" height="10" /><line x1="8" y1="3" x2="8" y2="13" /></svg
                  >
                  <span class="pi-label">Diff: toggle split view</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="diff toggle line wrap soft overflow"
                  onSelect={() => run(() => diffWrap.update((v) => !v))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M2.5 4h11M2.5 8h9a2 2 0 0 1 0 4h-2.5M11 10.5 9 12l2 1.5" /><path d="M2.5 12h3" /></svg
                  >
                  <span class="pi-label">Diff: toggle line wrap</span>
                </Command.Item>
                <Command.Item
                  class="p-item"
                  value="diff toggle ignore whitespace blank changes"
                  onSelect={() => run(() => diffIgnoreWhitespace.update((v) => !v))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M3 6.5v3M6 6.5v3M10 6.5v3M13 6.5v3" /></svg
                  >
                  <span class="pi-label">Diff: toggle ignore whitespace</span>
                </Command.Item>
              </Command.GroupItems>
            </Command.Group>
          </Command.Viewport>
        </Command.List>
        <div class="p-foot">
          <span><span class="keys"><b class="kbd">↑</b><b class="kbd">↓</b></span> navigate</span>
          <span><b class="kbd">⏎</b> select</span>
          <span><b class="kbd">esc</b> dismiss</span>
        </div>
      </Command.Root>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  /* The Claude harness mark keeps its coral identity ink rather than the
     neutral icon tint the shared .pi-ico applies. The palette is portaled out
     of this component's subtree, so the rule must be :global. The selected
     row's iris ink (.p-item[data-selected] .pi-ico) still wins by specificity,
     so it inverts correctly under selection. */
  :global(.p-item .pi-ico.pi-claude) {
    color: var(--mark-claude);
  }
  :global(.p-item .pi-ico.pi-codex) {
    color: var(--mark-codex);
  }
  /* Trailing chord hint on the rail-toggle rows. Quiet by default; inverts to
     the iris ink on the selected row so the keycaps read against the wash. */
  :global(.p-item .keys .kbd) {
    color: var(--ink-2);
  }
  :global(.p-item[data-selected] .keys .kbd) {
    color: var(--iris-ink);
    border-color: var(--iris-line);
  }
</style>
