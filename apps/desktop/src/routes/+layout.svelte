<script lang="ts">
  // Persistent app root. A SvelteKit root layout mounts once and is reused
  // across child-route navigation (`/` ↔ `/settings`), so the daemon connection
  // AND the three-pane shell set up here survive navigation — they are NOT
  // torn down when the route changes. Hosting the shell at the layout level
  // (rather than inside the `/` +page) is what keeps every live xterm and its
  // PTY-aligned grid intact when the user pops over to /settings and back;
  // remounting under /+page would re-parse the byte ring against a fresh
  // xterm whose size may not match the PTY's current cols/rows, displacing
  // wrapped lines and cursor-addressed TUI output.
  //
  // This layout owns:
  //   - the daemon connection lifecycle (initDaemon is idempotent; see daemon.ts)
  //   - the platform shortcuts: command palette (Cmd/Ctrl+K) and settings
  //     toggle (Cmd/Ctrl+,)
  //   - the WKWebView keep-alive heartbeat
  //   - the overlay surfaces (palette + dialogs) that any route may open
  //   - the 3-pane shell (TopNav · LeftRail · Center · RightRail) + rail state
  // Settings (and any future full-window route) renders via children() above
  // the shell; the shell is hidden with display:none so xterm sees a zero-
  // sized host and its ResizeObserver/fit no-ops — preserving the grid for
  // the moment the user returns.
  import "../app.css";
  import { onMount, tick } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { get } from "svelte/store";
  import {
    activeSessionId,
    activateTabIndex,
    activeTabIndex,
    closeActiveTab,
    disposeDaemon,
    initDaemon,
    openSession,
    orderedTabs,
    selectedParent,
    selectedProject,
  } from "$lib/daemon";
  import { initFileDrop } from "$lib/fileDrop";
  import {
    commandOpen,
    addProjectOpen,
    cloneProjectOpen,
    createWorktreeFor,
    removeProjectTarget,
    removeWorktreeTarget,
    commitOpen,
    createPrOpen,
    toggleLeftRailRequest,
    toggleRightRailRequest,
  } from "$lib/overlays";
  import { currentDesktopPlatform } from "$lib/desktopPlatform";
  import {
    focusedPane,
    focusTerminal,
    matchBinding,
    type FocusedPane,
  } from "$lib/keymap";
  import { terminalFontFamily } from "$lib/settings";
  import { ensureTerminalFontLoaded } from "$lib/terminalFont";
  import { initTheme } from "$lib/theme";
  import WindowControls from "$lib/components/WindowControls.svelte";
  import CommandPalette from "$lib/components/CommandPalette.svelte";
  import AddProjectDialog from "$lib/components/AddProjectDialog.svelte";
  import CloneProjectDialog from "$lib/components/CloneProjectDialog.svelte";
  import RemoteFolderBrowserDialog from "$lib/components/RemoteFolderBrowserDialog.svelte";
  import AddSshHostDialog from "$lib/components/AddSshHostDialog.svelte";
  import RemoveSshHostDialog from "$lib/components/RemoveSshHostDialog.svelte";
  import CreateWorktreeDialog from "$lib/components/CreateWorktreeDialog.svelte";
  import RemoveProjectDialog from "$lib/components/RemoveProjectDialog.svelte";
  import RemoveWorktreeDialog from "$lib/components/RemoveWorktreeDialog.svelte";
  import TopNav from "$lib/components/TopNav.svelte";
  import LeftRail from "$lib/components/LeftRail.svelte";
  import Center from "$lib/components/Center.svelte";
  import RightRail from "$lib/components/RightRail.svelte";
  import { Toaster } from "svelte-french-toast";

  let { children } = $props();

  // Rail collapse state lives here (not in /+page.svelte) so a navigation to
  // /settings and back doesn't snap collapsed rails back to expanded — which
  // would also change the center column's width and invalidate the live
  // xterm grids.
  let showLeft = $state(true);
  let showRight = $state(true);

  // The command palette can't reach this layout-local rail state, so its
  // "Toggle left/right rail" commands bump request counters in overlays.ts;
  // here we flip the matching rail when a counter advances. We read the count
  // (not just subscribe for its side effect) and gate on a remembered baseline
  // so the initial run doesn't fire a spurious toggle on mount.
  let seenToggleLeft = $state(get(toggleLeftRailRequest));
  let seenToggleRight = $state(get(toggleRightRailRequest));
  $effect(() => {
    if ($toggleLeftRailRequest !== seenToggleLeft) {
      seenToggleLeft = $toggleLeftRailRequest;
      showLeft = !showLeft;
    }
  });
  $effect(() => {
    if ($toggleRightRailRequest !== seenToggleRight) {
      seenToggleRight = $toggleRightRailRequest;
      showRight = !showRight;
    }
  });

  // Routes that fully replace the shell (the settings page renders on top of
  // the layout while the shell is hidden). Keep this allowlist tight — any
  // future overlay route must opt in explicitly so daemon-driven views never
  // tear down the warm terminal cache by accident.
  const SHELL_HIDDEN_ROUTES = new Set(["/settings"]);
  const shellHidden = $derived(SHELL_HIDDEN_ROUTES.has(page.url.pathname));

  // Heartbeat opacity for the keep-alive dot (see below). Toggled on a timer so
  // the WebContent process always has scheduled work to flush.
  let hbOpacity = $state(0.01);
  const desktopPlatform = currentDesktopPlatform();


  // Is any overlay/dialog open? Shortcuts are suppressed while one is — the
  // dialog owns the keyboard (typing, Esc-to-close). bits-ui dialogs handle Esc
  // themselves, so the dispatcher simply never intercepts while any of these are
  // open. Object-scoped dialogs (createWorktreeFor, removeProjectTarget,
  // removeWorktreeTarget) are "open" when non-null.
  function anyOverlayOpen(): boolean {
    return (
      get(commandOpen) ||
      get(addProjectOpen) ||
      get(cloneProjectOpen) ||
      get(commitOpen) ||
      get(createPrOpen) ||
      get(createWorktreeFor) !== null ||
      get(removeProjectTarget) !== null ||
      get(removeWorktreeTarget) !== null
    );
  }

  // True when the event target is an editable element (input/textarea/select or
  // a contenteditable). Bare-key bindings (R, Space, arrows) must never fire
  // while the user is typing. NOTE xterm's hidden textarea is also editable —
  // but bare-key bindings are pane-gated (when === focusedPane) and the terminal
  // pane has no bare-key bindings, so a focused terminal naturally never matches
  // one; this guard covers app inputs (commit message, palette, dialogs).
  function isEditableTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    const tag = target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
    return target.isContentEditable;
  }

  // Open the command palette (Cmd/Ctrl+K).
  function openPalette() {
    commandOpen.update((open) => !open);
  }

  // Cmd+, (Ctrl+, elsewhere) — the platform-conventional preferences shortcut.
  // Toggles: from the shell it opens /settings, from /settings it returns to the
  // shell (Escape on the page does the same). Identical to the pre-keymap behavior.
  function toggleSettings() {
    commandOpen.set(false);
    void goto(page.url.pathname === "/settings" ? "/" : "/settings");
  }

  // Move focus into a pane: set the focusedPane store, ensure the pane is
  // visible (un-collapse a hidden rail), then move DOM focus to its root (the
  // roving-row focus lands in slices 3/4). Terminal focus goes through the
  // keymap's per-session focuser registry.
  function focusPane(pane: FocusedPane) {
    focusedPane.set(pane);
    if (pane === "terminal") {
      focusTerminal(get(activeSessionId));
      return;
    }
    if (pane === "tree" && !showLeft) showLeft = true;
    if (pane === "git" && !showRight) showRight = true;
    // Focus after the (possible) un-collapse has applied so the element is
    // focusable (a collapsed rail is pointer-events:none but still focusable;
    // tick keeps this robust if visibility gating ever changes).
    void tick().then(() => {
      const sel = pane === "tree" ? '[data-pane="tree"]' : '[data-pane="git"]';
      document.querySelector<HTMLElement>(sel)?.focus();
    });
  }

  // Activate the tab at `index` in the visual order (sessions then diff tabs).
  // Out-of-range is a no-op (Cmd+N with fewer than N tabs). When the tab is a
  // session, also move DOM focus into its terminal and mark the terminal pane
  // focused — keyboard tab-switching should land the cursor in the terminal,
  // closing the same focus gap fixed in SessionTabs.select(). Diff tabs leave
  // focus where it is (their surface has no text entry to grab).
  function activateTab(index: number) {
    const tab = get(orderedTabs)[index];
    if (!tab) return;
    activateTabIndex(index);
    if (tab.kind === "session") {
      focusedPane.set("terminal");
      focusTerminal(get(activeSessionId));
    }
  }

  // Cycle to the next/previous tab in the visual order with wraparound. Pivots
  // off the currently active tab; with no active tab (empty strip) it's a no-op.
  function cycleTab(delta: number) {
    const tabs = get(orderedTabs);
    if (tabs.length === 0) return;
    const current = activeTabIndex();
    if (current === -1) return;
    activateTab((current + delta + tabs.length) % tabs.length);
  }

  // Cmd+T — open a new shell tab in the active parent (terminal-app convention:
  // Cmd+T gives you a plain shell; agents are an explicit pick from the
  // new-session menu/palette). No-op when no parent is selected (nothing to
  // host it).
  function newTab() {
    const parent = get(selectedParent);
    if (parent) void openSession(parent, "shell", null);
  }

  // The command dispatch table. The dispatcher ignores (does NOT preventDefault)
  // a matched binding with no handler, so unwired ids fall through to the
  // terminal/pane. A handler returns `false` to signal it DECLINED to act (its
  // own gate failed — e.g. git.commit outside the git pane, tree.newWorktree
  // with no git-backed project); the dispatcher then leaves the key alone rather
  // than consuming it for nothing. Handlers that always act return void.
  const handlers: Record<string, () => boolean | void> = {
    "focus.tree": () => focusPane("tree"),
    "focus.terminal": () => focusPane("terminal"),
    "focus.git": () => focusPane("git"),
    // Statement bodies (not expression bodies) so these return undefined, not
    // the boolean assignment value: an expression-body arrow returns `!showLeft`,
    // which is `false` whenever the rail is being HIDDEN — the dispatcher's
    // "returns false === declined" contract (see below) would then skip
    // preventDefault on every hide, letting Cmd+B leak to the terminal/panes.
    "toggle.left": () => {
      showLeft = !showLeft;
    },
    "toggle.right": () => {
      showRight = !showRight;
    },
    "palette.open": openPalette,
    "settings.toggle": toggleSettings,
    "focus.terminal.escape": () => {
      // Esc returns focus to the terminal only from a non-terminal pane (Esc in
      // the terminal must reach the PTY for vim/TUIs). Suppression while an
      // overlay is open is handled by the dispatcher's early return.
      if (get(focusedPane) !== "terminal") focusPane("terminal");
    },
    // ---- tabs (slice 2) ---------------------------------------------------
    // Cmd+1…9 jump to the Nth tab in the visual order (1-based → 0-based).
    // Generated to mirror the `tab.jump.${n}` ids keymap.ts emits for 1…9.
    ...Object.fromEntries(
      Array.from({ length: 9 }, (_, i) => [`tab.jump.${i + 1}`, () => activateTab(i)]),
    ),
    // Both next/prev pairs (Cmd+Shift+]/[ and Ctrl+Tab/Ctrl+Shift+Tab) cycle.
    "tab.next": () => cycleTab(1),
    "tab.prev": () => cycleTab(-1),
    "tab.next.ctrl": () => cycleTab(1),
    "tab.prev.ctrl": () => cycleTab(-1),
    "tab.new": newTab,
    "tab.close": closeActiveTab,
    // ---- tree (slice 3) ---------------------------------------------------
    // Cmd+N opens the create-worktree dialog for the selected project. It's a
    // modifier combo, so `matchBinding` does NOT pane-gate it (only bare keys are
    // gated); we gate it HERE to the tree pane — mirroring git.commit — so it
    // stays scoped to the tree context (its keymap `when: "tree"`) rather than
    // hijacking Cmd+N from the terminal or git pane. It additionally requires a
    // selected git-backed project (worktrees only exist for those), declining to
    // a no-op otherwise rather than opening an empty dialog. The bare tree keys
    // (↑/↓/←/→/Enter/Space) are handled component-locally in ProjectTree (DOM
    // focus is inside the pane); their ids stay unwired so the dispatcher lets
    // them fall through to the tree.
    "tree.newWorktree": () => {
      if (get(focusedPane) !== "tree") return false; // declined — no key consumed
      const project = get(selectedProject);
      if (project?.kind !== "git-backed") return false; // declined — no key consumed
      createWorktreeFor.set(project);
    },
    // ---- git --------------------------------------------------------------
    // Cmd+Enter opens the Composer (commit mode) — the key the right-rail footer
    // advertises. `git.commit` is a modifier combo, so `matchBinding` does NOT
    // pane-gate it (only bare keys are gated); we gate it HERE to the git pane so
    // it stays scoped to the Changes context the footer legend implies, rather
    // than hijacking Cmd+Enter globally. While the Composer is open the dispatcher
    // returns early (commitOpen counts as an overlay in anyOverlayOpen), so the
    // Composer's own Cmd/Ctrl+Enter handler is the sole route there — that key
    // confirms the commit (or queues commit-on-arrival mid-generation), no
    // conflict. The bare git keys (↑/↓/Space/Enter/Backspace/R) are handled
    // component-locally in RightRail (DOM focus is inside the pane); their keymap
    // ids stay unwired here so the dispatcher lets them fall through to the rail.
    "git.commit": () => {
      if (get(focusedPane) !== "git") return false; // declined — no key consumed
      commitOpen.set(true);
    },
  };

  function onKeydown(event: KeyboardEvent) {
    const pane = get(focusedPane);
    const binding = matchBinding(event, desktopPlatform, pane);
    if (!binding) return;

    // On a shell-hidden route (/settings) the 3-pane shell is display:none, so
    // tab/pane/git shortcuts would act on a hidden shell and Esc would
    // double-handle with the settings page's own Esc-to-back. Only the palette
    // and settings toggle make sense there; everything else falls through.
    if (shellHidden && binding.id !== "palette.open" && binding.id !== "settings.toggle") {
      return;
    }

    // While an overlay is open, intercept nothing: the dialog owns the keyboard
    // (typing, and Esc-to-close, which bits-ui handles itself). Returning here —
    // rather than special-casing Esc — keeps typing in the commit dialog /
    // palette from triggering single-key shortcuts and leaves Esc to the dialog.
    // Carve-out: palette.open must survive the gate when the PALETTE itself is the
    // open overlay, or its toggle-shaped openPalette() handler is unreachable in
    // the close direction (Cmd+K could open the palette but never close it). The
    // get(commandOpen) check keeps the gate intact for OTHER overlays — Cmd+K
    // stays blocked while a commit/create-worktree dialog owns the keyboard.
    if (anyOverlayOpen() && !(binding.id === "palette.open" && get(commandOpen))) {
      return;
    }

    // Bare-key bindings (no modifier) must not fire while typing in an editable
    // element. Modifier combos are always eligible.
    const hasModifier =
      binding.combo.primary ||
      binding.combo.ctrl ||
      binding.combo.shift ||
      binding.combo.alt;
    if (!hasModifier && isEditableTarget(event.target)) return;

    const handler = handlers[binding.id];
    if (!handler) return; // Unwired (tab/tree/git) — let the key fall through.

    // Run the handler; only consume the key if it actually acted. A handler that
    // returns false declined (its pane/state gate failed — e.g. git.commit
    // outside the git pane), so we leave the key alone rather than swallowing it.
    if (handler() !== false) event.preventDefault();
  }

  // Apply the persisted (or default light "paper") theme to <html> and keep it
  // in sync; see theme.ts. This runs during layout init — BEFORE any child
  // mounts — so components that resolve token values at mount (Terminal's
  // xterm theme reads computed colors off <html>) see the correct theme.
  // onMount would be too late: children mount before the parent's onMount.
  initTheme();

  onMount(() => {
    void initDaemon();
    // Preload the picked terminal font's web-font faces (multi-MB reads off
    // disk) so the first terminal usually mounts with them already usable —
    // Terminal.svelte's applyTerminalFont() awaits the same cached promise.
    void ensureTerminalFontLoaded(get(terminalFontFamily));
    // App-wide OS-file-drop listener: drops onto a terminal insert the dropped
    // paths at its prompt (see fileDrop.ts for why this is window-global rather
    // than a per-terminal DOM handler). Registration is async; stash the
    // unlisten so teardown removes it even if the promise resolves after unmount.
    let unlistenDrop: (() => void) | null = null;
    void initFileDrop().then((unlisten) => {
      unlistenDrop = unlisten;
    });
    // Keep the macOS WKWebView from going dormant. When the page has no
    // scheduled work (no terminal mounted, or the only terminal is unfocused so
    // xterm's cursor-blink timer is paused), the webview stops flushing frames:
    // clicks and store updates are processed but never painted, so the UI looks
    // frozen until an external event (resize/refresh) wakes it. A low-frequency
    // opacity toggle — the same trick xterm's blinking cursor relies on —
    // guarantees a steady stream of frames so async updates always paint.
    const heartbeat = setInterval(() => {
      hbOpacity = hbOpacity === 0.01 ? 0.02 : 0.01;
    }, 500);
    return () => {
      clearInterval(heartbeat);
      unlistenDrop?.();
      disposeDaemon();
    };
  });
</script>

<!-- One capture-phase keydown listener drives the whole keymap. Capture (the
     Svelte 5 `onkeydowncapture`) fires BEFORE xterm's textarea listener, so app
     combos win even when the terminal is focused; the matched command runs and
     preventDefault stops the key from also reaching the terminal. The xterm
     classifier's "app" pass-through (terminalKeys.ts) is the belt to this
     suspenders — together they guarantee app combos never reach the PTY. -->
<svelte:window onkeydowncapture={onKeydown} />

<!-- The shell is mounted exactly once for the app's lifetime. When a route
     opts into replacing it (currently only /settings), we hide it via the
     `.shell-hidden` class — `display:none` strips the host of measurable
     area so the Terminal's ResizeObserver/fit guards no-op (no grid changes),
     and the warm xterm + WebGL renderer continues to consume PTY output in
     place. The moment the user navigates back, the shell becomes visible
     again at exactly the size it had on the way out. -->
<div class="window" class:no-left={!showLeft} class:no-right={!showRight} class:shell-hidden={shellHidden} aria-hidden={shellHidden}>
  <TopNav />

  <div class="body">
    <LeftRail collapsed={!showLeft} />
    <Center />
    <RightRail collapsed={!showRight} />
  </div>
</div>

{@render children()}

<!-- Windows is frameless (decorations:false). The caption controls live here,
     OUTSIDE the `.window` shell, so they stay visible on full-window routes that
     hide the shell with display:none (currently /settings) — otherwise Windows
     users would lose every minimize/maximize/close button there. Rendered once
     (a fixed top-right layer) to keep the single native Snap-Layouts overlay
     parked over one max-button rectangle. -->
{#if desktopPlatform === "windows"}
  <WindowControls />
{/if}

<div class="wk-keepalive" aria-hidden="true" style="opacity:{hbOpacity}"></div>

<CommandPalette />
<AddProjectDialog />
<CloneProjectDialog />
<RemoteFolderBrowserDialog />
<AddSshHostDialog />
<RemoveSshHostDialog />
<CreateWorktreeDialog />
<RemoveProjectDialog />
<RemoveWorktreeDialog />
<!-- Toasts wear the letterpress chrome: paper-2 fill, ink-0 text, a hairline
     --line border, radius 0. Tokens (var()) resolve live so toasts follow the
     paper/dusk theme switch. The icon accent is the iris primary. -->
<Toaster
  position="bottom-right"
  toastOptions={{
    style:
      "background: var(--paper-2); color: var(--ink-0); border: 1px solid var(--line); border-radius: 0; font-size: 12px; padding: 10px 14px;",
    iconTheme: {
      primary: "var(--iris)",
      secondary: "var(--paper-2)",
    },
  }}
/>

<style>
  /* The 3-pane shell. Lives at the layout level (not the route) so the live
     xterm instances inside Center survive navigation to overlay routes — see
     the script-level note above. Layout/visual rules are unchanged from the
     previous /+page.svelte; only the mount point moved. */
  .window {
    height: 100%;
    width: 100%;
    display: grid;
    grid-template-rows: 42px 1fr;
    background: var(--paper-0);
    overflow: hidden;
  }
  /* `display:none` collapses the shell out of layout entirely so a route
     rendered above it (children() — e.g. /settings) gets the full viewport.
     xterm's host measures zero while hidden, so ResizeObserver/fit no-op and
     no daemon resize fires; the terminal grid is identical on return. */
  .window.shell-hidden {
    display: none;
  }
  .body {
    display: grid;
    grid-template-columns: var(--w-left, 295px) 1fr var(--w-right, 330px);
    min-height: 0;
    transition: grid-template-columns 0.2s ease-out;
  }
  .window.no-left .body {
    --w-left: 0px;
  }
  .window.no-right .body {
    --w-right: 0px;
  }

  /* WKWebView keep-alive dot — an imperceptible 1px square whose opacity the
     heartbeat toggles to force a repaint each tick. See onMount above. */
  .wk-keepalive {
    position: fixed;
    top: 0;
    left: 0;
    width: 1px;
    height: 1px;
    background: var(--iris);
    pointer-events: none;
  }
</style>
