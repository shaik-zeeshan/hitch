import { describe, expect, it } from "vitest";
import {
  desktopPlatformFromPlatform,
  revealItemLabel,
  isShortcutModifier,
  shortcutLabel,
  shellSessionShortcutLabel,
} from "./desktopPlatform";

describe("desktopPlatformFromPlatform", () => {
  it("normalizes browser platform strings", () => {
    expect(desktopPlatformFromPlatform("MacIntel")).toBe("macos");
    expect(desktopPlatformFromPlatform("Win32")).toBe("windows");
    expect(desktopPlatformFromPlatform("Linux x86_64")).toBe("linux");
    expect(desktopPlatformFromPlatform(undefined)).toBe("other");
  });
});

describe("revealItemLabel", () => {
  it("uses platform-specific file manager wording", () => {
    expect(revealItemLabel("macos")).toBe("Reveal in Finder");
    expect(revealItemLabel("windows")).toBe("Show in Explorer");
    expect(revealItemLabel("linux")).toBe("Show in file manager");
    expect(revealItemLabel("other")).toBe("Show in file manager");
  });
});

describe("shellSessionShortcutLabel", () => {
  it("uses Command on macOS and Control elsewhere", () => {
    expect(shellSessionShortcutLabel("macos")).toBe("⌘T");
    expect(shellSessionShortcutLabel("windows")).toBe("Ctrl+T");
    expect(shellSessionShortcutLabel("linux")).toBe("Ctrl+T");
    expect(shellSessionShortcutLabel("other")).toBe("Ctrl+T");
  });
});

describe("shortcutLabel", () => {
  it("formats platform shortcut labels", () => {
    expect(shortcutLabel("macos", "K")).toBe("⌘K");
    expect(shortcutLabel("windows", "K")).toBe("Ctrl+K");
    expect(shortcutLabel("linux", "Enter")).toBe("Ctrl+Enter");
  });
});

describe("isShortcutModifier", () => {
  it("uses Command on macOS and Control elsewhere", () => {
    expect(isShortcutModifier({ metaKey: true, ctrlKey: false }, "macos")).toBe(true);
    expect(isShortcutModifier({ metaKey: false, ctrlKey: true }, "macos")).toBe(false);
    expect(isShortcutModifier({ metaKey: false, ctrlKey: true }, "windows")).toBe(true);
    expect(isShortcutModifier({ metaKey: true, ctrlKey: false }, "windows")).toBe(false);
  });
});
