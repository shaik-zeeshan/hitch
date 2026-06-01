import { describe, expect, it } from "vitest";
import { classifyTerminalKey } from "./terminalKeys";

// Minimal event shape the classifier reads; lets us assert routing without DOM.
const ev = (
  over: Partial<{
    type: string;
    metaKey: boolean;
    shiftKey: boolean;
    key: string;
  }> = {},
) => ({
  type: "keydown",
  metaKey: false,
  shiftKey: false,
  key: "",
  ...over,
});

describe("classifyTerminalKey", () => {
  it("classifies Cmd+V as pass — paste is NOT special, it falls through to native xterm", () => {
    // The load-bearing assertion for slice 1: there is no manual paste action,
    // so the keyboard paste path cannot double-fire.
    expect(classifyTerminalKey(ev({ metaKey: true, key: "v" }))).toBe("pass");
  });

  it("classifies Cmd+C as copy (caller still gates on hasSelection)", () => {
    expect(classifyTerminalKey(ev({ metaKey: true, key: "c" }))).toBe("copy");
  });

  it("classifies Cmd+F as search", () => {
    expect(classifyTerminalKey(ev({ metaKey: true, key: "f" }))).toBe("search");
  });

  it("classifies Shift+Enter (keydown) as newline", () => {
    expect(classifyTerminalKey(ev({ shiftKey: true, key: "Enter" }))).toBe(
      "newline",
    );
  });

  it("suppresses non-keydown phases of Shift+Enter", () => {
    expect(
      classifyTerminalKey(ev({ type: "keyup", shiftKey: true, key: "Enter" })),
    ).toBe("suppress");
    expect(
      classifyTerminalKey(
        ev({ type: "keypress", shiftKey: true, key: "Enter" }),
      ),
    ).toBe("suppress");
  });

  it("passes Cmd shortcuts on non-keydown phases through", () => {
    expect(
      classifyTerminalKey(ev({ type: "keyup", metaKey: true, key: "c" })),
    ).toBe("pass");
  });

  it("never intercepts Ctrl combos (Ctrl+C stays SIGINT)", () => {
    expect(classifyTerminalKey(ev({ metaKey: false, key: "c" }))).toBe("pass");
  });

  it("passes unrelated keys through", () => {
    expect(classifyTerminalKey(ev({ metaKey: true, key: "a" }))).toBe("pass");
    expect(classifyTerminalKey(ev({ key: "x" }))).toBe("pass");
  });
});
