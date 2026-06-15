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
  import Cloud from "~icons/lucide/cloud";
  import RotateCw from "~icons/lucide/rotate-cw";
  import Folder from "~icons/lucide/folder";
  import GitBranch from "~icons/lucide/git-branch";
  import GitPullRequest from "~icons/lucide/git-pull-request";
  import Plus from "~icons/lucide/plus";
  import TerminalIcon from "~icons/lucide/terminal";
  import FolderOpen from "~icons/lucide/folder-open";
  import SquarePen from "~icons/lucide/square-pen";
  import Copy from "~icons/lucide/copy";
  import Trash2 from "~icons/lucide/trash-2";
  import toast from "svelte-french-toast";
  import { autoErrorMessage, logAutoError } from "../composerToast";
  import { editorApp } from "../settings";
  import {
    agentActRollupByProject,
    agentStateByWorktree,
    daemonScopesOrdered,
    liveScopes,
    openSession,
    prByWorktree,
    projectsByScope,
    projects,
    retrySshHost,
    selectedProjectId,
    selectedWorktreeId,
    sessionAgents,
    sessions,
    worktreeLineStats,
    worktrees,
  } from "../daemon";
  import { LAUNCHABLE_AGENTS, TAB_MARK, sessionTabKind } from "../sessionDisplay";
  import {
    createWorktreeFor,
    removeProjectTarget,
    removeSshHostTarget,
    removeWorktreeTarget,
  } from "../overlays";
  import { sshHosts } from "../sshHosts";
  import { focusedPane, matchBinding } from "../keymap";
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
  // them but won't manage their lifecycle. Keep that ownership visible as one
  // quiet suffix beside the branch name; the row title carries the full phrase.
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
  const KIND_CUE: Partial<Record<WorktreeKind, string>> = {
    managed: "hitch",
    external: "ext",
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
      logAutoError("open in editor", err);
      toast.error(autoErrorMessage(err));
    }
  }

  // Per-project expand state; git projects start expanded so worktrees show.
  let collapsed = $state<Record<Id, boolean>>({});

  const projectsForScope = (scopeId: Id): Project[] => $projectsByScope[scopeId] ?? [];

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

  // ---- roving tabindex (keyboard navigation, plan slice 3) ----
  // The tree is a flat sequence of VISIBLE rows for keyboard purposes: every
  // project row, plus the worktree rows of each EXPANDED project (collapsed
  // projects contribute none — ↑/↓ skip their hidden children). One row is the
  // roving target (tabindex 0); all others are tabindex -1, so Tab lands on the
  // current row and arrows move between rows without leaving the pane. Rows are
  // keyed `proj:<id>` / `wt:<id>` so the active key survives reorders/refreshes
  // (the element refs are re-collected each render via the `bind:this` actions).
  type Row =
    | { key: string; kind: "project"; project: Project }
    | { key: string; kind: "worktree"; worktree: Worktree };

  const visibleRows = $derived.by<Row[]>(() => {
    const rows: Row[] = [];
    // Iterate scopes only to preserve project ORDER (Local first, hosts alpha);
    // scopes no longer have their own visible/roving rows — the tree is flat.
    for (const scope of $daemonScopesOrdered) {
      for (const project of projectsForScope(scope.id)) {
        rows.push({ key: `proj:${project.id}`, kind: "project", project });
        if (isExpanded(project)) {
          for (const worktree of worktreesFor(project.id)) {
            rows.push({ key: `wt:${worktree.id}`, kind: "worktree", worktree });
          }
        }
      }
    }
    return rows;
  });

  // The roving target's key. Null until the first row mounts / focus arrives;
  // resolved to a concrete key (first visible row, or the selected row) lazily
  // by `activeKey()` so it tracks the live selection without extra effects.
  let activeRowKey = $state<string | null>(null);
  // Live element refs, keyed like the rows, for programmatic focus + scroll.
  const rowEls = new Map<string, HTMLElement>();

  // Svelte action: register a row's element under its stable key for
  // programmatic focus/scroll. Rows are keyed in the {#each}, so a given node's
  // key never changes for its lifetime; cleaned up on destroy.
  function registerRow(el: HTMLElement, key: string) {
    rowEls.set(key, el);
    return {
      destroy() {
        if (rowEls.get(key) === el) rowEls.delete(key);
      },
    };
  }

  // The effective roving key: prefer an explicit roving target if it's still
  // visible; else the selected worktree/project row; else the first row. Keeps
  // exactly one row at tabindex 0 even as selection changes elsewhere (clicks,
  // palette) without the roving state going stale.
  function activeKey(): string | null {
    const rows = visibleRows;
    if (rows.length === 0) return null;
    if (activeRowKey && rows.some((r) => r.key === activeRowKey)) return activeRowKey;
    if ($selectedWorktreeId) {
      const wt = rows.find((r) => r.key === `wt:${$selectedWorktreeId}`);
      if (wt) return wt.key;
    }
    if ($selectedProjectId) {
      const pr = rows.find((r) => r.key === `proj:${$selectedProjectId}`);
      if (pr) return pr.key;
    }
    return rows[0].key;
  }

  // Move the roving target to `key` and put DOM focus on its row, scrolling it
  // just into view (no new visual language — the row's own :focus-visible / .sel
  // styles carry the cue).
  function focusRow(key: string | null) {
    if (!key) return;
    activeRowKey = key;
    const el = rowEls.get(key);
    el?.focus();
    el?.scrollIntoView({ block: "nearest" });
  }

  // Forward focus from the pane root (LeftRail's [data-pane="tree"], focused by
  // the Cmd+Shift+E command) onto the roving row, so the shortcut lands the user
  // on a navigable row rather than the inert container.
  export function focusActiveRow() {
    focusRow(activeKey());
  }

  function moveRoving(delta: 1 | -1) {
    const rows = visibleRows;
    const cur = activeKey();
    const i = rows.findIndex((r) => r.key === cur);
    const next = i < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, i + delta));
    focusRow(rows[next]?.key ?? null);
  }

  // ←/→ on a project expand/collapse; on a worktree, ← jumps to its parent
  // project (→ is a no-op, worktrees have no children). Mirrors a file-tree's
  // arrow semantics.
  function rowRight(row: Row) {
    if (row.kind !== "project") return;
    if (row.project.kind === "git-backed" && collapsed[row.project.id]) {
      toggleExpand(row.project);
    } else {
      moveRoving(1); // already expanded → descend to first child
    }
  }

  function rowLeft(row: Row) {
    if (row.kind === "worktree") {
      focusRow(`proj:${row.worktree.project_id}`);
      return;
    }
    if (row.project.kind === "git-backed" && !collapsed[row.project.id]) {
      toggleExpand(row.project);
    }
  }

  function selectRow(row: Row) {
    if (row.kind === "project") selectProject(row.project);
    else selectWorktree(row.worktree);
  }

  // Component-local keydown for the bare tree keys. The global dispatcher matches
  // these same combos but registers NO handler for them, so it never
  // preventDefaults — handling here is the sole route (see keymap.ts / +layout).
  // We resolve the binding id via the SAME matchBinding the dispatcher uses, so
  // the keymap table stays the single source of truth (a future rebinding/Settings
  // panel changes both at once). matchBinding pane-gates bare keys to "tree" and
  // requires modifiers to match EXACTLY, so Cmd+ArrowDown / Cmd+N never resolve to
  // a bare tree id here — the modifier-combo ids (e.g. tree.newWorktree) have no
  // case below and fall through (no preventDefault) to the global dispatcher.
  function onRowKey(event: KeyboardEvent, row: Row) {
    const binding = matchBinding(event, desktopPlatform, "tree");
    switch (binding?.id) {
      case "tree.down":
        event.preventDefault();
        moveRoving(1);
        break;
      case "tree.up":
        event.preventDefault();
        moveRoving(-1);
        break;
      case "tree.expand":
        event.preventDefault();
        rowRight(row);
        break;
      case "tree.collapse":
        event.preventDefault();
        rowLeft(row);
        break;
      case "tree.select":
      case "tree.select.space":
        event.preventDefault();
        selectRow(row);
        break;
    }
  }
</script>

<!-- focusin sets the pane so clicking into the tree (not just the Cmd+Shift+E
     command) routes bare-key bindings here. Forwards to a real row when the
     focus landed on the inert container/root rather than a row. -->
<div class="tree" onfocusin={() => focusedPane.set("tree")}>
  <!-- Flat project tree: scope/host parent rows are gone. We still iterate
       $daemonScopesOrdered as the OUTER loop purely to keep project ORDER stable
       (Local first, then each host's projects together, hosts alpha) and to carry
       per-project REMOTE context (cloud + host suffix, host actions, liveness)
       from the owning scope — but each scope renders its projects directly, with
       no header and no expand/collapse gate. -->
  {#if $projects.length === 0}
    <p class="empty-copy">No projects yet. Add a local repo or folder to begin.</p>
  {/if}

  {#each $daemonScopesOrdered as scope (scope.id)}
    {@const scopeProjects = projectsForScope(scope.id)}
    {@const isRemote = scope.kind === "ssh-host"}
    {@const sshHost = isRemote ? $sshHosts.find((h) => h.id === scope.id) : null}
    {@const host = sshHost?.target}
    <!-- A scope is STALE (greyed, daemon actions disabled) whenever it is not
         LIVE (`running`) — issue #32. Local mirrors the local daemon, so a healthy
         Local reads live and nothing changes there. A remote project's row greys
         and its daemon-backed context-menu items disable while stale; the host
         actions (Retry, Remove) stay enabled (they're GUI-local). -->
    {@const scopeLive = $liveScopes.has(scope.id)}
    {@const scopeDown = scope.status === "unreachable" || scope.status === "failed"}
    {#each scopeProjects as project (project.id)}
        {@const rollup = $agentActRollupByProject[project.id]}
        {@const expanded = isExpanded(project)}
        {@const isGit = project.kind === "git-backed"}
        <div class="proj" class:stale={!scopeLive}>
      <ContextMenu.Root>
        <ContextMenu.Trigger>
          {#snippet child({ props })}
            <div
              {...props}
              use:registerRow={`proj:${project.id}`}
              class="row"
              class:sel={project.id === $selectedProjectId && $selectedWorktreeId === null}
              role="button"
              tabindex={activeKey() === `proj:${project.id}` ? 0 : -1}
              onclick={() => {
                activeRowKey = `proj:${project.id}`;
                selectProject(project);
              }}
              onfocus={() => (activeRowKey = `proj:${project.id}`)}
              onkeydown={(e) => onRowKey(e, { key: `proj:${project.id}`, kind: "project", project })}
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

              {#if isRemote && host}
                <!-- Remote project marker (flat tree, no host parent row): a quiet
                     cloud + the dim OpenSSH target, sitting immediately right of
                     the name. Local projects render nothing here, so they look
                     identical to before. -->
                <span class="rhost" title={`Remote on ${host}`}>
                  <Cloud class="rhost-ic icon" />
                  <span class="rhost-target">{host}</span>
                </span>
              {/if}

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

                {#if isGit && scopeLive}
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
              <!-- Daemon-backed: disabled while the owning scope is stale (issue
                   #32) — a worktree can't be created on an unreachable host. -->
              <ContextMenu.Item class="mi" disabled={!scopeLive} onSelect={() => createWorktreeFor.set(project)}>
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
            <!-- Daemon-backed: removing a project on a host is a remote op, so it
                 is disabled while the scope is stale (issue #32). -->
            <ContextMenu.Item class="mi danger" disabled={!scopeLive} onSelect={() => removeProjectTarget.set(project)}>
              <Trash2 class="mi-ico icon" />
              Remove project…
            </ContextMenu.Item>
            {#if isRemote && sshHost}
              <!-- Host actions live on the project context menu now that there is
                   no host parent row. Retry (only when the host is down) and
                   Remove host are GUI-local, so they stay enabled while stale.
                   "Add project" is deliberately NOT here — that lives on the
                   global "+" in LeftRail. -->
              <ContextMenu.Separator class="m-sep" />
              {#if scopeDown}
                <ContextMenu.Item class="mi" onSelect={() => void retrySshHost(sshHost.target)}>
                  <RotateCw class="mi-ico icon" />
                  Retry {sshHost.target}
                </ContextMenu.Item>
              {/if}
              <ContextMenu.Item class="mi danger" onSelect={() => removeSshHostTarget.set(sshHost)}>
                <Trash2 class="mi-ico icon" />
                Remove host…
              </ContextMenu.Item>
            {/if}
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
                      use:registerRow={`wt:${worktree.id}`}
                      class="wrow"
                      class:sel={isActive}
                      role="button"
                      tabindex={activeKey() === `wt:${worktree.id}` ? 0 : -1}
                      title={KIND_TITLE[kind]}
                      onclick={() => {
                        activeRowKey = `wt:${worktree.id}`;
                        selectWorktree(worktree);
                      }}
                      onfocus={() => (activeRowKey = `wt:${worktree.id}`)}
                      onkeydown={(e) => onRowKey(e, { key: `wt:${worktree.id}`, kind: "worktree", worktree })}
                    >
                      <div class="l1">
                        <GitBranch class="branchic icon" />
                        <span class="name">{worktree.branch}</span>
                        {#if kind === "main"}<span class="mainsuf">main</span>{:else}<span class="kindcue" title={KIND_TITLE[kind]}>{KIND_CUE[kind]}</span>{/if}
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
                          {@const kind = sessionTabKind(a.agent)}
                          {@const Mark = TAB_MARK[kind]}
                          <span class="h {kind}">
                            <Mark class="icon" />
                          </span>
                        {/each}
                      </div>
                    </div>
                  {/snippet}
                </ContextMenu.Trigger>
                <ContextMenu.Portal>
                  <ContextMenu.Content class="menu">
                    <!-- Daemon-backed: launching a session spawns a PTY on the
                         owning daemon, so these are disabled while the scope is
                         stale (issue #32). The OS-path items (reveal/editor/copy)
                         below stay enabled. -->
                    <ContextMenu.Item class="mi" disabled={!scopeLive} onSelect={() => launch(worktree, null, "shell")}>
                      <TerminalIcon class="mi-ico icon" />
                      Open shell session<span class="mi-k">{shellShortcutLabel}</span>
                    </ContextMenu.Item>
                    {#each LAUNCHABLE_AGENTS as a (a.kind)}
                      {@const Mark = a.icon}
                      <ContextMenu.Item class="mi" disabled={!scopeLive} onSelect={() => launch(worktree, a.launchArgv, a.kind)}>
                        <Mark class="mi-ico icon" />
                        Launch {a.title}
                      </ContextMenu.Item>
                    {/each}
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
                      <!-- Daemon-backed remote op: disabled while the scope is
                           stale (issue #32). -->
                      <ContextMenu.Item class="mi danger" disabled={!scopeLive} onSelect={() => removeWorktreeTarget.set(worktree)}>
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

  /* ---- stale scope subtree (issue #32, ADR 0014) ----
     When an SSH Host is unreachable/failed (or mid-reconnect), its last known
     tree stays visible but greys as stale UI and its daemon-backed actions are
     disabled. Opacity is multiplicative through a stacking context (a child can
     never be MORE opaque than its parent), so to keep attention readable we do
     NOT dim the whole `.proj` container — we ease back only the quiet,
     non-attention parts (names, icons, diffstat, facepile). The attention rollup
     pill and the AWAITING/ERROR state word are deliberately left untouched at
     full oxide so a stale host can still page the user (attention beats stale,
     ADR 0014). Selection (.sel) keeps its own iris styling regardless. */
  .proj.stale .pname,
  .proj.stale .pkind,
  .proj.stale .rhost-target,
  .proj.stale .name,
  .proj.stale .kindcue,
  .proj.stale .mainsuf,
  .proj.stale .diffn,
  .proj.stale .prchip,
  .proj.stale .pile,
  .proj.stale :global(.folder),
  .proj.stale :global(.rhost-ic),
  .proj.stale :global(.branchic),
  .proj.stale .tw {
    opacity: 0.5;
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
  /* `:focus`, not `:focus-visible`: roving moves focus PROGRAMMATICALLY
     (focusRow's el.focus()), and WKWebView does not reliably match
     :focus-visible for script focus on tabindex=-1 rows — the ring silently
     never rendered while arrowing. Rows only receive focus via keyboard
     roving, and a clicked row becomes .sel with the identical ring, so
     plain :focus adds no click noise. */
  .row:focus {
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
    /* flex:0 1 auto (not 1) so the name no longer eats all the row's free space:
       the remote cloud+host badge can sit immediately to its right while the
       `.trailing` cluster's margin-left:auto still pins the kind label / rollup /
       quick-add to the far right. Local rows render nothing between the name and
       the auto-margined trailing, so they are visually unchanged. */
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Remote project marker: a quiet dim cloud + OpenSSH target right after the
     project name (the flat tree's replacement for the old host parent row). */
  .rhost {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    min-width: 0;
  }
  .rhost :global(.rhost-ic) {
    width: 12px;
    height: 12px;
    flex: 0 0 12px;
    color: var(--ink-3);
  }
  .rhost-target {
    font-family: var(--mono);
    font-size: 0.625rem;
    font-weight: 500;
    color: var(--ink-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
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
  /* `:focus` for the same WKWebView script-focus reason as `.row:focus`. */
  .wrow:focus {
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
  /* faint suffixes mark worktree ownership without restoring the old dot column */
  .kindcue,
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
