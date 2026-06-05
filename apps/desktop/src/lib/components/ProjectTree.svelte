<script lang="ts">
  // Projects → Worktrees tree (Paper Terminal shell). Project rows are mono
  // group headers: twisty · folder icon (every project, git or plain) · name ·
  // trailing kind label OR a rolled-up attention pill on a collapsed project.
  // Worktrees hang under a hairline tree spine as word-only entries: a branch
  // line, an optional meta line (state word · diffstat · PR chip), and a
  // facepile of the worktree's live Agent sessions spanning both rows.
  import { ContextMenu } from "bits-ui";
  import { invoke } from "@tauri-apps/api/core";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import { get } from "svelte/store";
  import ChevronRight from "~icons/lucide/chevron-right";
  import Folder from "~icons/lucide/folder";
  import GitBranch from "~icons/lucide/git-branch";
  import GitPullRequest from "~icons/lucide/git-pull-request";
  import Plus from "~icons/lucide/plus";
  import TerminalIcon from "~icons/lucide/terminal";
  import FolderOpen from "~icons/lucide/folder-open";
  import SquarePen from "~icons/lucide/square-pen";
  import Copy from "~icons/lucide/copy";
  import Trash2 from "~icons/lucide/trash-2";
  import ClaudeMark from "~icons/hitch/claude";
  import CodexMark from "~icons/hitch/codex";
  import toast from "svelte-french-toast";
  import { editorApp } from "../settings";
  import {
    agentActRollupByProject,
    agentStateByWorktree,
    openSession,
    prByWorktree,
    projects,
    selectedProjectId,
    selectedWorktreeId,
    sessionAgents,
    sessions,
    worktreeLineStats,
    worktrees,
  } from "../daemon";
  import { createWorktreeFor, removeProjectTarget, removeWorktreeTarget } from "../overlays";
  import {
    currentDesktopPlatform,
    revealItemLabel,
    shellSessionShortcutLabel,
  } from "../desktopPlatform";
  import {
    type AgentState,
    type Id,
    type KnownAgent,
    type PrInfo,
    type Project,
    type Worktree,
  } from "../types";

  // The three kinds of worktree, distinguished only by a subtle trailing cue
  // (never reordered — branches stay in daemon order). `main` is the repo's
  // anchor and is never removable; `managed` worktrees were created by Hitch and
  // are safe to remove; `external` ones were discovered/imported, so Hitch shows
  // them but won't manage their lifecycle. The Paper Terminal shell dropped the
  // old leading dot column; the distinction now lives in a `title` tooltip plus
  // a faint `main` suffix on the anchor branch (the one a glance benefits from).
  type WorktreeKind = "main" | "managed" | "external";
  function worktreeKind(w: Worktree): WorktreeKind {
    if (w.is_main) return "main";
    return w.is_hitch_managed ? "managed" : "external";
  }
  const KIND_TITLE: Record<WorktreeKind, string> = {
    main: "Main worktree",
    managed: "Hitch-managed worktree",
    external: "External worktree (not managed by Hitch)",
  };

  // The worktree row's state WORD: only the act states and the live working
  // word are ever shown (idle/clean carry no label). `awaiting`/`error` share
  // the oxide `--st-need` (.need); `working` is teal `--st-run` (.run). Mirrors
  // the state vocabulary in doc-design/structure.md.
  const STATE_WORD: Partial<Record<AgentState, { word: string; cls: "need" | "run" }>> = {
    running: { word: "WORKING", cls: "run" },
    "needs-approval": { word: "AWAITING", cls: "need" },
    error: { word: "ERROR", cls: "need" },
  };

  // The rolled-up project pill (collapsed project with attention items) reuses
  // the same act-state vocabulary, lowercased into a human phrase.
  const ROLLUP_WORD: Record<"needs-approval" | "error", string> = {
    "needs-approval": "awaiting input",
    error: "error",
  };

  // PR chip carries the GitHub-conventional state in a title tooltip and as a
  // state-keyed accent color on the `#N` mark — the same keying the right
  // rail's PR chip uses (open green / merged purple / closed oxide / draft
  // faint), so the tree and the rail read the same at a glance.
  function prChipTitle(pr: PrInfo): string {
    return `PR #${pr.number} (${prChipState(pr)})`;
  }
  function prChipState(pr: PrInfo): "draft" | "open" | "closed" | "merged" {
    if (pr.draft) return "draft";
    return pr.state.toLowerCase() as "open" | "closed" | "merged";
  }

  // Open a session under a worktree, selecting it first so the new session lands
  // in view. Used by the worktree context menu's launch items.
  function launch(worktree: Worktree, command: string[] | null, name: string) {
    selectWorktree(worktree);
    void openSession({ kind: "worktree", id: worktree.id }, name, command);
  }

  async function copyPath(path: string) {
    try {
      await navigator.clipboard.writeText(path);
    } catch {
      // Clipboard can be unavailable (no focus / denied); a silent no-op is
      // better than surfacing a scary error for a convenience action.
    }
  }

  const desktopPlatform = currentDesktopPlatform();
  const revealMenuLabel = revealItemLabel(desktopPlatform);
  const shellShortcutLabel = shellSessionShortcutLabel(desktopPlatform);

  // Reveal the worktree in the OS file manager via the opener plugin.
  async function revealInFileManager(path: string) {
    try {
      await revealItemInDir(path);
    } catch (err) {
      // No file manager / denied — log and no-op rather than crash the menu.
      console.error(`${revealMenuLabel} failed:`, err);
    }
  }

  // Open the worktree in the configured editor via the backend so Windows can
  // resolve common editor install locations and pass spaced paths as one arg.
  // An empty editor means "System default": the backend resolves $VISUAL /
  // $EDITOR and errors when neither is set, so failures must surface visibly.
  async function openInEditor(path: string) {
    try {
      await invoke("open_in_editor", { path, editor: get(editorApp).trim() });
    } catch (err) {
      console.error("Open in editor failed:", err);
      const msg = (err instanceof Error ? err.message : String(err)).split("\n")[0].trim();
      toast.error(msg.length > 80 ? msg.slice(0, 77) + "…" : msg);
    }
  }

  // Per-project expand state; git projects start expanded so worktrees show.
  let collapsed = $state<Record<Id, boolean>>({});

  const worktreesFor = (projectId: Id) =>
    $worktrees.filter((w) => w.project_id === projectId);

  // The facepile holds ONE circle per live Agent session (Claude/Codex) under a
  // worktree — the announced agent identity (`sessionAgents`) is the only source
  // for "is this an agent session?". Shell sessions have no announced agent and
  // so contribute no circle; a worktree running only shells shows an empty pile.
  function agentsFor(worktreeId: Id): Array<{ id: Id; agent: KnownAgent }> {
    return $sessions
      .filter((s) => s.parent.kind === "worktree" && s.parent.id === worktreeId)
      .map((s) => ({ id: s.id, agent: $sessionAgents[s.id] }))
      .filter((s): s is { id: Id; agent: KnownAgent } => s.agent != null);
  }

  function isExpanded(p: Project): boolean {
    return p.kind === "git-backed" && !collapsed[p.id];
  }

  function toggleExpand(p: Project) {
    collapsed = { ...collapsed, [p.id]: !collapsed[p.id] };
  }

  // Clicking a project row selects it (for palette context + the quick-add
  // target) and ensures its worktree list is visible. It MUST also clear the
  // selected worktree so even re-clicking the SAME project row returns the UI to
  // the project-level "choose a worktree" state instead of leaving the prior
  // branch selected.
  function selectProject(p: Project) {
    selectedProjectId.set(p.id);
    selectedWorktreeId.set(null);
    if (p.kind === "git-backed") collapsed = { ...collapsed, [p.id]: false };
  }

  function selectWorktree(w: Worktree) {
    // Set the project too: the tree shows every project's worktrees at once,
    // and the selected worktree must belong to the selected project.
    selectedProjectId.set(w.project_id);
    selectedWorktreeId.set(w.id);
  }

  function onProjectKey(event: KeyboardEvent, p: Project) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      selectProject(p);
    }
  }
</script>

<div class="tree">
  {#if $projects.length === 0}
    <p class="empty-copy">No projects yet. Add a local repo or folder to begin.</p>
  {/if}

  {#each $projects as project (project.id)}
    {@const rollup = $agentActRollupByProject[project.id]}
    {@const expanded = isExpanded(project)}
    {@const isGit = project.kind === "git-backed"}
    <div class="proj">
      <ContextMenu.Root>
        <ContextMenu.Trigger>
          {#snippet child({ props })}
            <div
              {...props}
              class="row"
              class:sel={project.id === $selectedProjectId && $selectedWorktreeId === null}
              role="button"
              tabindex="0"
              onclick={() => selectProject(project)}
              onkeydown={(e) => onProjectKey(e, project)}
            >
              {#if isGit}
                <button
                  class="tw"
                  class:open={expanded}
                  aria-label={expanded ? "Collapse" : "Expand"}
                  onclick={(e) => {
                    e.stopPropagation();
                    toggleExpand(project);
                  }}
                >
                  <ChevronRight class="icon" />
                </button>
              {:else}
                <span class="tw spacer"></span>
              {/if}

              <Folder class="folder icon" />

              <span class="pname">{project.name}</span>

              <span class="trailing">
                {#if !expanded && rollup}
                  <span class="rollup">
                    <span class="g">◆</span>
                    {rollup.count}
                    {ROLLUP_WORD[rollup.state]}
                  </span>
                {:else}
                  <span class="pkind">{isGit ? "git" : "folder"}</span>
                {/if}

                {#if isGit}
                  <button
                    class="quick-add"
                    aria-label={`New worktree in ${project.name}`}
                    title={`New worktree in ${project.name}`}
                    onclick={(e) => {
                      e.stopPropagation();
                      createWorktreeFor.set(project);
                    }}
                  >
                    <Plus class="icon" />
                  </button>
                {/if}
              </span>
            </div>
          {/snippet}
        </ContextMenu.Trigger>
        <ContextMenu.Portal>
          <ContextMenu.Content class="menu">
            {#if isGit}
              <ContextMenu.Item class="mi" onSelect={() => createWorktreeFor.set(project)}>
                <Plus class="mi-ico icon" />
                New worktree…
              </ContextMenu.Item>
              <ContextMenu.Separator class="m-sep" />
            {/if}
            <ContextMenu.Item class="mi" onSelect={() => void revealInFileManager(project.root)}>
              <FolderOpen class="mi-ico icon" />
              {revealMenuLabel}
            </ContextMenu.Item>
            <ContextMenu.Item class="mi" onSelect={() => void openInEditor(project.root)}>
              <SquarePen class="mi-ico icon" />
              Open in editor
            </ContextMenu.Item>
            <ContextMenu.Item class="mi" onSelect={() => void copyPath(project.root)}>
              <Copy class="mi-ico icon" />
              Copy path
            </ContextMenu.Item>
            <ContextMenu.Separator class="m-sep" />
            <ContextMenu.Item class="mi danger" onSelect={() => removeProjectTarget.set(project)}>
              <Trash2 class="mi-ico icon" />
              Remove project…
            </ContextMenu.Item>
          </ContextMenu.Content>
        </ContextMenu.Portal>
      </ContextMenu.Root>

      {#if expanded}
        <ul class="wt">
          {#each worktreesFor(project.id) as worktree (worktree.id)}
            {@const wtState = $agentStateByWorktree[worktree.id]}
            {@const stateWord = wtState ? STATE_WORD[wtState] : undefined}
            {@const lineStat = $worktreeLineStats[worktree.id]}
            {@const isActive = worktree.id === $selectedWorktreeId}
            {@const hasLoc = !!lineStat && (lineStat.additions > 0 || lineStat.deletions > 0)}
            {@const kind = worktreeKind(worktree)}
            {@const pr = $prByWorktree[worktree.id]}
            {@const agents = agentsFor(worktree.id)}
            {@const showMeta = !!stateWord || hasLoc || !!pr}
            <li>
              <ContextMenu.Root>
                <ContextMenu.Trigger>
                  {#snippet child({ props })}
                    <div
                      {...props}
                      class="wrow"
                      class:sel={isActive}
                      role="button"
                      tabindex="0"
                      title={KIND_TITLE[kind]}
                      onclick={() => selectWorktree(worktree)}
                      onkeydown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          selectWorktree(worktree);
                        }
                      }}
                    >
                      <div class="l1">
                        <GitBranch class="branchic icon" />
                        <span class="name">{worktree.branch}</span>
                        {#if kind === "main"}<span class="mainsuf">main</span>{/if}
                      </div>

                      {#if showMeta}
                        <div class="l2">
                          {#if stateWord}
                            <span class="statetag {stateWord.cls}">{stateWord.word}</span>
                          {/if}
                          {#if hasLoc && lineStat}
                            {#if stateWord}<span class="sep">·</span>{/if}
                            <span
                              class="diffn"
                              title={`${lineStat.additions} additions, ${lineStat.deletions} deletions`}
                            >
                              {#if lineStat.additions > 0}<span class="a">+{lineStat.additions}</span>{/if}
                              {#if lineStat.deletions > 0}<span class="d">−{lineStat.deletions}</span>{/if}
                            </span>
                          {/if}
                          {#if pr}
                            {#if stateWord || hasLoc}<span class="sep">·</span>{/if}
                            <span class="prchip {prChipState(pr)}" title={prChipTitle(pr)}>
                              <GitPullRequest class="pric icon" />#{pr.number}
                            </span>
                          {/if}
                        </div>
                      {/if}

                      <div class="pile" class:empty={agents.length === 0}>
                        {#each agents as a (a.id)}
                          <span class="h {a.agent === 'codex' ? 'codex' : 'claude'}">
                            {#if a.agent === "codex"}
                              <CodexMark class="icon" />
                            {:else}
                              <ClaudeMark class="icon" />
                            {/if}
                          </span>
                        {/each}
                      </div>
                    </div>
                  {/snippet}
                </ContextMenu.Trigger>
                <ContextMenu.Portal>
                  <ContextMenu.Content class="menu">
                    <ContextMenu.Item class="mi" onSelect={() => launch(worktree, null, "shell")}>
                      <TerminalIcon class="mi-ico icon" />
                      Open shell session<span class="mi-k">{shellShortcutLabel}</span>
                    </ContextMenu.Item>
                    <ContextMenu.Item class="mi" onSelect={() => launch(worktree, ["claude"], "claude")}>
                      <ClaudeMark class="mi-ico icon" />
                      Launch Claude
                    </ContextMenu.Item>
                    <ContextMenu.Separator class="m-sep" />
                    <ContextMenu.Item class="mi" onSelect={() => void revealInFileManager(worktree.path)}>
                      <FolderOpen class="mi-ico icon" />
                      {revealMenuLabel}
                    </ContextMenu.Item>
                    <ContextMenu.Item class="mi" onSelect={() => void openInEditor(worktree.path)}>
                      <SquarePen class="mi-ico icon" />
                      Open in editor
                    </ContextMenu.Item>
                    <ContextMenu.Item class="mi" onSelect={() => void copyPath(worktree.path)}>
                      <Copy class="mi-ico icon" />
                      Copy path
                    </ContextMenu.Item>
                    {#if worktree.is_hitch_managed && !worktree.is_main}
                      <ContextMenu.Separator class="m-sep" />
                      <ContextMenu.Item class="mi danger" onSelect={() => removeWorktreeTarget.set(worktree)}>
                        <Trash2 class="mi-ico icon" />
                        Remove worktree…
                      </ContextMenu.Item>
                    {/if}
                  </ContextMenu.Content>
                </ContextMenu.Portal>
              </ContextMenu.Root>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/each}
</div>

<style>
  .tree {
    padding: 6px 6px 8px;
  }
  .empty-copy {
    padding: 8px;
    font-family: var(--mono);
    font-size: 0.6875rem;
    color: var(--ink-2);
    line-height: 1.5;
  }

  /* ---- project row ---- */
  .row {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    min-width: 0;
    overflow: hidden;
    text-align: left;
    font-family: var(--mono);
    font-size: 0.8125rem;
    font-weight: 600;
    color: var(--ink-0);
    padding: 6px 8px;
    border-radius: 0;
    cursor: pointer;
    border: 1px solid transparent;
    background: transparent;
    transition: background 0.15s ease-out;
  }
  .row:hover {
    background: var(--paper-3);
  }
  .row:focus-visible {
    outline: 1px solid var(--iris-line);
    outline-offset: -1px;
  }
  .row.sel {
    background: var(--iris-wash);
    box-shadow: inset 0 0 0 1px var(--iris-line);
  }
  .row.sel .pname {
    color: var(--iris-ink);
  }

  .tw {
    width: 0.625rem;
    height: 0.625rem;
    flex: 0 0 0.625rem;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--ink-3);
    cursor: pointer;
  }
  .tw :global(svg) {
    width: 0.625rem;
    height: 0.625rem;
    transition: transform 0.15s ease-out;
  }
  .tw.open :global(svg) {
    transform: rotate(90deg);
  }
  .tw.spacer {
    cursor: default;
  }
  button.tw:hover {
    color: var(--ink-2);
  }

  .row :global(.folder) {
    width: 15px;
    height: 15px;
    flex: 0 0 15px;
    color: var(--ink-2);
  }

  .pname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .trailing {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }
  .pkind {
    font-size: 0.625rem;
    font-weight: 500;
    color: var(--ink-3);
  }

  /* rolled-up attention pill on a collapsed project */
  .rollup {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--mono);
    font-size: 0.625rem;
    font-weight: 600;
    color: var(--st-need);
    background: var(--st-need-wash);
    border: 1px solid var(--st-need-line);
    border-radius: 0;
    padding: 1px 7px 1px 6px;
    white-space: nowrap;
  }
  .rollup .g {
    font-size: 0.7rem;
    line-height: 1;
  }

  /* per-project quick-add: a "+" on the project row, hidden at rest, revealed on
     hover/focus. It SWAPS with the kind label / rollup pill in the trailing
     slot, so at rest it must take no space (display, not opacity) — otherwise
     the row shows a dead gap after the kind label. */
  .quick-add {
    width: 18px;
    height: 18px;
    display: none;
    place-items: center;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 0;
    background: transparent;
    color: var(--ink-3);
    cursor: pointer;
    flex: none;
    transition:
      color 0.15s ease-out,
      border-color 0.15s ease-out;
  }
  .quick-add :global(svg) {
    width: 12px;
    height: 12px;
  }
  .row:hover .quick-add,
  .row:focus-within .quick-add {
    display: grid;
  }
  .quick-add:hover {
    color: var(--iris-ink);
    border-color: var(--iris-line);
  }
  /* the kind label / rollup pill yields to the "+" on hover/focus */
  .row:hover .trailing .pkind,
  .row:focus-within .trailing .pkind,
  .row:hover .trailing .rollup,
  .row:focus-within .trailing .rollup {
    display: none;
  }

  /* ---- worktree list ---- */
  .wt {
    list-style: none;
    margin: 1px 0 6px;
    padding: 0;
    margin-left: 8px;
    border-left: 1px solid var(--line-soft);
  }
  .wt li {
    margin: 0;
  }

  .wrow {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-areas:
      "name pile"
      "meta pile";
    align-items: center;
    column-gap: 4px;
    width: 100%;
    min-width: 0;
    padding: 5px 6px 5px 7px;
    margin: 1px 0;
    border-radius: 0;
    border: 1px solid transparent;
    background: transparent;
    font-family: var(--mono);
    text-align: left;
    cursor: pointer;
    transition: background 0.15s ease-out;
  }
  .wrow:hover {
    background: var(--paper-3);
  }
  .wrow:focus-visible {
    outline: 1px solid var(--iris-line);
    outline-offset: -1px;
  }
  .wrow.sel {
    background: var(--iris-wash);
    box-shadow: inset 0 0 0 1px var(--iris-line);
    --pile-ring: var(--iris-wash);
  }

  .l1 {
    grid-area: name;
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .l1 :global(.branchic) {
    width: 12px;
    height: 12px;
    flex: 0 0 12px;
    margin-right: -1px;
    color: var(--ink-3);
  }
  .wrow.sel .l1 :global(.branchic) {
    color: var(--iris-ink);
  }
  .name {
    font-size: 0.8125rem;
    font-weight: 600;
    color: var(--ink-0);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .wrow.sel .name {
    color: var(--iris-ink);
  }
  /* a faint cue marking the repo's main anchor (replaces the old kind dot) */
  .mainsuf {
    flex: none;
    font-size: 0.5625rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .l2 {
    grid-area: meta;
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 2px;
    padding-left: 16px;
    font-size: 0.625rem;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .wrow.sel .l2 {
    color: var(--iris-ink);
  }
  .sep {
    color: var(--ink-3);
  }
  .wrow.sel .sep {
    color: var(--iris-line);
  }

  .statetag {
    font-size: 0.5625rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .statetag.need {
    color: var(--st-need);
  }
  .statetag.run {
    color: var(--st-run);
  }

  .diffn {
    display: inline-flex;
    gap: 4px;
    font-weight: 600;
    white-space: nowrap;
  }
  .diffn .a {
    color: var(--diff-add);
  }
  .diffn .d {
    color: var(--diff-del);
  }

  .prchip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-weight: 600;
    white-space: nowrap;
  }
  .prchip :global(.pric) {
    width: 12px;
    height: 12px;
    color: currentColor;
  }
  /* State-color keying mirrors the right rail's PR chip; like the state tags,
     it survives row selection (the tooltip word is the non-color channel). */
  .prchip.open {
    color: var(--st-ok);
  }
  .prchip.merged {
    color: var(--pr-merged);
  }
  .prchip.closed {
    color: var(--st-need);
  }
  .prchip.draft {
    color: var(--ink-3);
  }
  .wrow.sel .prchip.draft {
    color: var(--iris-ink);
  }

  /* ---- facepile: one ringed circle per live Agent session ---- */
  .pile {
    grid-area: pile;
    display: flex;
    align-items: center;
    flex: 0 0 auto;
  }
  .pile.empty {
    width: 4px;
  }
  .pile .h {
    width: 17px;
    height: 17px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    line-height: 1;
    background: var(--paper-2);
    box-shadow: 0 0 0 1.75px var(--pile-ring, var(--paper-1));
    margin-left: -8px;
  }
  .pile .h:first-child {
    margin-left: 0;
  }
  .pile .h :global(svg) {
    width: 12px;
    height: 12px;
  }
  .pile .h.codex :global(svg) {
    width: 13px;
    height: 13px;
  }
  .pile .h.claude {
    color: var(--mark-claude);
  }
  .pile .h.codex {
    color: var(--mark-codex);
  }
</style>
