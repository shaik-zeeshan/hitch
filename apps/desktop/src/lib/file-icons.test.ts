import { describe, expect, it } from "vitest";

import { fileIconName, fileIconUrl } from "./file-icons";

// The resolver delegates precedence to vscode-material-icons' getIconForFilePath
// (exact file name → file suffix → extension → language → generic "file"). These
// tests pin the behaviour we rely on through our thin wrapper: basename handling,
// compound-extension precedence, well-known names, a never-throw generic fallback,
// and that every resolved name maps to a real emitted asset URL.
describe("fileIconName", () => {
  it("resolves common language extensions", () => {
    expect(fileIconName("src/app.ts")).toBe("typescript");
    expect(fileIconName("util.js")).toBe("javascript");
    expect(fileIconName("deep/nested/path/app.rs")).toBe("rust");
    expect(fileIconName("App.svelte")).toBe("svelte");
  });

  it("keys off the basename, ignoring the directory", () => {
    expect(fileIconName("a/b/c/package.json")).toBe(fileIconName("package.json"));
    expect(fileIconName("deep/nested/path/app.rs")).toBe(fileIconName("app.rs"));
  });

  it("matches an exact file name before its extension", () => {
    // package.json has a dedicated glyph (nodejs), distinct from the generic
    // .json glyph — proving exact-name wins over the extension fallback.
    expect(fileIconName("package.json")).toBe("nodejs");
    expect(fileIconName("package.json")).not.toBe(fileIconName("data.json"));
    // tsconfig.json likewise resolves to its own mark, not plain typescript.
    expect(fileIconName("tsconfig.json")).toBe("tsconfig");
  });

  it("prefers the longer compound suffix over the bare extension", () => {
    // .d.ts and .test.ts each have dedicated glyphs distinct from plain .ts —
    // the library matches the full suffix before falling back to the extension.
    expect(fileIconName("mod.ts")).toBe("typescript");
    expect(fileIconName("mod.d.ts")).toBe("typescript-def");
    expect(fileIconName("mod.test.ts")).toBe("test-ts");
    expect(fileIconName("mod.d.ts")).not.toBe(fileIconName("mod.ts"));
  });

  it("handles dotfiles and well-known names", () => {
    expect(fileIconName(".gitignore")).toBe("git");
    expect(fileIconName("Dockerfile")).toBe("docker");
    expect(fileIconName("Makefile")).toBe("makefile");
  });

  it("resolves images and lockfiles to their own glyphs", () => {
    expect(fileIconName("photo.webp")).toBe("image");
    expect(fileIconName("Cargo.lock")).toBe("lock");
    expect(fileIconName("bun.lockb")).toBe("bun");
  });

  it("falls back to the generic file glyph for unknown types", () => {
    expect(fileIconName("mystery.qqzz")).toBe("file");
    expect(fileIconName("noextension")).toBe("file");
  });
});

// A usable SVG asset reference: either an emitted (hashed) `.svg` file URL in a
// production build, or a `data:image/svg+xml,…` URI when Vite inlines the small
// asset (dev/test, and any asset under assetsInlineLimit). Both are valid <img src>.
function isSvgAssetUrl(url: string): boolean {
  return /\.svg(\?|$)/.test(url) || url.startsWith("data:image/svg+xml");
}

describe("fileIconUrl", () => {
  it("returns a usable SVG asset reference for known and unknown types", () => {
    for (const path of ["src/app.ts", "app.rs", "App.svelte", "mystery.qqzz"]) {
      const url = fileIconUrl(path);
      expect(typeof url).toBe("string");
      expect(url.length).toBeGreaterThan(0);
      expect(isSvgAssetUrl(url)).toBe(true);
    }
  });

  it("maps the same resolved icon name to the same URL", () => {
    // .d.ts and a plain .ts resolve to different names → different URLs.
    expect(fileIconUrl("mod.d.ts")).not.toBe(fileIconUrl("mod.ts"));
    // basename-only resolution → identical URL regardless of directory.
    expect(fileIconUrl("a/b/package.json")).toBe(fileIconUrl("package.json"));
  });

  it("never throws, even for odd input, and still yields the generic glyph", () => {
    expect(() => fileIconUrl("")).not.toThrow();
    expect(() => fileIconUrl("...")).not.toThrow();
    // Empty / dot-only paths resolve to the generic "file" glyph (same URL).
    expect(fileIconUrl("")).toBe(fileIconUrl("noextension"));
    expect(isSvgAssetUrl(fileIconUrl(""))).toBe(true);
  });
});
