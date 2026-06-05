import { describe, expect, it } from "vitest";
import { classifyTerminalKey, type TerminalShortcutPlatform } from "./terminalKeys";

// Minimal event shape the classifier reads; lets us assert routing without DOM.
const ev = (
  over: Partial<{
    type: string;
    metaKey: boolean;
    ctrlKey: boolean;
    shiftKey: boolean;
    key: string;
  }> = {},
) => ({
  type: "keydown",
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
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
