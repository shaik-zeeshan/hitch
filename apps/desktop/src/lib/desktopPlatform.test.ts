import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  desktopPlatformFromPlatform,
  revealItemLabel,
  isShortcutModifier,
  shortcutLabel,
  shortcutKeys,
  shellSessionShortcutLabel,
} from "./desktopPlatform";
import { get } from "svelte/store";

class LocalStorageStub {
  readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("editorApp settings migration", () => {
  async function loadEditorApp(stored: string) {
    const storage = new LocalStorageStub();
    storage.values.set("hitch.editorApp", stored);
    vi.stubGlobal("localStorage", storage);

    const settings = await import("./settings");

    return { storage, settings };
  }

  it("normalizes the legacy Visual Studio Code default to system default", async () => {
    const { storage, settings } = await loadEditorApp("Visual Studio Code");

    expect(get(settings.editorApp)).toBe(settings.SYSTEM_DEFAULT_EDITOR);
    expect(storage.values.get("hitch.editorApp")).toBe(settings.SYSTEM_DEFAULT_EDITOR);
  });

  it("keeps explicit non-default editor values intact", async () => {
    for (const stored of ["Cursor", "Zed", "/Applications/Custom Editor.app"]) {
      const { storage, settings } = await loadEditorApp(stored);

      expect(get(settings.editorApp)).toBe(stored);
      expect(storage.values.get("hitch.editorApp")).toBe(stored);
      vi.resetModules();
    }
  });
});

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

describe("shortcutKeys", () => {
  it("splits the shortcut into one entry per keycap", () => {
    expect(shortcutKeys("macos", "K")).toEqual(["⌘", "K"]);
    expect(shortcutKeys("windows", "K")).toEqual(["Ctrl", "K"]);
    expect(shortcutKeys("linux", "Enter")).toEqual(["Ctrl", "Enter"]);
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
