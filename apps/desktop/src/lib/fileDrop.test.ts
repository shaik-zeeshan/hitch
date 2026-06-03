import { describe, expect, it } from "vitest";
import { formatDroppedPaths } from "./fileDrop";

// Pure path-formatting only (the listener wiring needs a live webview/DOM and is
// exercised by hand). The `windows` arg is passed explicitly so the same test
// asserts both platforms regardless of where it runs.
describe("formatDroppedPaths", () => {
  describe("posix", () => {
    const fmt = (paths: string[]) => formatDroppedPaths(paths, false);

    it("appends a trailing space to a plain path", () => {
      expect(fmt(["/home/user/file.txt"])).toBe("/home/user/file.txt ");
    });

    it("backslash-escapes spaces", () => {
      expect(fmt(["/home/user/my file.txt"])).toBe(
        "/home/user/my\\ file.txt ",
      );
    });

    it("escapes shell metacharacters so paths can't execute", () => {
      expect(fmt(["/tmp/$(rm -rf ~).txt"])).toBe(
        "/tmp/\\$\\(rm\\ -rf\\ \\~\\).txt ",
      );
    });

    it("leaves safe path characters unescaped", () => {
      expect(fmt(["/a-b_c.d/e+f@g%h=i,j:k"])).toBe(
        "/a-b_c.d/e+f@g%h=i,j:k ",
      );
    });

    it("space-separates multiple paths", () => {
      expect(fmt(["/a/b", "/c d"])).toBe("/a/b /c\\ d ");
    });
  });

  describe("windows", () => {
    const fmt = (paths: string[]) => formatDroppedPaths(paths, true);

    it("leaves a plain path bare (backslashes are separators)", () => {
      expect(fmt(["C:\\Users\\me\\file.txt"])).toBe("C:\\Users\\me\\file.txt ");
    });

    it("double-quotes a path containing spaces", () => {
      expect(fmt(["C:\\Users\\me\\my file.txt"])).toBe(
        '"C:\\Users\\me\\my file.txt" ',
      );
    });

    it("double-quotes paths with cmd-special characters", () => {
      expect(fmt(["C:\\tmp\\a(b)&c.txt"])).toBe('"C:\\tmp\\a(b)&c.txt" ');
    });

    it("space-separates multiple paths, quoting only those that need it", () => {
      expect(fmt(["C:\\a\\b.txt", "C:\\c d\\e.txt"])).toBe(
        'C:\\a\\b.txt "C:\\c d\\e.txt" ',
      );
    });
  });

  it("formats an empty list to just a trailing space", () => {
    // The listener guards against empty drops before calling this, but keep the
    // function total rather than relying on the caller.
    expect(formatDroppedPaths([], false)).toBe(" ");
  });
});
