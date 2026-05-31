import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const desktopRoot = resolve(scriptDir, "..");
const workspaceRoot = resolve(desktopRoot, "../..");
const targetTriple = process.env.CARGO_BUILD_TARGET || hostTriple();
const explicitTarget = Boolean(process.env.CARGO_BUILD_TARGET);
const isWindowsTarget = targetTriple.includes("windows");

const cargoArgs = ["build", "-p", "hitch-daemon", "--release"];
if (explicitTarget) {
  cargoArgs.push("--target", targetTriple);
}

const build = spawnSync("cargo", cargoArgs, {
  cwd: workspaceRoot,
  stdio: "inherit",
});
if (build.error) {
  console.error(build.error.message);
}
if (build.status !== 0) {
  process.exit(build.status ?? 1);
}

const exe = isWindowsTarget ? "hitch-daemon.exe" : "hitch-daemon";
const source = resolve(
  workspaceRoot,
  "target",
  ...(explicitTarget ? [targetTriple] : []),
  "release",
  exe,
);
const sidecarName = `hitch-daemon-${targetTriple}${isWindowsTarget ? ".exe" : ""}`;
const destinationDir = resolve(desktopRoot, "src-tauri/binaries");
const destination = resolve(destinationDir, sidecarName);

mkdirSync(destinationDir, { recursive: true });
copyFileSync(source, destination);
chmodSync(destination, statSync(source).mode & 0o777);
console.log(`Prepared daemon sidecar: ${destination}`);

function hostTriple() {
  switch (`${process.platform}:${process.arch}`) {
    case "darwin:arm64":
      return "aarch64-apple-darwin";
    case "darwin:x64":
      return "x86_64-apple-darwin";
    case "linux:arm64":
      return "aarch64-unknown-linux-gnu";
    case "linux:x64":
      return "x86_64-unknown-linux-gnu";
    case "win32:arm64":
      return "aarch64-pc-windows-msvc";
    case "win32:x64":
      return "x86_64-pc-windows-msvc";
    default:
      throw new Error(
        `Unsupported host platform for daemon sidecar: ${process.platform}/${process.arch}. ` +
          "Set CARGO_BUILD_TARGET to the Rust target triple.",
      );
  }
}
