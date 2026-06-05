<script lang="ts">
  // Left rail (Paper Terminal shell): a 38px PROJECTS header over the scrolling
  // Projects→Worktrees tree over a counts-only footer. The header's add button
  // is a split control — primary click opens the native folder picker (the
  // common case: add a local repo/folder); the caret menu keeps that fast path,
  // adds an explicit manual local-path fallback, and leaves remote clone in its
  // own dialog. Settings is no longer footed here (it lives on the /settings
  // route and the command palette); the footer is counts only.
  import { DropdownMenu } from "bits-ui";
  import Plus from "~icons/lucide/plus";
  import ChevronDown from "~icons/lucide/chevron-down";
  import Folder from "~icons/lucide/folder";
  import FileText from "~icons/lucide/file-text";
  import GitBranch from "~icons/lucide/git-branch";
  import ProjectTree from "./ProjectTree.svelte";
  import { pickAndAddProject, sessions, worktrees } from "../daemon";
  import { addProjectOpen, cloneProjectOpen } from "../overlays";

  let { collapsed = false }: { collapsed?: boolean } = $props();

  // Footer counts. A worktree is "active" when it hosts at least one live
  // session (agent or shell); the count emphasises how much of the tree is
  // currently in play, not how many worktrees exist.
  const sessionCount = $derived($sessions.length);
  const activeWorktreeCount = $derived(
    new Set(
      $sessions
        .filter((s) => s.parent.kind === "worktree")
        .map((s) => s.parent.id),
    ).size,
  );
  const worktreeCount = $derived($worktrees.length);
  const plural = (n: number, word: string) => `${word}${n === 1 ? "" : "s"}`;
</script>

<aside class="rail-left" class:collapsed>
  <header class="rail-head">
    <span class="rail-title">Projects</span>
    <div class="add-split">
      <button
        class="add"
        aria-label="Add project"
        title="Add project"
        onclick={() => void pickAndAddProject()}
      >
        <Plus class="icon" />
      </button>
      <DropdownMenu.Root>
        <DropdownMenu.Trigger>
          {#snippet child({ props })}
            <button {...props} class="add-more" aria-label="More add options" title="More add options">
              <ChevronDown class="icon" />
            </button>
          {/snippet}
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content class="menu" align="end" side="bottom" sideOffset={6}>
            <DropdownMenu.Item class="mi" onSelect={() => void pickAndAddProject()}>
              <Folder class="mi-ico icon" />
              Add local folder…
            </DropdownMenu.Item>
            <DropdownMenu.Item class="mi" onSelect={() => addProjectOpen.set(true)}>
              <FileText class="mi-ico icon" />
              Enter local path…
            </DropdownMenu.Item>
            <DropdownMenu.Item class="mi" onSelect={() => cloneProjectOpen.set(true)}>
              <GitBranch class="mi-ico icon" />
              Clone remote repository…
            </DropdownMenu.Item>
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
    </div>
  </header>

  <div class="rail-scroll">
    <ProjectTree />
  </div>

  <footer class="rail-foot">
    <span class="k">{sessionCount}</span>
    {plural(sessionCount, "session")}
    <span class="sep">·</span>
    <span class="k">{activeWorktreeCount}</span>
    of
    <span class="k">{worktreeCount}</span>
    {plural(worktreeCount, "worktree")} active
  </footer>
</aside>

<style>
  .rail-left {
    background: var(--paper-1);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    transition: opacity 0.18s ease-out;
  }
  .rail-left.collapsed {
    opacity: 0;
    pointer-events: none;
  }

  /* 38px header on the shared baseline grid (matches the center tab strip and
     the right rail header so the three columns read as one aligned system). */
  .rail-head {
    flex: 0 0 38px;
    height: 38px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 8px 0 12px;
    border-bottom: 1px solid var(--line);
  }
  .rail-title {
    font-size: 0.6875rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    font-weight: 700;
    color: var(--ink-2);
  }

  .add-split {
    margin-left: auto;
    display: flex;
    align-items: stretch;
    gap: 1px;
  }
  .add {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 0;
    background: var(--paper-2);
    color: var(--ink-2);
    cursor: pointer;
    transition:
      color 0.18s ease-out,
      border-color 0.18s ease-out,
      background 0.18s ease-out;
  }
  .add :global(svg) {
    width: 13px;
    height: 13px;
  }
  .add:hover {
    color: var(--ink-1);
    border-color: var(--ink-3);
  }
  .add-more {
    width: 16px;
    height: 20px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid var(--line);
    border-left: none;
    border-radius: 0;
    background: var(--paper-2);
    color: var(--ink-3);
    cursor: pointer;
    transition:
      color 0.18s ease-out,
      border-color 0.18s ease-out,
      background 0.18s ease-out;
  }
  .add-more :global(svg) {
    width: 11px;
    height: 11px;
  }
  .add-more:hover,
  .add-more[data-state="open"] {
    color: var(--ink-1);
    border-color: var(--ink-3);
  }

  .rail-scroll {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
  }

  /* counts-only footer: a single quiet line, emphasised numerals (.k). No
     daemon-ownership line (the daemon indicator lives in the top bar) and no
     add/settings buttons (those moved to the header / the /settings route). */
  .rail-foot {
    flex: 0 0 auto;
    border-top: 1px solid var(--line);
    background: linear-gradient(var(--paper-1), var(--paper-3));
    padding: 7px 12px;
    font-family: var(--mono);
    font-size: 0.625rem;
    line-height: 1.6;
    color: var(--ink-2);
  }
  .rail-foot .k {
    color: var(--ink-1);
    font-weight: 600;
  }
  .rail-foot .sep {
    color: var(--ink-3);
    margin: 0 2px;
  }
</style>
