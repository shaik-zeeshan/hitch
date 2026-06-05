import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (isMain()) {
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const desktopRoot = resolve(scriptDir, "..");
  const workspaceRoot = resolve(desktopRoot, "../..");
  const daemonPath = debugDaemonPath(workspaceRoot, process.platform);

  if (process.platform === "win32") {
    const result = stopWindowsDebugDaemon(daemonPath);
    if (result.error) {
      console.error(result.error.message);
      process.exit(1);
    }
    if (result.status !== 0) {
      process.exit(result.status ?? 1);
    }
  }
}

function isMain() {
  return process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
}

/**
 * @param {string} root
 * @param {NodeJS.Platform} platform
 * @param {string | undefined} [cargoTargetDir]
 */
export function debugDaemonPath(root, platform, cargoTargetDir = process.env.CARGO_TARGET_DIR) {
  // Mirror build.rs: honor CARGO_TARGET_DIR (absolute, or relative to the
  // workspace root) so a custom target dir is still found and stopped before
  // a rebuild reuses the in-use exe. Falls back to the default `target/`.
  const targetDir = cargoTargetDir && cargoTargetDir.length > 0 ? resolve(root, cargoTargetDir) : resolve(root, "target");
  return resolve(targetDir, "debug", platform === "win32" ? "hitch-daemon.exe" : "hitch-daemon");
}

/**
 * @param {string} targetPath
 */
export function stopWindowsDebugDaemon(targetPath) {
  return spawnSync("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", windowsStopCommand()], {
    env: { ...process.env, HITCH_DEV_DAEMON_EXE: targetPath },
    stdio: "inherit",
  });
}

export function windowsStopCommand() {
  return String.raw`
$ErrorActionPreference = 'Stop'
$target = [System.IO.Path]::GetFullPath($env:HITCH_DEV_DAEMON_EXE)
$matches = @(Get-Process -Name 'hitch-daemon' -ErrorAction SilentlyContinue | Where-Object {
  $_.Path -and ([System.IO.Path]::GetFullPath($_.Path) -ieq $target)
})
if ($matches.Count -eq 0) {
  exit 0
}
$ids = @($matches | ForEach-Object { $_.Id })
Write-Host ("Stopping existing debug hitch-daemon: " + ($ids -join ', '))
$matches | Stop-Process -Force
$deadline = (Get-Date).AddSeconds(10)
do {
  Start-Sleep -Milliseconds 100
  $remaining = @(Get-Process -Id $ids -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and ([System.IO.Path]::GetFullPath($_.Path) -ieq $target)
  })
} while ($remaining.Count -ne 0 -and (Get-Date) -lt $deadline)
if ($remaining.Count -ne 0) {
  throw ("Timed out waiting for debug hitch-daemon to exit: " + (($remaining | ForEach-Object { $_.Id }) -join ', '))
}
`;
}
