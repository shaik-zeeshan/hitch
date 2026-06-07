import { describe, expect, it } from "vitest";
import { classifyTerminalKey, type TerminalShortcutPlatform } from "./terminalKeys";

// Minimal event shape the classifier reads; lets us assert routing without DOM.
const ev = (
  over: Partial<{
    type: string;
    metaKey: boolean;
    ctrlKey: boolean;
    shiftKey: boolean;
    altKey: boolean;
    key: string;
  }> = {},
) => ({
  type: "keydown",
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  key: "",
  ...over,
});

const classify = (
  platform: TerminalShortcutPlatform,
  over: Parameters<typeof ev>[0] = {},
) => classifyTerminalKey(ev(over), platform);

describe("classifyTerminalKey", () => {
  it("classifies macOS Cmd+C as copy (caller still gates on hasSelection)", () => {
    expect(classify("macos", { metaKey: true, key: "c" })).toBe("copy");
  });

  it("classifies macOS Cmd+F as search", () => {
    expect(classify("macos", { metaKey: true, key: "f" })).toBe("search");
  });

  it("passes plain non-mac Ctrl+C through so it interrupts the PTY", () => {
    expect(classify("windows", { ctrlKey: true, key: "c" })).toBe("pass");
    expect(classify("linux", { ctrlKey: true, key: "c" })).toBe("pass");
  });

  it("classifies non-mac Ctrl+Shift+C as copy", () => {
    expect(classify("windows", { ctrlKey: true, shiftKey: true, key: "C" })).toBe(
      "copy",
    );
    expect(classify("linux", { ctrlKey: true, shiftKey: true, key: "C" })).toBe(
      "copy",
    );
  });

  it("passes plain non-mac Ctrl+F through to the PTY", () => {
    expect(classify("windows", { ctrlKey: true, key: "f" })).toBe("pass");
    expect(classify("linux", { ctrlKey: true, key: "f" })).toBe("pass");
  });

  it("classifies non-mac Ctrl+Shift+F as search", () => {
    expect(classify("windows", { ctrlKey: true, shiftKey: true, key: "F" })).toBe(
      "search",
    );
    expect(classify("linux", { ctrlKey: true, shiftKey: true, key: "F" })).toBe(
      "search",
    );
  });

  it("does not treat Windows Meta/Cmd as the shortcut modifier", () => {
    expect(classify("windows", { metaKey: true, key: "c" })).toBe("pass");
    expect(classify("windows", { metaKey: true, key: "f" })).toBe("pass");
  });

  it("passes paste shortcuts through on macOS and Windows", () => {
    expect(classify("macos", { metaKey: true, key: "v" })).toBe("pass");
    expect(classify("windows", { ctrlKey: true, key: "v" })).toBe("pass");
  });

  it("classifies Shift+Enter (keydown) as newline", () => {
    expect(classify("macos", { shiftKey: true, key: "Enter" })).toBe(
      "newline",
    );
  });

  it("suppresses non-keydown phases of Shift+Enter", () => {
    expect(
      classify("macos", { type: "keyup", shiftKey: true, key: "Enter" }),
    ).toBe("suppress");
    expect(
      classify("macos", { type: "keypress", shiftKey: true, key: "Enter" }),
    ).toBe("suppress");
  });

  it("passes shortcut keys on non-keydown phases through", () => {
    expect(classify("macos", { type: "keyup", metaKey: true, key: "c" })).toBe(
      "pass",
    );
    expect(
      classify("windows", { type: "keyup", ctrlKey: true, key: "f" }),
    ).toBe("pass");
  });

  it("passes unrelated keys through", () => {
    expect(classify("macos", { metaKey: true, key: "a" })).toBe("pass");
    expect(classify("windows", { key: "x" })).toBe("pass");
  });
});

describe("classifyTerminalKey — app pass-through (keymap-derived)", () => {
  // Every app-level combo must classify as "app" on the right platform so
  // attachCustomKeyEventHandler returns false and xterm never processes it. The
  // list is derived from keymap.ts's appCombos, so this guards against drift.
  const appMac = [
    { key: "1", metaKey: true },
    { key: "9", metaKey: true },
    { key: "w", metaKey: true },
    { key: "t", metaKey: true },
    { key: "b", metaKey: true },
    { key: "b", metaKey: true, altKey: true },
    { key: "e", metaKey: true, shiftKey: true },
    { key: "g", metaKey: true, shiftKey: true },
    // Shift held → browser reports the shifted glyph in event.key, so
    // Cmd+Shift+[ arrives as "{" and Cmd+Shift+] as "}" (tab.prev/tab.next).
    { key: "{", metaKey: true, shiftKey: true },
    { key: "}", metaKey: true, shiftKey: true },
    { key: "k", metaKey: true },
    { key: ",", metaKey: true },
    // Ctrl-on-both combos:
    { key: "`", ctrlKey: true },
    { key: "Tab", ctrlKey: true },
    { key: "Tab", ctrlKey: true, shiftKey: true },
    // NOTE: Cmd+Enter (git.commit) and Cmd+N (tree.newWorktree) are pane-gated
    // (when="git"/"tree"), so they are deliberately NOT app pass-through — see
    // the "pane-gated combos pass through" test below.
  ];

  it("classifies every macOS app combo as 'app'", () => {
    for (const over of appMac) {
      expect(classify("macos", over)).toBe("app");
    }
  });

  it("classifies every Windows/Linux app combo as 'app' (Cmd→Ctrl)", () => {
    // Map metaKey→ctrlKey for the platform-primary combos; Ctrl-on-both combos
    // already use ctrlKey and stay as-is (drop the absent metaKey rather than
    // forwarding `undefined`, which would override the ev() default of false).
    const appOther = appMac.map(({ metaKey, ...rest }) =>
      metaKey ? { ...rest, ctrlKey: true } : rest,
    );
    for (const over of appOther) {
      expect(classify("windows", over)).toBe("app");
      expect(classify("linux", over)).toBe("app");
    }
  });

  it("does NOT classify terminal-internal copy/search/paste as 'app'", () => {
    // These are NOT keymap bindings — they must keep their own classification so
    // the terminal owns copy/search/paste.
    expect(classify("macos", { metaKey: true, key: "c" })).toBe("copy");
    expect(classify("macos", { metaKey: true, key: "f" })).toBe("search");
    expect(classify("macos", { metaKey: true, key: "v" })).toBe("pass");
    expect(classify("windows", { ctrlKey: true, key: "v" })).toBe("pass");
  });

  it("does NOT classify pane-gated combos (Cmd+Enter, Cmd+N) as 'app'", () => {
    // git.commit (Cmd+Enter, when="git") and tree.newWorktree (Cmd+N,
    // when="tree") are pane-gated: the layout dispatcher acts on them only
    // inside their pane (and no-ops otherwise). They must reach the PTY when
    // the terminal is focused — they are NOT app pass-through.
    expect(classify("macos", { metaKey: true, key: "Enter" })).toBe("pass");
    expect(classify("macos", { metaKey: true, key: "n" })).toBe("pass");
    expect(classify("windows", { ctrlKey: true, key: "Enter" })).toBe("pass");
    expect(classify("linux", { ctrlKey: true, key: "n" })).toBe("pass");
  });

  it("does not consume app combos off the keydown phase", () => {
    expect(classify("macos", { type: "keyup", metaKey: true, key: "1" })).toBe("pass");
  });

  it("still lets plain keys reach the PTY", () => {
    expect(classify("macos", { key: "a" })).toBe("pass");
    expect(classify("macos", { key: "1" })).toBe("pass");
    expect(classify("windows", { key: "Tab" })).toBe("pass");
  });

  it("handles DOM-faithful events whose fields are prototype getters", () => {
    // A real KeyboardEvent exposes key/metaKey/… as PROTOTYPE getters, not
    // own-enumerable props — so `{ ...event }` copies nothing. The classifier
    // once spread the event to default altKey and silently matched against
    // `key: undefined`, throwing inside xterm's keydown handler for every key
    // and breaking keydown-only keys (Backspace, Ctrl+C) in the live app while
    // plain-object tests stayed green. Assert against an event shaped like the
    // real thing: fields on the prototype, nothing own-enumerable.
    const domEv = (fields: ReturnType<typeof ev>) => {
      const proto = {};
      for (const [k, v] of Object.entries(fields)) {
        Object.defineProperty(proto, k, { get: () => v });
      }
      return Object.create(proto) as ReturnType<typeof ev>;
    };
    expect(classifyTerminalKey(domEv(ev({ key: "Backspace" })), "macos")).toBe("pass");
    expect(classifyTerminalKey(domEv(ev({ key: "c", ctrlKey: true })), "macos")).toBe("pass");
    expect(classifyTerminalKey(domEv(ev({ key: "w", metaKey: true })), "macos")).toBe("app");
    expect(
      classifyTerminalKey(domEv(ev({ key: "b", metaKey: true, altKey: true })), "macos"),
    ).toBe("app");
  });
});
