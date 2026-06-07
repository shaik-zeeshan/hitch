import { afterEach, describe, expect, it, vi } from "vitest";

import { cssEscape } from "./cssEscape";

// Pins the SSR/test guard: cssEscape delegates to CSS.escape in the webview but
// must fall back to the raw value when the global CSS object is missing (jsdom/SSR).
describe("cssEscape", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("delegates to CSS.escape when available", () => {
    const escape = vi.fn((v: string) => `escaped:${v}`);
    vi.stubGlobal("CSS", { escape });
    expect(cssEscape("a.b")).toBe("escaped:a.b");
    expect(escape).toHaveBeenCalledWith("a.b");
  });

  it("returns the raw value when CSS is undefined", () => {
    vi.stubGlobal("CSS", undefined);
    expect(cssEscape("a.b")).toBe("a.b");
  });
});
