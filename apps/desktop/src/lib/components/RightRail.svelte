<script lang="ts">
  // Right rail — Changes (Paper Terminal shell). Header diffstat + branch/PR
  // context, then ONE state-derived split button (the primary always does the
  // next meaningful step; its caret opens the full action menu), then Staged /
  // Changes file groups with inline stage toggles. Clicking a file row opens its
  // diff. This is the restyle of the long-standing smart-action state machine —
  // the ladder below mirrors that logic exactly, it is not a rewrite.
  import { tick } from "svelte";
  import { DropdownMenu } from "bits-ui";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import GitBranch from "~icons/lucide/git-branch";
  import GitPullRequest from "~icons/lucide/git-pull-request";
  import GitCommitHorizontal from "~icons/lucide/git-commit-horizontal";
  import ArrowUp from "~icons/lucide/arrow-up";
  import ArrowDown from "~icons/lucide/arrow-down";
  import ArrowUpFromLine from "~icons/lucide/arrow-up-from-line";
  import ArrowUpRight from "~icons/lucide/arrow-up-right";
  import Plus from "~icons/lucide/plus";
  import ChevronDown from "~icons/lucide/chevron-down";
  import RefreshCw from "~icons/lucide/refresh-cw";
  import {
    cancelJob,
    cancellableJobForSelectedWorktree,
    commit,
    commitLog,
    defaultBase,
    activeDiffPath,
    diffStaged,
    discardAllFiles,
    discardFile,
    generateCommitDraft,
    fetchRemote,
    gitBusy,
    gitStatus,
    gitWorktreeId,
    loadGitStatus,
    loadPrStatus,
    openPrInfo,
    prInfo,
    pull,
    push,
    setFileStaged,
    setFilesStaged,
    viewAllChanges,
    viewDiff,
  } from "../daemon";
  import { currentDesktopPlatform, shortcutKeys, shortcutLabel } from "../desktopPlatform";
  import { focusWithoutScroll } from "../focusWithoutScroll";
  import { fileIconUrl } from "../file-icons";
  import { focusedPane, matchBinding } from "../keymap";
  import { autoCommitPush, railView } from "../settings";
  import { commitOpen, createPrOpen } from "../overlays";
  import { STATUS_GLYPH, statusGlyphClass } from "../types";
  import CommitDialog from "./CommitDialog.svelte";
  import CreatePrDialog from "./CreatePrDialog.svelte";
  import HistoryList from "./HistoryList.svelte";
  import toast from "svelte-french-toast";

  // The Paper Terminal header drops the hide/collapse toggle (the rail is part
  // of the fixed 3-pane grid); rail visibility is driven from the layout via the
  // keymap chords and the command palette. `collapsed` still drives the opacity
  // fade when the layout hides the rail.
  let {
    collapsed = false,
  }: {
    collapsed?: boolean;
  } = $props();

  // Commit-shortcut hints. The handler (CommitDialog) is platform-aware via
  // isShortcutModifier, so the hints must agree: ⌘ on macOS, Ctrl elsewhere.
  const platform = currentDesktopPlatform();
  const commitKeys = shortcutKeys(platform, "↵");
  const commitHint = shortcutLabel(platform, "↵");

  const files = $derived($gitStatus?.files ?? []);
  const staged = $derived(files.filter((f) => f.staged));
  const unstaged = $derived(files.filter((f) => !f.staged));
  const ahead = $derived($gitStatus?.ahead ?? 0);
  const behind = $derived($gitStatus?.behind ?? 0);
  const additions = $derived($gitStatus?.additions ?? 0);
  const deletions = $derived($gitStatus?.deletions ?? 0);
  const isDefaultBranch = $derived(Boolean($defaultBase && $gitStatus?.branch === $defaultBase));

  const cancellableJob = $derived($cancellableJobForSelectedWorktree);

  let autoRunning = $state(false);

  // ---- roving keyboard focus (slice 4) ------------------------------------
  // The staged + unstaged groups are navigated as ONE list in visual order
  // (staged first, then changes) — what the user sees top-to-bottom. The active
  // row is tracked by file PATH (not index) so it survives a status refresh that
  // reorders/regroups rows (e.g. staging moves a file between groups); after the
  // lists update we re-anchor to the same path, clamping to the nearest index if
  // that path is gone. The row keeps the EXISTING `.active` selection visual —
  // no new focus language; we just reuse the same treatment the diff selection
  // already paints. Bare keys are handled here (component-local) rather than in
  // the layout dispatcher: DOM focus is inside this pane, the dispatcher does not
  // preventDefault unwired git ids, and the keymap entries (git.up/down/stage/…)
  // exist purely as the documentation/Settings source for these same keys.
  const rovingFiles = $derived([...staged, ...unstaged]);
  // A partially-staged file appears as TWO rows (staged + unstaged) sharing the
  // same path, so the active row is keyed by a composite of staged-side + path,
  // not path alone. Without this the lookups would always resolve the staged row
  // and the unstaged copy could never be reached, toggled, opened, or discarded.
  let activeKey = $state<string | null>(null);
  let railEl = $state<HTMLElement | null>(null);

  // The composite key for a roving row: `s:` (staged) or `w:` (working tree)
  // prefix + the path. Non-partially-staged files have exactly one row, so their
  // key is unique by construction — behavior is identical to keying by path.
  function rowKey(file: { path: string; staged: boolean }): string {
    return `${file.staged ? "s" : "w"}:${file.path}`;
  }

  // Re-anchor the active row after the list changes. If the tracked row is gone
  // (committed, discarded, or staged-away), clamp to the nearest surviving index
  // so focus lands on an adjacent row instead of vanishing.
  let lastFiles: typeof rovingFiles = [];
  $effect(() => {
    const list = rovingFiles;
    if (list.length === 0) {
      activeKey = null;
    } else if (activeKey === null || !list.some((f) => rowKey(f) === activeKey)) {
      const prevIdx = lastFiles.findIndex((f) => rowKey(f) === activeKey);
      const clamped = prevIdx < 0 ? 0 : Math.min(prevIdx, list.length - 1);
      activeKey = rowKey(list[clamped]);
    }
    lastFiles = list;
  });

  // Move the active row by `delta` rows within the flattened list and scroll it
  // into view. No wrap-around — clamps at the ends.
  function moveActive(delta: number) {
    const list = rovingFiles;
    if (list.length === 0) return;
    const cur = list.findIndex((f) => rowKey(f) === activeKey);
    const next = cur < 0 ? 0 : Math.min(Math.max(cur + delta, 0), list.length - 1);
    const target = list[next];
    activeKey = rowKey(target);
    void tick().then(() => {
      // Move DOM focus to the new row (not just scroll it into view) so
      // :focus-visible follows .roving — matching ProjectTree's focusRow().
      const row = railEl?.querySelector<HTMLElement>(
        `.frow[data-path="${cssEscape(target.path)}"][data-staged="${target.staged}"]`,
      );
      row?.focus();
      row?.scrollIntoView({ block: "nearest" });
    });
  }

  // Stage/unstage the active file — the same action the row's checkbox runs.
  function toggleActiveStaged() {
    const file = rovingFiles.find((f) => rowKey(f) === activeKey);
    if (!file) return;
    void setFileStaged(file.path, !file.staged).catch(() => {});
  }

  // Open the active file's diff — the same action a row click runs (the staged
  // copy diffs staged, the unstaged copy diffs the working tree).
  function openActiveDiff() {
    const file = rovingFiles.find((f) => rowKey(f) === activeKey);
    if (!file) return;
    void viewDiff(file.path, true, file.staged);
  }

  // Discard the active file through the existing confirm flow.
  function discardActive() {
    const file = rovingFiles.find((f) => rowKey(f) === activeKey);
    if (file) confirmDiscardFile(file.path);
  }

  // Pane-local key handling for the bare git keys advertised in the footer. The
  // layout dispatcher leaves these unwired (no preventDefault), so handling them
  // here is the sole route — no double-fire. Cmd+Enter (commit) is NOT handled
  // here: it is a modifier combo wired in the layout dispatcher, which gates it
  // on the git pane and suppresses it while the commit dialog is open.
  function onRailKeydown(event: KeyboardEvent) {
    // Never hijack keys typed into an editable element (none today, but the
    // checkbox/discard spans are role="button" — keep the guard for safety). This
    // is a DOM-target gate, so it stays here rather than in matchBinding (which is
    // DOM-free — see keymap.ts).
    if (event.target instanceof HTMLElement) {
      const tag = event.target.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || event.target.isContentEditable) {
        return;
      }
    }
    // Resolve the binding id via the SAME matchBinding the layout dispatcher uses,
    // so the keymap table stays the single source of truth. We pass pane "git" so
    // the bare git keys (arrows/Space/Enter/Backspace/R) match; matchBinding
    // requires modifiers to match EXACTLY, so e.g. Cmd+Enter resolves to
    // git.commit (no case here → falls through to the dispatcher) and Cmd+R / any
    // modifier+arrow resolves to nothing. R is git.refresh's case-insensitive key,
    // but its combo forbids Shift, so Shift+R no longer refreshes (Caps-Lock R,
    // which carries no shiftKey, still does).
    const binding = matchBinding(event, platform, "git");
    // ←/→ switch the rail view in BOTH views (git pane focus is shared). Handled
    // before the per-view branch so it works identically from Changes or History.
    // Switching the view UNMOUNTS the focused row (the other list renders), so
    // DOM focus would fall back to <body> and the next ←/→ would dead-end. We
    // restore focus into the rail after the flip (tick() lets the new list mount)
    // so arrows keep round-tripping; on an empty target list focusRailRows falls
    // back to the aside itself, which still carries this handler.
    if (binding?.id === "git.viewPrev" || binding?.id === "git.viewNext") {
      if (!$gitWorktreeId) return;
      event.preventDefault();
      toggleRailView();
      void tick().then(() => focusRailRows());
      return;
    }

    if (historyView) {
      // HISTORY: ↑/↓ rove commits, Enter opens the commit tab. Space/Backspace
      // carry Changes-only meanings (stage / discard) — they are INERT here, not
      // errors. R still refreshes (handled in the shared branch below) — but for
      // History the log rides the status backbone, so it's a no-op-friendly
      // refetch of status; we leave the existing handleRefresh as-is.
      switch (binding?.id) {
        case "git.down":
          event.preventDefault();
          moveActiveCommit(1);
          return;
        case "git.up":
          event.preventDefault();
          moveActiveCommit(-1);
          return;
        case "git.openDiff":
          event.preventDefault();
          openActiveCommit();
          return;
        case "git.stage":
        case "git.discard":
          // Inert in HISTORY (no stage/discard target). Swallow so the page
          // doesn't scroll / navigate, but do nothing else.
          event.preventDefault();
          return;
        case "git.refresh":
          event.preventDefault();
          void handleRefresh();
          return;
      }
      return;
    }

    switch (binding?.id) {
      case "git.down":
        event.preventDefault();
        moveActive(1);
        break;
      case "git.up":
        event.preventDefault();
        moveActive(-1);
        break;
      case "git.stage":
        // preventDefault stops the space from scrolling the file list / page.
        event.preventDefault();
        toggleActiveStaged();
        break;
      case "git.openDiff":
        // Plain Enter only — Cmd+Enter (commit) is the layout dispatcher's
        // (it resolves to git.commit, which has no case here).
        event.preventDefault();
        openActiveDiff();
        break;
      case "git.discard":
        event.preventDefault();
        discardActive();
        break;
      case "git.refresh":
        event.preventDefault();
        void handleRefresh();
        break;
    }
  }

  // Forward focus to the active/first row of whichever view is rendered so the
  // arrow keys work immediately. Shared by onRailFocus (rail gains focus, e.g.
  // via Cmd+Shift+G) and the ←/→ view switch (which unmounts the focused row).
  // When the target list is EMPTY (clean tree / empty log) there is no row to
  // focus, so we fall back to the aside itself — it has tabindex="-1" and carries
  // onRailKeydown, so ←/→ still round-trips out of an empty view. Assumes the new
  // list is already mounted; callers tick() first when they just changed views.
  // Forward focus WITHOUT scrolling: this helper only restores DOM focus into the
  // rail (so arrows work) — it never intends to move the viewport. A roving row is
  // often clipped at the scroll edge, and a plain .focus() would scroll it fully
  // into view; under a fresh mouse focus that lands here (target === railEl), the
  // native scroll yanks the list back to the active row and strands the in-flight
  // click. The keyboard/scroll paths that DO want to reveal a row call
  // scrollIntoView explicitly (moveActive / moveActiveCommit → scrollActiveIntoView).
  function focusRailRows() {
    if (historyView) {
      // HISTORY: focus the roving (or first) commit row so arrows work at once.
      // The cursor is a sha, so target [data-sha]; fall back to the first row when
      // nothing is roving (or the roving sha isn't currently rendered).
      const row =
        (activeCommitSha
          ? railEl?.querySelector<HTMLElement>(`.crow[data-sha="${cssEscape(activeCommitSha)}"]`)
          : null) ?? railEl?.querySelector<HTMLElement>(".crow");
      (row ?? railEl)?.focus({ preventScroll: true });
      return;
    }
    const active = rovingFiles.find((f) => rowKey(f) === activeKey);
    const row =
      (active
        ? railEl?.querySelector<HTMLElement>(
            `.frow[data-path="${cssEscape(active.path)}"][data-staged="${active.staged}"]`,
          )
        : null) ?? railEl?.querySelector<HTMLElement>(".frow");
    (row ?? railEl)?.focus({ preventScroll: true });
  }

  // When the rail root itself receives focus (e.g. via the Cmd+Shift+G focus.git
  // command, which focuses [data-pane="git"]), forward focus to the active/first
  // file row so the arrow keys work immediately without a second keystroke.
  function onRailFocus(event: FocusEvent) {
    if (event.target !== railEl) return;
    void tick().then(() => focusRailRows());
  }

  // Minimal CSS.escape fallback — file paths can contain characters that break a
  // raw attribute selector (quotes, brackets). CSS.escape exists in the webview;
  // guard for the test/SSR environment just in case.
  function cssEscape(value: string): string {
    return typeof CSS !== "undefined" && CSS.escape ? CSS.escape(value) : value;
  }

  // Row clicks on the scrollable file list use the shared focusWithoutScroll
  // pointerdown helper — see ../focusWithoutScroll for why a clipped row's native
  // focus-into-view would otherwise strand the click.

  // ---- HISTORY view: toggle + roving over commit rows ---------------------
  // Git pane focus is SHARED with Changes; the rail-view toggle decides which
  // list is rendered. ←/→ flips the view; ↑/↓ roves commit rows (clamped, no
  // wrap — same as file rows); Enter opens the focused commit's tab. The roving
  // cursor is a plain index into the (immutable, append-only) commit list — a
  // simpler shape than the file list's composite key because commit rows never
  // regroup or split. It resets to -1 on view switch / worktree change / log
  // refetch so a stale index never points past the rows.
  // HISTORY is only an available view for a git worktree (the toggle is hidden
  // otherwise). Gating the rendered view on $gitWorktreeId too keeps the header
  // and body consistent: with no git worktree the header shows the plain CHANGES
  // label and the body shows the Changes empty-state, even if railView still
  // reads "history" from a previous git selection this session.
  const historyView = $derived($railView === "history" && Boolean($gitWorktreeId));
  const commits = $derived($commitLog.commits);
  // The roving (keyboard-focused) commit is tracked by SHA, not array index, so it
  // survives a pagination APPEND (earlier rows keep their identity) and only drops
  // when the selected commit genuinely leaves the rendered set. A bare index would
  // desync after appends/resets and land the indicator on the wrong row (or -1);
  // the sha is matched against the live array whenever an index is needed.
  let activeCommitSha = $state<string | null>(null);
  let historyList = $state<HistoryList | null>(null);

  // Reset the roving cursor only when the rendered log's WORKTREE changes (or we
  // leave HISTORY) — not on a HEAD-change refetch or a pagination append. The
  // selection is sha-keyed, so it self-heals across those: if the previously
  // selected sha is still present after a refetch it stays roving, and if it's
  // gone the cursor simply reads as unset until the user moves/clicks again, rather
  // than being force-cleared (which previously clobbered a valid selection whenever
  // page one replaced the array or HEAD moved).
  let lastWorktreeKey = "";
  $effect(() => {
    const key = $commitLog.worktreeId ?? "";
    if (!historyView) {
      activeCommitSha = null;
      lastWorktreeKey = "";
    } else if (key !== lastWorktreeKey) {
      activeCommitSha = null;
      lastWorktreeKey = key;
    }
  });

  // Switch CHANGES ⇄ HISTORY. Only meaningful when the toggle is present (a git
  // worktree is selected); a no-op otherwise so a stray ←/→ in a non-git rail
  // does nothing.
  function setRailView(view: "changes" | "history") {
    if (!$gitWorktreeId) return;
    railView.set(view);
  }
  function toggleRailView() {
    setRailView(historyView ? "changes" : "history");
  }

  // Move the roving commit by `delta` (clamped, no wrap) and scroll/focus it via
  // the HistoryList helper — mirrors the file list's moveActive. The cursor is a
  // sha, so we resolve it to the current index, step, then store the neighbor's
  // sha. An unset/stale sha starts from the top (↓) or bottom (↑) edge.
  function moveActiveCommit(delta: number) {
    if (commits.length === 0) return;
    const curIdx = activeCommitSha ? commits.findIndex((c) => c.id === activeCommitSha) : -1;
    const from = curIdx < 0 ? (delta > 0 ? -1 : 0) : curIdx;
    const next = Math.min(Math.max(from + delta, 0), commits.length - 1);
    activeCommitSha = commits[next]?.id ?? null;
    historyList?.scrollActiveIntoView();
  }

  // Open the focused commit's tab — routes through the SAME path as a row click
  // (HistoryList.openCommit), so the keyboard Enter both opens the tab AND sets the
  // sha-based roving selection, exactly as clicking does. Falls back to the first
  // row when nothing is roving yet so Enter on a fresh History view still opens.
  function openActiveCommit() {
    const commit = commits.find((c) => c.id === activeCommitSha) ?? commits[0];
    if (commit) historyList?.openCommit(commit);
  }

  // ---- smart actions ------------------------------------------------------
  // One state machine drives both the split button's primary action and the
  // enabled/disabled state of every item in its dropdown. The primary always
  // does the *next* meaningful step; the dropdown exposes each step directly,
  // greyed out (with a reason) when it doesn't apply to the current status.
  const pr = $derived($prInfo);
  const openPr = $derived($openPrInfo);
  const hasChanges = $derived(files.length > 0);
  const onDefault = $derived(isDefaultBranch);
  const busy = $derived($gitBusy || autoRunning);

  function openCommit() {
    commitOpen.set(true);
  }
  function openCreatePr() {
    if (busy || !$gitWorktreeId) return;
    createPrOpen.set(true);
  }
  async function openExistingPr() {
    if (openPr) await openUrl(openPr.url);
  }
  async function openDisplayedPr() {
    if (pr) await openUrl(pr.url);
  }

  // The headline action: the first applicable step in commit → pull → push →
  // create-PR → open-PR order. `null` run means nothing to do (e.g. clean +
  // synced on the default branch) and the button renders disabled.
  //
  // `key` names which menu row is currently primary (marked .is-primary); the
  // icon component is chosen per action for the split-button leading glyph.
  type PrimaryKey = "commitpush" | "commit" | "pull" | "push" | "createpr" | "openpr" | "none";
  type PrimaryAction = {
    label: string;
    run: (() => void) | null;
    mutates: boolean;
    key: PrimaryKey;
  };
  const primary = $derived<PrimaryAction>(
    hasChanges
      ? $autoCommitPush
        ? { label: "Commit & Push", run: () => void handleAutoCommitPush(), mutates: true, key: "commitpush" }
        : { label: "Commit…", run: openCommit, mutates: true, key: "commit" }
      : behind > 0
        ? { label: `Pull ↓${behind}`, run: () => void handleManualPull(), mutates: true, key: "pull" }
        : ahead > 0
          ? { label: `Push ↑${ahead}`, run: () => void handleManualPush(), mutates: true, key: "push" }
          : !onDefault && $gitWorktreeId && !openPr
            ? { label: "Create PR", run: openCreatePr, mutates: true, key: "createpr" }
            : openPr
              ? { label: `Open PR #${openPr.number}`, run: () => void openExistingPr(), mutates: false, key: "openpr" }
              : { label: "Up to date", run: null, mutates: false, key: "none" },
  );

  // Per-step availability + the reason shown when a step is unavailable, so the
  // dropdown reads as a checklist of what this worktree can do right now.
  const pushReason = $derived(ahead > 0 ? "" : "Nothing to push");
  const pullReason = $derived(behind > 0 ? "" : "Up to date with remote");
  const commitReason = $derived(busy ? "Git operation in progress" : hasChanges ? "" : "No changes to commit");
  const createPrReason = $derived(
    busy ? "Git operation in progress" : onDefault ? "On the default branch" : !$gitWorktreeId ? "No worktree selected" : "",
  );

  // The "why this action is primary" hint segments, derived from the same state
  // the ladder uses: staged count · ahead/behind · PR status. Quiet, tabular.
  const whyParts = $derived(
    [
      staged.length > 0 ? `${staged.length} staged` : "",
      ahead > 0 ? `↑${ahead}` : "",
      behind > 0 ? `↓${behind}` : "",
      pr ? `PR #${pr.number} ${pr.draft ? "draft" : pr.state.toLowerCase()}` : "",
    ].filter(Boolean),
  );

  function shortError(err: unknown): string {
    const msg = err instanceof Error ? err.message : String(err);
    const first = msg.split("\n")[0].trim();
    return first.length > 80 ? first.slice(0, 77) + "…" : first;
  }

  // Split a path into a dimmed directory part and an emphasized filename.
  function splitPath(path: string): { dir: string; name: string } {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? { dir: "", name: path } : { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
  }

  async function handleAutoCommitPush() {
    const worktreeId = $gitWorktreeId;
    if ($gitBusy || autoRunning || !worktreeId) return;
    const pathsToStage = unstaged.map((file) => file.path);
    autoRunning = true;
    const id = toast.loading("Staging files…");
    try {
      if (pathsToStage.length > 0) {
        await setFilesStaged(pathsToStage, true, worktreeId);
      }
      toast.loading("Generating commit message…", { id });
      const draft = await generateCommitDraft(worktreeId);
      toast.loading("Committing…", { id });
      await commit(draft.subject, draft.body, worktreeId);
      toast.loading("Pushing…", { id });
      await push(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      void loadPrStatus(worktreeId);
      toast.success(draft.subject, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    } finally {
      autoRunning = false;
    }
  }

  async function handleManualPush() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    const count = ahead;
    const id = toast.loading("Pushing…");
    try {
      await push(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      void loadPrStatus(worktreeId);
      toast.success(`Pushed ↑${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  async function handleManualPull() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    const count = behind;
    const id = toast.loading("Pulling…");
    try {
      await pull(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      toast.success(`Pulled ↓${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  async function handleRefresh() {
    if ($gitBusy || !$gitWorktreeId) return;
    const worktreeId = $gitWorktreeId;
    const id = toast.loading("Fetching…");
    try {
      await fetchRemote(worktreeId);
      await loadGitStatus(worktreeId);
      void loadPrStatus(worktreeId);
      toast.success("Fetched", { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  function confirmDiscardAll() {
    if (files.length === 0 || $gitBusy) return;
    // A partially-staged file appears as two rows (staged + unstaged); count
    // distinct paths so the prompt matches what discard actually touches.
    const count = new Set(files.map((f) => f.path)).size;
    if (window.confirm(`Discard all ${count} changed file${count === 1 ? "" : "s"}?`)) {
      void discardAllFiles();
    }
  }

  function confirmDiscardFile(path: string) {
    if ($gitBusy) return;
    if (window.confirm(`Discard changes to ${path}?`)) {
      void discardFile(path);
    }
  }
</script>

<!-- data-pane + tabindex let the keymap's focus.git command move DOM focus into
     this rail (the dispatcher queries [data-pane="git"] and focuses it). focusin
     marks the git pane as focused so the layout dispatcher gates its modifier
     combos (Cmd+Enter commit) correctly; onfocus on the root forwards focus to a
     file row so arrows work immediately. Bare git keys are handled locally
     (onRailKeydown) — see the roving-focus note in the script. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside
  bind:this={railEl}
  class="rail-right"
  class:collapsed
  data-pane="git"
  tabindex="-1"
  onfocusin={() => focusedPane.set("git")}
  onfocus={onRailFocus}
  onkeydown={onRailKeydown}
>
  <!-- Header: 38px baseline grid. CHANGES label + net diffstat. A quiet refresh
       icon sits next to the net stat (the old header refresh/hide buttons are
       gone; hide is dropped, refresh lives here + in the action menu's reach). -->
  <div class="changes-head">
    <div class="title">
      {#if $gitWorktreeId}
        <!-- CHANGES | HISTORY view toggle. Shown only for a git worktree; for a
             non-git / no-worktree rail the header keeps the plain CHANGES label
             (the toggle is absent, matching how git ops are gated). Mono-uppercase
             text buttons bound to the railView selection, with an iris-ink active
             state — the rail's own header language (no segmented widget exists in
             the chrome to reuse). -->
        <div class="view-toggle" role="tablist" aria-label="Right rail view">
          <button
            class="vtab"
            role="tab"
            aria-selected={!historyView}
            class:on={!historyView}
            onclick={() => setRailView("changes")}
          >Changes</button>
          <button
            class="vtab"
            role="tab"
            aria-selected={historyView}
            class:on={historyView}
            onclick={() => setRailView("history")}
          >History</button>
        </div>
      {:else}
        <h2>Changes</h2>
      {/if}
      <span class="head-right">
        {#if !historyView && $gitStatus}
          <span class="net">
            <span class="a">+{additions}</span> <span class="d">−{deletions}</span>
          </span>
        {/if}
        <button
          class="refresh"
          title="Fetch remote and refresh status"
          aria-label="Fetch remote and refresh status"
          disabled={!$gitWorktreeId || $gitBusy}
          onclick={() => void handleRefresh()}
        >
          <RefreshCw class="icon" />
        </button>
      </span>
    </div>
  </div>

  {#if historyView}
    <HistoryList bind:this={historyList} bind:activeSha={activeCommitSha} />
  {:else}
  {#if $gitStatus}
    <div class="changes-ctx">
      <div class="branchline">
        <GitBranch class="ic icon" />
        <span class="b" title={$gitStatus.branch}>{$gitStatus.branch}</span>
        {#if $defaultBase && $defaultBase !== $gitStatus.branch}
          <span class="from">from {$defaultBase}</span>
        {/if}
        {#if ahead > 0 || behind > 0}
          <span
            class="ahead"
            title="{ahead} ahead{behind > 0 ? `, ${behind} behind` : ''} of origin"
          >
            {#if ahead > 0}<span class="arr">↑</span>{ahead}{/if}{#if behind > 0}<span class="arr down">↓</span>{behind}{/if}
          </span>
        {/if}
      </div>

      {#if pr}
        <a
          class="pr {pr.draft ? 'draft' : pr.state.toLowerCase()}"
          href={pr.url}
          title="{pr.draft ? 'Draft' : pr.state} pull request #{pr.number} — open on GitHub"
          onclick={(e) => {
            e.preventDefault();
            void openDisplayedPr();
          }}
        >
          <GitPullRequest class="pric icon" />
          <span>PR</span><span class="num">#{pr.number}</span>
        </a>
      {/if}
    </div>

    <div class="actions">
      {#if cancellableJob}
        <button
          class="cancel"
          title="Cancel the running operation"
          onclick={() => void cancelJob(cancellableJob.id)}
        >
          Cancel
        </button>
      {:else}
        <div class="splitbtn" class:disabled={!primary.run}>
          <button
            class="split-main on-iris"
            disabled={!primary.run || (busy && primary.mutates)}
            onclick={() => primary.run?.()}
          >
            {#if primary.key === "commitpush"}
              <ArrowUpFromLine class="btnic icon" />
            {:else if primary.key === "commit"}
              <GitCommitHorizontal class="btnic icon" />
            {:else if primary.key === "push"}
              <ArrowUp class="btnic icon" />
            {:else if primary.key === "pull"}
              <ArrowDown class="btnic icon" />
            {:else if primary.key === "createpr"}
              <Plus class="btnic icon" />
            {:else if primary.key === "openpr"}
              <ArrowUpRight class="btnic icon" />
            {/if}
            {primary.label}
            {#if primary.key === "commit" || primary.key === "commitpush"}
              <span class="keys">
                {#each commitKeys as k (k)}<kbd>{k}</kbd>{/each}
              </span>
            {/if}
          </button>
          <DropdownMenu.Root>
            <DropdownMenu.Trigger>
              {#snippet child({ props })}
                <button
                  {...props}
                  class="split-caret"
                  aria-label="More git actions"
                  title="More git actions"
                >
                  <ChevronDown class="icon" />
                </button>
              {/snippet}
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="menu act-menu" align="end" side="bottom" sideOffset={6}>
                <DropdownMenu.Item
                  class="mi {primary.key === 'commit' ? 'is-primary' : ''}"
                  disabled={!hasChanges || busy}
                  title={commitReason}
                  onSelect={openCommit}
                >
                  <GitCommitHorizontal class="mi-ico icon" />
                  Commit…
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi {primary.key === 'commitpush' ? 'is-primary' : ''}"
                  disabled={!hasChanges || busy}
                  title={commitReason}
                  onSelect={() => void handleAutoCommitPush()}
                >
                  <ArrowUpFromLine class="mi-ico icon" />
                  Commit &amp; Push <span class="mi-k">{commitHint}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                <DropdownMenu.Item
                  class="mi {primary.key === 'push' ? 'is-primary' : ''}"
                  disabled={ahead === 0 || busy}
                  title={pushReason}
                  onSelect={() => void handleManualPush()}
                >
                  <ArrowUp class="mi-ico icon" />
                  Push <span class="mi-k">↑{ahead}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi {primary.key === 'pull' ? 'is-primary' : ''}"
                  disabled={behind === 0 || busy}
                  title={pullReason}
                  onSelect={() => void handleManualPull()}
                >
                  <ArrowDown class="mi-ico icon" />
                  Pull <span class="mi-k">↓{behind}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                {#if openPr}
                  <DropdownMenu.Item class="mi" onSelect={() => void openExistingPr()}>
                    <GitPullRequest class="mi-ico icon" />
                    Open PR #{openPr.number} <span class="mi-k">↗</span>
                  </DropdownMenu.Item>
                {:else}
                  <DropdownMenu.Item
                    class="mi {primary.key === 'createpr' ? 'is-primary' : ''}"
                    disabled={Boolean(createPrReason)}
                    title={createPrReason}
                    onSelect={openCreatePr}
                  >
                    <Plus class="mi-ico icon" />
                    Create PR…
                  </DropdownMenu.Item>
                {/if}
                <DropdownMenu.Separator class="m-sep" />
                <DropdownMenu.Item
                  class="mi toggle"
                  closeOnSelect={false}
                  onSelect={() => autoCommitPush.update((v) => !v)}
                >
                  <span class="check" class:on={$autoCommitPush} aria-hidden="true">✓</span>
                  auto-generate commit message
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>

        {#if whyParts.length > 0}
          <div class="why-primary">
            {#each whyParts as part, i (part)}
              {#if i > 0}<span class="sep" aria-hidden="true">·</span>{/if}
              <span>{part}</span>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="files">
    {#if !$gitWorktreeId}
      <div class="empty"><p>Select a git worktree to see its changes.</p></div>
    {:else if files.length === 0}
      <div class="empty"><p>Working tree clean.<br />Nothing to commit.</p></div>
    {:else}
      {#if staged.length > 0}
        <div class="fgroup">
          <h3>
            <button
              class="group-label"
              title="View all changes as one diff"
              aria-label="View all staged and unstaged changes as one diff"
              onclick={() => void viewAllChanges()}
            >Staged</button><span class="ct">{staged.length}</span><span class="hr"></span>
            <button
              class="all"
              onclick={() => void setFilesStaged(staged.map((f) => f.path), false).catch(() => {})}
            >unstage all</button>
          </h3>
          {#each staged as file (file.path)}
            {@const parts = splitPath(file.path)}
            <button class="frow" data-path={file.path} data-staged="true" class:active={$activeDiffPath === file.path && $diffStaged !== false} class:roving={activeKey === rowKey(file)} onpointerdown={focusWithoutScroll} onclick={() => { activeKey = rowKey(file); void viewDiff(file.path, true, true); }}>
              <span
                class="chk on"
                role="button"
                tabindex="-1"
                title="Unstage {file.path}"
                aria-label="Unstage {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  void setFileStaged(file.path, false).catch(() => {});
                }}
                onkeydown={() => {}}
              >✓</span>
              <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
              <span class="ftype" aria-hidden="true"><img src={fileIconUrl(file.path)} alt="" /></span>
              <span class="path">{#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<b>{parts.name}</b></span>
              <span class="fdiff">
                <span class="a">+{file.additions ?? 0}</span>
                <span class="d">−{file.deletions ?? 0}</span>
              </span>
              <span
                class="discard"
                role="button"
                tabindex="-1"
                title="Discard file"
                aria-label="Discard changes to {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  confirmDiscardFile(file.path);
                }}
                onkeydown={() => {}}
              >×</span>
            </button>
          {/each}
        </div>
      {/if}

      {#if unstaged.length > 0}
        <div class="fgroup">
          <h3>
            <button
              class="group-label"
              title="View all changes as one diff"
              aria-label="View all staged and unstaged changes as one diff"
              onclick={() => void viewAllChanges()}
            >Changes</button><span class="ct">{unstaged.length}</span><span class="hr"></span>
            <button
              class="all"
              onclick={() => void setFilesStaged(unstaged.map((f) => f.path), true).catch(() => {})}
            >stage all</button>
            <button class="all discard-all" disabled={$gitBusy} onclick={confirmDiscardAll}>discard</button>
          </h3>
          {#each unstaged as file (file.path)}
            {@const parts = splitPath(file.path)}
            <button class="frow" data-path={file.path} data-staged="false" class:active={$activeDiffPath === file.path && $diffStaged !== true} class:roving={activeKey === rowKey(file)} onpointerdown={focusWithoutScroll} onclick={() => { activeKey = rowKey(file); void viewDiff(file.path, true, false); }}>
              <span
                class="chk"
                role="button"
                tabindex="-1"
                title="Stage {file.path}"
                aria-label="Stage {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  void setFileStaged(file.path, true).catch(() => {});
                }}
                onkeydown={() => {}}
              ></span>
              <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
              <span class="ftype" aria-hidden="true"><img src={fileIconUrl(file.path)} alt="" /></span>
              <span class="path">{#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<b>{parts.name}</b></span>
              <span class="fdiff">
                <span class="a">+{file.additions ?? 0}</span>
                <span class="d">−{file.deletions ?? 0}</span>
              </span>
              <span
                class="discard"
                role="button"
                tabindex="-1"
                title="Discard file"
                aria-label="Discard changes to {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  confirmDiscardFile(file.path);
                }}
                onkeydown={() => {}}
              >×</span>
            </button>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
  {/if}

  <!-- Footer kbd legend swaps per view: Changes advertises stage/diff/commit,
       History advertises rove/open/view-switch. -->
  {#if historyView}
    <div class="rail-r-foot">
      <span><kbd>↑</kbd><kbd>↓</kbd> rove</span>
      <span><kbd>↵</kbd> open</span>
      <span><kbd>←</kbd><kbd>→</kbd> view</span>
    </div>
  {:else}
    <div class="rail-r-foot">
      <span><kbd>␣</kbd> stage</span>
      <span><kbd>↵</kbd> open diff</span>
      <span><span class="keys">{#each commitKeys as k (k)}<kbd>{k}</kbd>{/each}</span> commit</span>
    </div>
  {/if}

  <!-- Mounted once, triggerless: opened from the action menu (and the command
       palette) via the commitOpen / createPrOpen stores. -->
  <CommitDialog triggerless />
  <CreatePrDialog triggerless />
</aside>

<style>
  .rail-right {
    background: var(--paper-1);
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    transition: opacity 0.2s ease-out;
  }
  .rail-right.collapsed {
    opacity: 0;
    pointer-events: none;
  }

  /* Header — shares the 38px baseline grid with PROJECTS + the tab strip. */
  .changes-head {
    flex: 0 0 38px;
    height: 38px;
    padding: 0 16px;
    border-bottom: 1px solid var(--line);
  }
  .changes-head .title {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .changes-head h2 {
    font-size: 0.6875rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
  }
  /* CHANGES | HISTORY toggle: two mono-uppercase text buttons in the rail's own
     header language. The inactive view reads like the dimmed PROJECTS/CHANGES
     label; the active view lifts to iris-ink (the selection accent) so one view
     is unambiguously current. Square, no widget chrome. */
  .changes-head .view-toggle {
    display: inline-flex;
    align-items: center;
    gap: 12px;
  }
  .changes-head .vtab {
    font-family: var(--ui);
    font-size: 0.6875rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    font-weight: 700;
    color: var(--ink-3);
    background: transparent;
    border: 0;
    border-radius: 0;
    padding: 0;
    cursor: pointer;
    transition: color 0.15s ease-out;
  }
  .changes-head .vtab:hover {
    color: var(--ink-1);
  }
  .changes-head .vtab.on {
    color: var(--iris-ink);
  }
  .changes-head .vtab:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: 2px;
  }
  .changes-head .head-right {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .changes-head .net {
    font-family: var(--mono);
    font-size: var(--r0);
    font-variant-numeric: tabular-nums;
  }
  .changes-head .net .a {
    color: var(--diff-add);
    font-weight: 600;
  }
  .changes-head .net .d {
    color: var(--diff-del);
    font-weight: 600;
  }
  .changes-head .refresh {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--ink-3);
    cursor: pointer;
    transition: color 0.15s ease-out;
  }
  .changes-head .refresh :global(svg) {
    width: 13px;
    height: 13px;
  }
  .changes-head .refresh:hover:not(:disabled) {
    color: var(--ink-1);
  }
  .changes-head .refresh:disabled {
    opacity: 0.4;
    cursor: default;
  }

  /* Branch + PR context block, directly under the aligned title row. */
  .changes-ctx {
    flex: none;
    padding: 11px 16px 12px;
    border-bottom: 1px solid var(--line);
  }
  .branchline {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--ink-0);
  }
  .branchline :global(.ic) {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    color: var(--ink-2);
  }
  .branchline .b {
    flex: 0 1 auto;
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .branchline .from {
    flex: none;
    color: var(--ink-2);
    white-space: nowrap;
  }
  .branchline .ahead {
    margin-left: auto;
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    font-family: var(--mono);
    font-size: var(--r0);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--st-ok);
    cursor: default;
  }
  .branchline .ahead .arr {
    font-size: 0.8rem;
    line-height: 1;
  }
  .branchline .ahead .arr.down {
    margin-left: 4px;
  }

  /* PR chip — rectangular, hairline; the WHOLE chip is washed in the PR-state
     color (open green / merged purple / closed oxide), draft stays faint
     paper. No in-chip state word — that word lives in the title tooltip,
     which is the non-color channel. */
  .pr {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    margin-top: 9px;
    font-family: var(--mono);
    font-size: var(--r0);
    color: var(--ink-1);
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: 0;
    padding: 3px 10px 3px 8px;
    text-decoration: none;
  }
  .pr:hover .num {
    text-decoration: underline;
  }
  .pr :global(.pric) {
    width: 13px;
    height: 13px;
    flex: 0 0 13px;
    color: currentColor;
  }
  .pr .num {
    font-weight: 600;
  }
  .pr.open {
    color: var(--st-ok);
    background: var(--st-ok-wash);
    border-color: var(--st-ok-line);
  }
  .pr.merged {
    color: var(--pr-merged);
    background: var(--pr-merged-wash);
    border-color: var(--pr-merged-line);
  }
  .pr.closed {
    color: var(--st-need);
    background: var(--st-need-wash);
    border-color: var(--st-need-line);
  }
  .pr.draft {
    color: var(--ink-2);
  }

  /* ---- dynamic git action: ONE state-derived split button --------------- */
  .actions {
    flex: none;
    padding: 12px 16px;
    border-bottom: 1px solid var(--line);
    position: relative;
  }
  .splitbtn {
    display: flex;
    align-items: stretch;
    width: 100%;
    border-radius: 0;
    overflow: hidden;
    box-shadow: 0 1px 0 oklch(100% 0 0 / 0.14) inset;
  }
  .split-main {
    flex: 1;
    min-width: 0;
    justify-content: center;
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--iris);
    color: var(--iris-on);
    border: 1px solid var(--iris-ink);
    border-right: none;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: filter 0.15s ease-out;
  }
  .split-main:hover:not(:disabled) {
    filter: brightness(1.06);
  }
  .split-main :global(.btnic) {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    color: var(--iris-on);
  }
  .split-main .keys {
    margin-left: 2px;
  }
  .split-caret {
    flex: 0 0 30px;
    display: grid;
    place-items: center;
    background: var(--iris);
    color: var(--iris-on);
    border: 1px solid var(--iris-ink);
    box-shadow: inset 1px 0 0 var(--iris-on-sc-line);
    cursor: pointer;
    transition: filter 0.15s ease-out;
  }
  .split-caret:hover {
    filter: brightness(1.06);
  }
  .split-caret :global(svg) {
    width: 13px;
    height: 13px;
    display: block;
  }
  /* Disabled (`Up to date`): quiet paper treatment, muted ink. */
  .splitbtn.disabled .split-main,
  .splitbtn.disabled .split-caret {
    background: var(--paper-2);
    color: var(--ink-3);
    border-color: var(--line);
  }
  .splitbtn.disabled .split-caret {
    box-shadow: inset 1px 0 0 var(--line);
  }
  .split-main:disabled {
    cursor: default;
  }
  .splitbtn.disabled .split-main:hover,
  .splitbtn.disabled .split-caret:hover {
    filter: none;
  }

  /* Cancel state replaces the whole split with one quiet destructive button. */
  .cancel {
    width: 100%;
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    padding: 8px 12px;
    text-align: center;
    border-radius: 0;
    color: var(--st-need);
    background: transparent;
    border: 1px solid var(--st-need-line);
    cursor: pointer;
    transition: background 0.15s ease-out;
  }
  .cancel:hover {
    background: var(--st-need-wash);
  }

  /* Quiet "why this action is primary" hint, faint mono, under the split. */
  .why-primary {
    margin-top: 8px;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.01em;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  /* act-menu: the shared .menu/.mi/.m-sep recipe is global (app.css); add only
     the rail-specific width, primary marking, and disabled treatment. */
  :global(.act-menu) {
    min-width: 230px;
  }
  :global([data-theme="dark"] .act-menu) {
    box-shadow:
      0 0 0 1px oklch(8% 0.01 72 / 0.5),
      0 16px 34px -16px oklch(4% 0.01 72 / 0.7);
  }
  :global(.act-menu .mi.is-primary) {
    color: var(--iris-ink);
    font-weight: 600;
  }
  :global(.act-menu .mi.is-primary .mi-ico) {
    color: var(--iris-ink);
  }
  :global(.act-menu .mi[data-disabled]) {
    opacity: 0.42;
    pointer-events: none;
  }
  :global(.act-menu .mi[data-disabled] .mi-k) {
    color: var(--ink-3);
  }
  :global(.act-menu .mi.toggle) {
    color: var(--ink-1);
  }

  /* ---- file list -------------------------------------------------------- */
  .files {
    flex: 1;
    overflow: auto;
    min-height: 0;
    padding: 6px 10px 12px;
  }
  .empty {
    padding: 38px 20px;
    text-align: center;
  }
  .empty p {
    font-size: var(--r1);
    color: var(--ink-3);
    line-height: 1.55;
  }

  .fgroup {
    margin-top: 8px;
  }
  .fgroup h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
    padding: 6px 8px 5px;
    margin: 0;
  }
  /* The group label IS the entry point to the unified all-changes diff.
     A button only for semantics + keyboard focus; copies the h3's text
     rendering exactly (mono caps, same size/spacing/ink/weight) so it reads
     identical to the former <span> label, with a quiet hover to stronger ink. */
  .fgroup h3 .group-label {
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
    transition: color 0.2s ease-out;
  }
  .fgroup h3 .group-label:hover {
    color: var(--ink-1);
  }
  .fgroup h3 .ct {
    color: var(--ink-3);
    font-weight: 600;
  }
  .fgroup h3 .hr {
    flex: 1;
    height: 1px;
    background: var(--line);
  }
  .fgroup h3 .all {
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--iris-ink);
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: none;
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
    transition: opacity 0.15s ease-out;
  }
  .fgroup h3 .all:hover:not(:disabled) {
    opacity: 0.75;
  }
  .fgroup h3 .all.discard-all {
    color: var(--st-need);
  }
  .fgroup h3 .all:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .frow {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    text-align: left;
    font-family: var(--mono);
    font-size: var(--r1);
    padding: 5px 8px;
    border-radius: 0;
    color: var(--ink-2);
    cursor: pointer;
    background: transparent;
    border: 0;
    transition: background 0.15s ease-out;
  }
  .frow:hover {
    background: var(--paper-3);
  }
  /* `.roving` is the keyboard-focused row. It reuses the EXISTING `.active`
     (diff-selected) treatment verbatim — no new focus language — so a row is
     visibly selected while arrowing even before its diff opens. .frow is a
     <button>, so its native :focus is also styled below to match. */
  .frow.active,
  .frow.roving {
    background: var(--paper-3);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .frow:focus-visible {
    outline: none;
    background: var(--paper-3);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  /* The discard affordance is revealed for the roving row too (it is hidden by
     default and shown on hover/active). */
  .frow.roving .discard {
    opacity: 1;
  }
  .frow .chk {
    width: 14px;
    height: 14px;
    border-radius: 0;
    flex: 0 0 14px;
    border: 1px solid var(--line);
    display: grid;
    place-items: center;
    font-size: 0.6rem;
    color: var(--paper-2);
    background: var(--paper-1);
  }
  .frow .chk.on {
    background: var(--iris);
    border-color: var(--iris-ink);
    color: var(--iris-on);
  }
  .frow .st {
    width: 13px;
    text-align: center;
    font-weight: 700;
    font-size: 0.75rem;
    flex: 0 0 13px;
  }
  .frow .st.M {
    color: var(--st-stall);
  }
  .frow .st.A {
    color: var(--st-ok);
  }
  .frow .st.D {
    color: var(--diff-del);
  }
  .frow .st.U {
    color: var(--ink-3);
  }
  /* Per-file-type glyph (VS Code Material Icons, full colour). Sits between the
     status letter and the path. Deliberate colour exception in the otherwise
     monochrome shell — chosen for instant recognition at this small size.
     Rendered as <img> (colored SVGs aren't tintable). Fixed 16px box keeps rows
     from jumping; the flex row's align-items:center keeps it vertically centred. */
  .frow .ftype {
    flex: 0 0 16px;
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
  }
  .frow .ftype img {
    width: 16px;
    height: 16px;
    display: block;
  }
  .frow .path {
    flex: 1;
    min-width: 0;
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .frow .path .dir {
    color: var(--ink-2);
  }
  .frow .path b {
    color: var(--ink-0);
    font-weight: 500;
  }
  /* Per-file LOC change counts, right-aligned. Mirrors the header net diffstat
     (.changes-head .net) — same mono/tabular treatment and add/del tokens — so
     a row reads as that file's slice of the panel total. */
  .frow .fdiff {
    flex: 0 0 auto;
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-family: var(--mono);
    font-size: var(--r0);
    font-variant-numeric: tabular-nums;
  }
  .frow .fdiff .a {
    color: var(--diff-add);
    font-weight: 600;
  }
  .frow .fdiff .d {
    color: var(--diff-del);
    font-weight: 600;
  }
  /* Inline discard affordance — quiet, revealed on hover/active. */
  .frow .discard {
    flex: none;
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    border-radius: 0;
    color: var(--ink-3);
    opacity: 0;
    transition:
      opacity 0.15s ease-out,
      color 0.15s ease-out;
  }
  .frow:hover .discard,
  .frow.active .discard {
    opacity: 1;
  }
  .frow .discard:hover {
    color: var(--st-need);
  }

  /* ---- footer: keyboard legend ------------------------------------------ */
  .rail-r-foot {
    flex: none;
    border-top: 1px solid var(--line);
    padding: 8px 16px;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-2);
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .rail-r-foot span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
</style>
