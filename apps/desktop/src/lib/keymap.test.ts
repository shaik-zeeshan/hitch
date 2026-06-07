import { describe, expect, it } from "vitest";
import { appCombos, matchBinding, type FocusedPane } from "./keymap";
import type { DesktopPlatform } from "./desktopPlatform";

// Minimal event shape matchBinding reads; lets us assert resolution without DOM
// (mirrors terminalKeys.test.ts).
const ev = (
  over: Partial<{
    key: string;
    metaKey: boolean;
    ctrlKey: boolean;
    shiftKey: boolean;
    altKey: boolean;
  }> = {},
) => ({
  key: "",
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...over,
});

const match = (
  platform: DesktopPlatform,
  over: Parameters<typeof ev>[0],
  pane: FocusedPane = "terminal",
) => matchBinding(ev(over), platform, pane);

const id = (
  platform: DesktopPlatform,
  over: Parameters<typeof ev>[0],
  pane: FocusedPane = "terminal",
) => match(platform, over, pane)?.id ?? null;

describe("matchBinding — global combos per platform", () => {
  it("resolves the primary modifier to Cmd on macOS and Ctrl elsewhere", () => {
    expect(id("macos", { key: "k", metaKey: true })).toBe("palette.open");
    expect(id("windows", { key: "k", ctrlKey: true })).toBe("palette.open");
    expect(id("linux", { key: "k", ctrlKey: true })).toBe("palette.open");
  });

  it("does NOT treat metaKey as the primary modifier off macOS", () => {
    expect(id("windows", { key: "k", metaKey: true })).toBeNull();
  });

  it("matches Cmd+Shift+E (focus tree) and Cmd+Shift+G (focus git)", () => {
    expect(id("macos", { key: "e", metaKey: true, shiftKey: true })).toBe("focus.tree");
    expect(id("macos", { key: "g", metaKey: true, shiftKey: true })).toBe("focus.git");
    expect(id("windows", { key: "E", ctrlKey: true, shiftKey: true })).toBe("focus.tree");
  });

  it("matches Ctrl+` (focus terminal) as literal Ctrl on BOTH platforms", () => {
    expect(id("macos", { key: "`", ctrlKey: true })).toBe("focus.terminal");
    expect(id("windows", { key: "`", ctrlKey: true })).toBe("focus.terminal");
    // Cmd+` on macOS must NOT match (it's Ctrl-only).
    expect(id("macos", { key: "`", metaKey: true })).toBeNull();
  });

  it("migrates Cmd+, (settings) per platform", () => {
    expect(id("macos", { key: ",", metaKey: true })).toBe("settings.toggle");
    expect(id("windows", { key: ",", ctrlKey: true })).toBe("settings.toggle");
  });

  it("matches Cmd+B (toggle left) and Cmd+Alt+B (toggle right) distinctly", () => {
    expect(id("macos", { key: "b", metaKey: true })).toBe("toggle.left");
    expect(id("macos", { key: "b", metaKey: true, altKey: true })).toBe("toggle.right");
  });
});

describe("matchBinding — exact modifier discrimination", () => {
  it("Cmd+Shift+E does NOT match the Cmd+E-shaped slot (no plain Cmd+E binding)", () => {
    // There is no plain-Cmd binding for 'e', so plain Cmd+E must be null while
    // Cmd+Shift+E matches focus.tree — proving shift is required-or-forbidden.
    expect(id("macos", { key: "e", metaKey: true })).toBeNull();
    expect(id("macos", { key: "e", metaKey: true, shiftKey: true })).toBe("focus.tree");
  });

  it("Cmd+B does NOT match when Alt is also held (that's Cmd+Alt+B)", () => {
    expect(id("macos", { key: "b", metaKey: true })).toBe("toggle.left");
    expect(id("macos", { key: "b", metaKey: true, altKey: true })).not.toBe("toggle.left");
  });

  it("requires the primary modifier — bare 'k' is not the palette", () => {
    expect(id("macos", { key: "k" })).toBeNull();
  });

  it("Ctrl+Tab vs Ctrl+Shift+Tab resolve to next/prev", () => {
    expect(id("macos", { key: "Tab", ctrlKey: true })).toBe("tab.next.ctrl");
    expect(id("macos", { key: "Tab", ctrlKey: true, shiftKey: true })).toBe("tab.prev.ctrl");
  });

  it("Cmd/Ctrl+Shift+]/[ resolve via the shifted glyph the browser reports", () => {
    // With Shift held, browsers put the PRODUCED character in event.key, so the
    // combo specced as "]"/"[" actually arrives as "}"/"{". Assert the REAL
    // shape — synthesizing {key:"]", shiftKey:true} is something no browser emits.
    expect(id("macos", { key: "}", metaKey: true, shiftKey: true })).toBe("tab.next");
    expect(id("macos", { key: "{", metaKey: true, shiftKey: true })).toBe("tab.prev");
    expect(id("windows", { key: "}", ctrlKey: true, shiftKey: true })).toBe("tab.next");
    expect(id("linux", { key: "{", ctrlKey: true, shiftKey: true })).toBe("tab.prev");
  });

  it("matches Cmd+1..9 tab jumps", () => {
    expect(id("macos", { key: "1", metaKey: true })).toBe("tab.jump.1");
    expect(id("macos", { key: "9", metaKey: true })).toBe("tab.jump.9");
    expect(id("windows", { key: "3", ctrlKey: true })).toBe("tab.jump.3");
  });
});

describe("matchBinding — bare-key pane gating", () => {
  it("arrows resolve to the focused pane's binding (or null in terminal)", () => {
    expect(id("macos", { key: "ArrowDown" }, "tree")).toBe("tree.down");
    // The git pane has its OWN ArrowDown binding, so a git-focused ArrowDown
    // resolves to git.down — not the tree's.
    expect(id("macos", { key: "ArrowDown" }, "git")).toBe("git.down");
    // No bare-key arrow binding is gated to the terminal pane.
    expect(id("macos", { key: "ArrowDown" }, "terminal")).toBeNull();
    // The tree's expand/collapse arrows map to expand/collapse there; in the git
    // pane the same arrows switch the rail view (Changes ⇄ History).
    expect(id("macos", { key: "ArrowRight" }, "tree")).toBe("tree.expand");
    expect(id("macos", { key: "ArrowRight" }, "git")).toBe("git.viewNext");
    expect(id("macos", { key: "ArrowLeft" }, "git")).toBe("git.viewPrev");
  });

  it("git Space/Enter/R/Backspace only match when the git pane is focused", () => {
    expect(id("macos", { key: " " }, "git")).toBe("git.stage");
    expect(id("macos", { key: "Enter" }, "git")).toBe("git.openDiff");
    expect(id("macos", { key: "r" }, "git")).toBe("git.refresh");
    expect(id("macos", { key: "Backspace" }, "git")).toBe("git.discard");
    expect(id("macos", { key: " " }, "tree")).toBe("tree.select.space");
    expect(id("macos", { key: "r" }, "terminal")).toBeNull();
  });

  it("git.refresh accepts Caps-Lock R but not Shift+R (combo forbids Shift)", () => {
    // RightRail routes refresh through matchBinding, so the bare `r` combo's
    // exact-shift rule governs: an uppercase key WITHOUT shiftKey (Caps Lock)
    // still resolves to git.refresh, but Shift+R (shiftKey true) does not.
    expect(id("macos", { key: "R" }, "git")).toBe("git.refresh");
    expect(id("macos", { key: "R", shiftKey: true }, "git")).toBeNull();
  });

  it("a modifier combo is pane-independent (fires from any focus)", () => {
    expect(id("macos", { key: "k", metaKey: true }, "tree")).toBe("palette.open");
    expect(id("macos", { key: "k", metaKey: true }, "git")).toBe("palette.open");
  });

  it("Cmd+Enter commit (modifier combo) is still gated to the git pane", () => {
    expect(id("macos", { key: "Enter", metaKey: true }, "git")).toBe("git.commit");
  });
});

describe("appCombos — xterm app pass-through list", () => {
  // A stable signature for a combo so we can assert set membership regardless of
  // ordering: key + the four (possible) modifier flags.
  const sig = (c: {
    key: string;
    primary?: boolean;
    ctrl?: boolean;
    shift?: boolean;
    alt?: boolean;
  }) =>
    [
      c.key,
      c.primary ? "P" : "",
      c.ctrl ? "C" : "",
      c.shift ? "S" : "",
      c.alt ? "A" : "",
    ].join("|");

  it("contains exactly PLAN.md's app combos (no pane-gated combos)", () => {
    const got = new Set(appCombos.map(sig));
    const want = new Set(
      [
        // Cmd+1..9
        ...["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((key) => ({
          key,
          primary: true,
        })),
        { key: "w", primary: true }, // tab.close
        { key: "t", primary: true }, // tab.new
        { key: "b", primary: true }, // toggle.left
        { key: "b", primary: true, alt: true }, // toggle.right
        { key: "e", primary: true, shift: true }, // focus.tree
        { key: "g", primary: true, shift: true }, // focus.git
        { key: "[", primary: true, shift: true }, // tab.prev
        { key: "]", primary: true, shift: true }, // tab.next
        { key: "`", ctrl: true }, // focus.terminal
        { key: "Tab", ctrl: true }, // tab.next.ctrl
        { key: "Tab", ctrl: true, shift: true }, // tab.prev.ctrl
        { key: "k", primary: true }, // palette.open
        { key: ",", primary: true }, // settings.toggle
      ].map(sig),
    );
    expect(got).toEqual(want);
  });

  it("excludes the pane-gated modifier combos (Cmd+Enter, Cmd+N)", () => {
    const got = new Set(appCombos.map(sig));
    // git.commit (Cmd+Enter, when="git") and tree.newWorktree (Cmd+N, when="tree").
    expect(got.has(sig({ key: "Enter", primary: true }))).toBe(false);
    expect(got.has(sig({ key: "n", primary: true }))).toBe(false);
  });
});

describe("matchBinding — no match", () => {
  it("returns null for an unbound key", () => {
    expect(match("macos", { key: "q", metaKey: true })).toBeNull();
    expect(match("windows", { key: "z" })).toBeNull();
  });
});
