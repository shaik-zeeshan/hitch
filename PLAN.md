# Plan: Terminal experience overhaul

## Problem

Hitch is a terminal-first product (PRODUCT.md: "the terminal is the product"), but the live terminal has three classes of defects that undermine that promise:

1. **Resize/layout glitches.** Switching tabs or peeking at a diff fully tears down and rebuilds the xterm instance (`Center.svelte:55-60`), and every `ResizeObserver` tick refits *and* fires a `resize-session` to the daemon with no debounce (`Terminal.svelte:109-117`). When a TUI or agent UI is running, dragging the window or toggling the right rail floods the child with `SIGWINCH`s and garbles its redraw; switching tabs loses scroll position and replays the whole buffer.
2. **Key repeat is dead.** Holding a key (e.g. `j` in vim) does nothing, because macOS WKWebView's press-and-hold accent popup (`ApplePressAndHoldEnabled`) suppresses OS key repeat. Nothing in the code disables it.
3. **Output correctness + memory.** PTY bytes are `String::from_utf8_lossy`'d *per 8 KB frame* (`lib.rs:399`), so any multi-byte glyph split across a read boundary becomes `�` — constant for agents/TUIs. The frontend `buffers` store appends every chunk to one string **forever, unbounded** (`daemon.ts:315`), a memory leak that also slows repaint.

Developers need a terminal that stays fluid under load, repeats keys, renders glyphs correctly, and never loses its place on a layout change.

## Solution

Rework the PTY data plane and the terminal's rendering model, then layer on standard terminal-grade UX:

- **Keep terminals warm.** Mount one xterm per Session for the active parent and hide/show instead of destroy/rebuild. Switching tabs/diff becomes instant and preserves scroll + selection.
- **Stream bytes over a per-session binary `Channel`, not the event bus** (see `docs/adr/0007-pty-output-channel.md`). Never stringify in Rust; let xterm's UTF-8 decoder handle frames. The GUI keeps a bounded byte ring for repaint; the daemon stays the durable scrollback owner.
- **Tame resize.** rAF-coalesce the local `fit()`, debounce the daemon `resize-session`, guard zero-size, and fit on activate.
- **Disable press-and-hold** at macOS startup so keys repeat.
- **Add terminal-grade extras:** GPU rendering (active terminal only), correct initial PTY size, bracketed paste + clipboard, clickable links, in-terminal search, and a non-intrusive "new output" affordance.

## User Stories

1. As a developer, I want switching between session tabs and the diff to be instant and preserve my scroll position, so that I don't lose context or wait on a rebuild.
2. As a developer, I want resizing the window or toggling panels to not garble a running TUI/agent, so that full-screen tools stay legible.
3. As a developer, I want holding a key to repeat it, so that vim/less navigation works.
4. As a developer, I want box-drawing, CJK, and emoji from agents/TUIs to render correctly, so that output isn't littered with `�`.
5. As a developer, I want heavy output (big `cat`, noisy build, streaming agent) to stay smooth, so that the UI doesn't lag or leak memory.
6. As a developer, I want a new terminal to open at the correct size immediately, so that I don't see a prompt reflow flash.
7. As a developer, I want safe paste, clickable links, and search in the terminal, so that it matches the tools I already trust.

## Implementation Decisions

- **Keep-alive scope = active parent only.** Within a parent, all its Sessions stay mounted; hidden ones use `display:none`. Switching parents unmounts the previous parent's terminals. (Bounds the number of live xterm instances.)
- **Data plane = per-session `Channel<&[u8]>`** between Tauri and the webview; control plane (`session/worktree/agent-state`) stays on `emit`/`listen`. Channel opens on `session-opened`, closes on `session-closed`, lives in the Tauri process so it survives daemon reconnects.
- **GUI buffer holds bytes** (`Uint8Array`) in a bounded ring (~daemon's 1 MB scrollback cap). Paint tracks a **total-bytes-seen** offset (not current-buffer-length) so head-trims don't corrupt the append math. Daemon remains authoritative scrollback owner (ADR 0003 unchanged).
- **No Rust-side UTF-8 decoding.** `term.write(Uint8Array)` does it correctly across frames.
- **Resize:** `fit()` coalesced via `requestAnimationFrame`; `resize-session` debounced ~60–80 ms trailing; skip when host measures 0×0; explicit `fit()` when a Session becomes active.
- **Press-and-hold:** `[[NSUserDefaults standardUserDefaults] registerDefaults:@{@"ApplePressAndHoldEnabled": @NO}]` in the Tauri macOS setup hook. Process-scoped, no disk write, app-wide (accepted: no accent-hold in Hitch input fields).
- **Output writes are rAF-batched** into xterm to avoid per-frame Svelte reactivity storms.
- **WebGL** (`@xterm/addon-webgl`) attached only to the *active* terminal; detached when it goes hidden, to stay under the browser's ~16-context cap. Fall back to default renderer on context-loss/unsupported.
- **Initial PTY size:** add `cols/rows` to the `OpenSession` request so the PTY spawns at the real size instead of the 120×40 default (`hitch-pty/src/lib.rs:18-19`).
- **Bigger PTY reads:** bump the reader buffer 8 KB → 64 KB (`hitch-pty/src/lib.rs:223`).
- **Bracketed paste** enabled; Cmd+C copies selection, Cmd+V pastes via `term.paste()`.
- Assumption: the diff tab and session tabs continue to share the center column; keep-alive means the diff overlays/hides terminals rather than replacing them in the render tree.

## Testing Decisions

- **Rust:** keep `hitch-pty` streaming test green after the read-buffer bump; add a `hitch-proto` round-trip test for `OpenSession { cols, rows }` (follow the existing `ResizeSession` round-trip pattern in `message.rs`).
- **Frontend logic:** unit-test the bounded ring buffer — append past capacity trims the head, and the total-bytes-seen offset still yields a correct paint tail (no duplication, no loss).
- **Manual verification (the real acceptance):**
  - Hold `j` in vim → cursor repeats. (Decision 4)
  - Run vim/lazygit, drag the window + toggle the right rail → no garble, settles at the right size. (Decision 3)
  - Switch tabs and open/close the diff mid-scroll → instant, scroll position preserved, no replay flash. (Decision 1)
  - `cat` a large file / run a noisy build → stays smooth, memory flat over time. (Decisions 2, 6)
  - Print box-drawing + emoji + CJK across a forced chunk boundary → no `�`. (Decision 5)
  - Open a new terminal → no prompt reflow flash. (Tier 1 #2)
  - Paste a multi-line blob → held by bracketed paste, not auto-executed. (Tier 1 #3)
- **Do not test:** xterm/addon internals, exact rAF timing — assert observable behavior (no SIGWINCH storm = one resize after settle; correct final grid).

## Slices

1. **Disable macOS press-and-hold**
   - Goal: holding a key repeats.
   - Areas: `apps/desktop/src-tauri/src/main.rs` (or `lib.rs`) macOS setup hook; objc `registerDefaults`.
   - Acceptance: holding `j` in vim repeats; verified manually.
   - Depends on: none. Parallel: yes.

2. **Bigger PTY read buffer (8 KB → 64 KB)**
   - Goal: fewer frames / less splitting under heavy output.
   - Areas: `crates/hitch-pty/src/lib.rs:223`.
   - Acceptance: existing pty streaming test passes; manual heavy-output check.
   - Depends on: none. Parallel: yes.

3. **Bracketed paste + clipboard**
   - Goal: safe paste, Cmd+C/Cmd+V.
   - Areas: `Terminal.svelte`.
   - Acceptance: multi-line paste is bracketed; copy/paste work.
   - Depends on: none. Parallel: yes.

4. **Clickable links + search addons (Tier 2)**
   - Goal: `@xterm/addon-web-links`, `@xterm/addon-search`.
   - Areas: `Terminal.svelte`, package deps.
   - Acceptance: URLs open in browser; Cmd+F searches scrollback.
   - Depends on: none. Parallel: yes.

5. **Binary output data plane: per-session Channel + bytes-to-xterm + UTF-8 fix**
   - Goal: stream raw bytes; xterm decodes; no `from_utf8_lossy`.
   - Areas: `apps/desktop/src-tauri/src/lib.rs` (Channel per session, drop stringify), `apps/desktop/src/lib/daemon.ts` (channel subscription replaces output `listen`), `Terminal.svelte` (`term.write(Uint8Array)`).
   - Acceptance: multibyte glyphs across a forced boundary render cleanly; output still flows on reconnect.
   - Depends on: none (foundational). Parallel: no — slices 6–10 build on its bytes contract.

6. **Bounded byte ring buffer + paint-by-offset**
   - Goal: kill the unbounded string; cap to ~daemon scrollback.
   - Areas: `daemon.ts` (`buffers` store → bytes ring), `Terminal.svelte` (`written` → total-bytes-seen).
   - Acceptance: ring-buffer unit test passes; memory flat under sustained output.
   - Depends on: 5.

7. **Keep-alive rendering per active parent**
   - Goal: mount once, hide/show; no teardown on tab/diff switch.
   - Areas: `Center.svelte` (remove `{#key}` remount, render all visible-parent sessions, hide inactive), `Terminal.svelte` (fit-on-activate).
   - Acceptance: tab/diff switch instant, scroll/selection preserved.
   - Depends on: 6 (paint/repaint model).

8. **Resize: rAF-coalesce + debounce + zero-size guard**
   - Goal: smooth reflow, one SIGWINCH after settle.
   - Areas: `Terminal.svelte` resize path, `daemon.ts:resizeSession` (debounce).
   - Acceptance: vim doesn't garble on drag/rail-toggle; settles correctly.
   - Depends on: 7 (fit-on-activate integration).

9. **Initial PTY size on open-session**
   - Goal: no prompt reflow flash on new terminals.
   - Areas: `crates/hitch-proto/src/message.rs` (`OpenSession { cols, rows }`), `crates/hitch-daemon/src/main.rs` (pass size to spawn), `daemon.ts:openSession`, `Terminal.svelte` (compute initial size).
   - Acceptance: proto round-trip test; new terminal opens at correct size, no reflow.
   - Depends on: 8 (size computation).

10. **WebGL renderer, active-only**
    - Goal: GPU rendering for the active terminal.
    - Areas: `Terminal.svelte` (attach on activate, dispose on hide; context-loss fallback), package deps.
    - Acceptance: active terminal uses WebGL; switching parents doesn't exhaust contexts; falls back gracefully.
    - Depends on: 7 (activate/hide hooks).

11. **"New output ↓" nudge (Tier 2)**
    - Goal: don't yank a scrolled-up reader to the bottom; show a pill instead.
    - Areas: `Terminal.svelte` (scroll-at-bottom detection + affordance). First verify xterm's current scroll-lock behavior.
    - Acceptance: scrolled-up + new output → stays put, pill appears; click jumps to bottom.
    - Depends on: 7, 8.

**Parallel groups:**
- Group A (immediately, independent): **1, 2, 3, 4.**
- Group B (data-plane chain, sequential): **5 → 6 → 7 → 8.**
- Group C (after 7/8): **9, 10, 11** can run in parallel with each other.

## Out of Scope

- **Tier 3 deferred:** re-resolving theme on light/dark toggle (`resolveTheme()` runs once at mount); explicit mouse-reporting verification for TUIs. Revisit later.
- Promoting the data plane to a localhost socket bypassing Tauri (ADR 0007 rejected it; the daemon stays Unix-socket-local).
- Daemon-authority `GetScrollback` fetch-on-mount (rejected in favor of the bounded GUI ring; would reopen the attach race).
- Keep-alive across *parent* switches (scoped to the active parent only, by design).
- Any change to the daemon's scrollback ownership or persistence model (ADR 0003 stands).
- Synthesizing key repeat in JS (rejected — fights the OS).

## Further Notes

- **Risk — WebGL context cap.** Browsers allow ~16 live WebGL contexts; active-only attach + keep-alive scoped to one parent keeps us well under, but if a parent ever hosts many sessions, confirm hidden terminals release their context. Always wire the canvas-renderer fallback on `webglcontextlost`.
- **Risk — resize debounce lag.** A 60–80 ms trailing debounce means a brief cosmetic window where the rendered grid is wider/narrower than the PTY believes; acceptable, but if it reads as tearing, shorten toward 50 ms.
- **Verify the press-and-hold diagnosis** before considering Decision 4 closed — it's a strong hypothesis, not yet confirmed in this app.
- **Docs:** ADR 0007 already records the data-plane decision. CONTEXT.md needs no change — all of this is implementation of the existing Session/Daemon terms. Add a short code comment at the press-and-hold call explaining *why* (it's surprising but not ADR-worthy).
