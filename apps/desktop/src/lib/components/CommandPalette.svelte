<script lang="ts">
  // ⌘K command palette (mockup #cmdk-modal). A bits-ui Dialog (focus trap,
  // escape, overlay) hosting a bits-ui Command (fuzzy filter + arrow nav).
  // "Jump to" lists every worktree and live session; "Actions" are one-shot
  // triggers — opening a dialog (new worktree, add project, create PR) or
  // firing a session. Selecting any item closes the palette first, then runs.
  import { Command, Dialog } from "bits-ui";
  import {
    activeSessionId,
    agentStateByWorktree,
    diffActive,
    gitWorktreeId,
    openSession,
    projects,
    selectedParent,
    selectedProject,
    selectedProjectId,
    selectedWorktreeId,
    sessions,
    worktrees,
  } from "../daemon";
  import { addProjectOpen, commandOpen, createPrOpen, createWorktreeFor } from "../overlays";
  import { AGENT_LABEL, type Session, type Worktree } from "../types";

  const projectName = (id: string) => $projects.find((p) => p.id === id)?.name ?? "";

  // The project a "New worktree…" lands under: the selected one if it's
  // git-backed, otherwise the first git-backed project (worktrees need git).
  const worktreeProject = $derived(
    $selectedProject?.kind === "git-backed"
      ? $selectedProject
      : ($projects.find((p) => p.kind === "git-backed") ?? null),
  );

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
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
            ><circle cx="7" cy="7" r="4.5" /><line x1="10.5" y1="10.5" x2="14" y2="14" /></svg
          >
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
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle
                        cx="12"
                        cy="6"
                        r="1.8"
                      /><path d="M4 5.8v4.4M5.7 4.5h5.3M11 7.7c0 1.5-1.4 2.4-3.2 2.4H5.8" /></svg
                    >
                    <span class="pi-label"
                      ><span class="mono">{w.branch}</span> <span class="ctx">· {projectName(w.project_id)}</span
                      ></span
                    >
                    {#if state}<span class="status {AGENT_LABEL[state].cls}">{AGENT_LABEL[state].label}</span>{/if}
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
                    <span class="pi-ico" style="color:var(--warn); font-size:13px; display:grid; place-items:center"
                      >✳</span
                    >
                    <span class="pi-label">Launch Claude in this worktree</span>
                  </Command.Item>
                {/if}
                {#if $gitWorktreeId}
                  <Command.Item
                    class="p-item"
                    value="create pull request pr"
                    onSelect={() => run(() => createPrOpen.set(true))}
                  >
                    <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle
                        cx="12"
                        cy="6"
                        r="1.8"
                      /><path d="M4 5.8v4.4M5.7 4.5h5.3M11 7.7c0 1.5-1.4 2.4-3.2 2.4H5.8" /></svg
                    >
                    <span class="pi-label">Create pull request…</span>
                  </Command.Item>
                {/if}
                <Command.Item
                  class="p-item"
                  value="add project local clone"
                  onSelect={() => run(() => addProjectOpen.set(true))}
                >
                  <svg class="pi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
                    ><line x1="8" y1="3" x2="8" y2="13" /><line x1="3" y1="8" x2="13" y2="8" /></svg
                  >
                  <span class="pi-label">Add project…</span>
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
