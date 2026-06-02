import { describe, expect, it } from "vitest";
import { resolve } from "node:path";

import { debugDaemonPath, windowsStopCommand } from "../../scripts/stop-dev-daemon.mjs";

describe("stop-dev-daemon script", () => {
  it("targets the debug daemon executable for the host platform", () => {
    expect(debugDaemonPath("C:/repo/hitch", "win32")).toBe(resolve("C:/repo/hitch", "target", "debug", "hitch-daemon.exe"));
    expect(debugDaemonPath("/repo/hitch", "linux")).toBe(resolve("/repo/hitch", "target", "debug", "hitch-daemon"));
  });

  it("passes the target path through the environment instead of interpolating it into PowerShell", () => {
    const command = windowsStopCommand();

    expect(command).toContain("$env:HITCH_DEV_DAEMON_EXE");
    expect(command).toContain("Get-Process -Name 'hitch-daemon'");
    expect(command).toContain("Stop-Process -Force");
    expect(command).not.toContain("target\\debug\\hitch-daemon.exe");
  });
});
