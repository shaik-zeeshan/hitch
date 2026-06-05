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
    defaultBase,
    diffActive,
    gitBusy,
    gitStatus,
    gitWorktreeId,
    openSession,
    pickAndAddProject,
    projects,
    selectedParent,
    selectedProject,
    selectedProjectId,
    selectedWorktreeId,
    sessions,
    worktrees,
  } from "../daemon";
  import { addProjectOpen, cloneProjectOpen, commandOpen, createPrOpen, createWorktreeFor } from "../overlays";
  import { AGENT_LABEL, type Session, type Worktree } from "../types";
  import Search from "~icons/lucide/search";
  import Claude from "~icons/hitch/claude";

  const projectName = (id: string) => $projects.find((p) => p.id === id)?.name ?? "";

  // The project a "New worktree…" lands under: the selected one if it's
  // git-backed, otherwise the first git-backed project (worktrees need git).
  const worktreeProject = $derived(
    $selectedProject?.kind === "git-backed"
      ? $selectedProject
      : ($projects.find((p) => p.kind === "git-backed") ?? null),
  );
  const canCreatePr = $derived(Boolean($gitWorktreeId && !$gitBusy && (!$defaultBase || $gitStatus?.branch !== $defaultBase)));

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
                  <Command.Item
                    class="p-item"
                    value={`worktree ${w.branch} ${projectName(w.project_id)}`}
                    keywords={[w.branch, projectName(w.project_id)]}
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
                      ><span class="mono">{w.branch}</span> <span class="ctx">· {projectName(w.project_id)}</span
                      ></span
                    >
                    {#if state}{@const label = AGENT_LABEL[state]}{#if label}<span class="status {label.cls}">{label.label}</span>{/if}{/if}
                  </Command.Item>
                {/each}

                {#each $sessions as s (s.id)}
                  <Command.Item
                    class="p-item"
                    value={`session ${s.name} ${sessionContext(s)}`}
                    keywords={[s.name]}
                    onSelect={() => run(() => jumpSession(s))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
                    >
                    <span class="pi-label">{s.name} <span class="ctx">· {sessionContext(s)}</span></span>
                  </Command.Item>
                {/each}
              </Command.GroupItems>
            </Command.Group>

            <Command.Group>
              <Command.GroupHeading class="p-group">Actions</Command.GroupHeading>
              <Command.GroupItems>
                {#if worktreeProject}
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
                {#if $selectedParent}
                  <Command.Item
                    class="p-item"
                    value="launch claude agent"
                    onSelect={() => run(() => void openSession($selectedParent!, "claude", ["claude"]))}
                  >
                    <Claude class="pi-ico pi-claude" />
                    <span class="pi-label">Launch Claude in this worktree</span>
                  </Command.Item>
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
              </Command.GroupItems>
            </Command.Group>
          </Command.Viewport>
        </Command.List>
        <div class="p-foot">
          <span><b class="kbd">↑↓</b> navigate</span>
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
</style>
