// Unit tests for the remote file-drop UPLOAD flow (issue #31, ADR 0014): a drop
// onto a remote Session streams the files to the remote daemon, then inserts the
// returned ACTUAL remote paths with remote-platform quoting; a cancelled batch
// inserts nothing; dropped directories are rejected with explicit copy; local
// drops are byte-identical (covered by fileDrop.test.ts + remoteSessions.test.ts).
//
// `./daemon` is mocked so scope routing + `sendInput` are controllable without the
// live transport; the Tauri `invoke`/`listen` surface and the toast surface are
// mocked too so the module loads headless.

import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

// `listen` registers the progress listener; capture the handler so a test can
// drive progress ticks. Returns an unlisten fn.
let progressHandler: ((event: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: (e: { payload: unknown }) => void) => {
    progressHandler = handler;
    return () => {
      progressHandler = null;
    };
  }),
}));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
}));

// The toast surface: a callable default with methods (svelte-french-toast shape).
// Built via `vi.hoisted` so the mock factory (hoisted to top) can reference it,
// and every method is a spy so the tests can assert toast copy.
const toast = vi.hoisted(() => {
  const callable = Object.assign(vi.fn(), {
    error: vi.fn(),
    success: vi.fn(),
    loading: vi.fn(() => "toast-id"),
    dismiss: vi.fn(),
  });
  return callable;
});
vi.mock("svelte-french-toast", () => ({ default: toast }));

// Control scope routing + capture inserted input.
const sendInputMock = vi.fn();
let sessionScope = "ssh:prod";
vi.mock("./daemon", () => ({
  scopeForSession: () => sessionScope,
  scopeLabel: (scope: string) => (scope === "local" ? "LOCAL" : scope.replace("ssh:", "")),
  sendInput: (...args: unknown[]) => sendInputMock(...args),
}));

import { handleDropForTest } from "./fileDrop";

type FileResult =
  | { type: "uploaded"; name: string; remotePath: string }
  | { type: "rejected-directory"; name: string }
  | { type: "failed"; name: string; error: string };

function batch(files: FileResult[], osFamily: "unix" | "windows", cancelled = false) {
  return { osFamily, cancelled, files };
}

// Run a drop and let the async upload chain settle (a few microtask ticks).
async function drop(sessionId: string, paths: string[]) {
  handleDropForTest(sessionId, paths);
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

beforeEach(() => {
  invokeMock.mockReset();
  sendInputMock.mockReset();
  toast.error.mockReset();
  toast.success.mockReset();
  toast.loading.mockReset();
  toast.loading.mockReturnValue("toast-id");
  toast.dismiss.mockReset();
  toast.mockReset();
  progressHandler = null;
  sessionScope = "ssh:prod";
});

describe("successful remote upload", () => {
  it("inserts the actual remote paths POSIX-quoted for a unix remote", async () => {
    invokeMock.mockResolvedValue(
      batch(
        [
          { type: "uploaded", name: "a b.txt", remotePath: "/home/dev/.hitch/uploads/s/a b.txt" },
          { type: "uploaded", name: "c.txt", remotePath: "/home/dev/.hitch/uploads/s/c.txt" },
        ],
        "unix",
      ),
    );

    await drop("rs1", ["/local/a b.txt", "/local/c.txt"]);

    // The uploaded remote paths are inserted, POSIX-escaped (space backslashed),
    // space-separated, trailing space — the same shape as a local drop.
    expect(sendInputMock).toHaveBeenCalledWith(
      "rs1",
      "/home/dev/.hitch/uploads/s/a\\ b.txt /home/dev/.hitch/uploads/s/c.txt ",
    );
  });

  it("quotes inserted paths for a WINDOWS remote regardless of the GUI platform", async () => {
    invokeMock.mockResolvedValue(
      batch(
        [
          {
            type: "uploaded",
            name: "my file.txt",
            remotePath: "C:\\Users\\dev\\.hitch\\uploads\\s\\my file.txt",
          },
        ],
        "windows",
      ),
    );

    await drop("rs1", ["/local/my file.txt"]);

    // Windows quoting: a path with a space is double-quoted, not backslash-escaped.
    expect(sendInputMock).toHaveBeenCalledWith(
      "rs1",
      '"C:\\Users\\dev\\.hitch\\uploads\\s\\my file.txt" ',
    );
  });

  it("calls the upload command with a unique batch id and the session scope", async () => {
    invokeMock.mockResolvedValue(batch([], "unix"));
    await drop("rs1", ["/local/x"]);
    const call = invokeMock.mock.calls.find(([c]) => c === "upload_files_to_session");
    expect(call).toBeTruthy();
    expect(call?.[1]).toMatchObject({ scope: "ssh:prod", sessionId: "rs1", paths: ["/local/x"] });
    expect(typeof (call?.[1] as { batchId: string }).batchId).toBe("string");
  });
});

describe("cancelled remote upload", () => {
  it("inserts nothing when the batch reports cancelled", async () => {
    invokeMock.mockResolvedValue(batch([], "unix", /* cancelled */ true));
    await drop("rs1", ["/local/big.bin"]);
    expect(sendInputMock).not.toHaveBeenCalled();
  });
});

describe("directory drops", () => {
  it("rejects a dropped directory with explicit recursive-upload copy and inserts the files", async () => {
    invokeMock.mockResolvedValue(
      batch(
        [
          { type: "rejected-directory", name: "src" },
          { type: "uploaded", name: "ok.txt", remotePath: "/home/dev/.hitch/uploads/s/ok.txt" },
        ],
        "unix",
      ),
    );

    await drop("rs1", ["/local/src", "/local/ok.txt"]);

    // The file is still inserted; the directory triggers the explicit toast.
    expect(sendInputMock).toHaveBeenCalledWith("rs1", "/home/dev/.hitch/uploads/s/ok.txt ");
    expect(toast.error).toHaveBeenCalledWith(
      expect.stringContaining("recursive upload isn't supported"),
      expect.anything(),
    );
  });
});

describe("local drop regression", () => {
  it("inserts local paths directly without invoking the upload command", async () => {
    sessionScope = "local";
    await drop("ls1", ["/Users/me/file.txt"]);
    expect(sendInputMock).toHaveBeenCalledWith("ls1", "/Users/me/file.txt ");
    expect(
      invokeMock.mock.calls.some(([c]) => c === "upload_files_to_session"),
    ).toBe(false);
  });
});
