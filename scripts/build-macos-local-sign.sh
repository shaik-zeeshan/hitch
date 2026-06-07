#!/bin/zsh

# Build the Hitch Tauri app and sign it locally on macOS with a real
# (Apple Development) code-signing identity so that the signed bundle's
# code identifier matches the real bundle id (com.hitch.desktop).
#
# Why this matters: a Tauri release built without a signing identity is
# adhoc/linker-signed with a synthetic identifier (e.g. hitch_desktop-XXXX),
# its Info.plist is not bound, and its resources are not sealed. macOS then
# refuses to deliver NSUserNotificationCenter / UNUserNotification
# notifications for it. Signing with a stable identity binds the Info.plist
# (so the registered bundle id is com.hitch.desktop) and seals resources,
# which is what lets native notifications work.
#
# Modeled on the mnema build-macos-local-sign.sh, adapted for Hitch:
#   - app lives under apps/desktop, Cargo workspace target is at the repo root
#   - bundle id com.hitch.desktop, app name Hitch.app
#   - the bundle ships sidecar binaries (hitch-daemon, hitch-hook) under
#     Contents/MacOS that must be signed too
#
# This is a LOCAL signing flow only: no notarization, no stapling.

set -euo pipefail

if [[ "${OSTYPE}" != darwin* ]]; then
  print -u2 "This script only runs on macOS."
  exit 1
fi

# ---------------------------------------------------------------------------
# Resolve a code-signing identity.
#
# Prefer an explicit APPLE_SIGNING_IDENTITY. Otherwise pick a real
# "Apple Development" identity (binds Info.plist, sets a team id). If none
# exists fall back to a self-signed identity, and as a last resort use an
# adhoc signature ("-"). Both fallbacks still bind the Info.plist and seal
# resources (unlike Tauri's default linker-signed output), which is enough
# for notification delivery against com.hitch.desktop.
# ---------------------------------------------------------------------------
identity="${APPLE_SIGNING_IDENTITY:-}"

if [[ -z "${identity}" ]]; then
  identity="$(security find-identity -v -p codesigning | grep 'Apple Development' | head -n 1 | sed -E 's/.*"(.*)"/\1/' || true)"
fi

if [[ -z "${identity}" ]]; then
  identity="$(security find-identity -v -p codesigning | grep -i 'self-signed' | head -n 1 | sed -E 's/.*"(.*)"/\1/' || true)"
fi

if [[ -z "${identity}" ]]; then
  print -u2 "No Apple Development or self-signed code-signing identity found; falling back to adhoc (-)."
  print -u2 "For a proper team-bound signature create an identity in Xcode:"
  print -u2 "  Settings > Accounts > Apple ID > Manage Certificates > + > Apple Development."
  print -u2 "Then re-run, or set APPLE_SIGNING_IDENTITY explicitly."
  identity="-"
fi

print "Using signing identity: ${identity}"

script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

# Hitch is a Cargo workspace, so the Tauri bundle lands under the workspace
# target dir at the repo root (NOT apps/desktop/src-tauri/target).
macos_bundle_dir="${repo_root}/target/release/bundle/macos"
dmg_dir="${repo_root}/target/release/bundle/dmg"
app_name="Hitch.app"
bundle_id="com.hitch.desktop"
install_dir="/Applications"
installed_app="${install_dir}/${app_name}"

# ---------------------------------------------------------------------------
# Build. Tauri will use APPLE_SIGNING_IDENTITY to sign during bundling, but we
# re-sign explicitly below to guarantee the whole bundle (including sidecars)
# is sealed under one identity.
# ---------------------------------------------------------------------------
cd "${repo_root}/apps/desktop"
CI=true APPLE_SIGNING_IDENTITY="${identity}" bun run tauri -- build

built_app="${macos_bundle_dir}/${app_name}"
if [[ ! -d "${built_app}" ]]; then
  print -u2 "Build succeeded, but ${built_app} was not found."
  exit 1
fi

# ---------------------------------------------------------------------------
# Re-sign the bundle, inside-out.
#
# Sign the nested sidecar/helper Mach-O binaries first, then the main
# executable, then the .app bundle as a whole. --force replaces Tauri's
# default signature; --options runtime applies the hardened runtime; we let
# codesign re-seal resources and bind the Info.plist. Using an explicit
# inside-out pass (rather than relying solely on --deep) is the supported,
# reliable way to sign nested code.
# ---------------------------------------------------------------------------
sign() {
  local target="$1"
  if [[ "${identity}" == "-" ]]; then
    codesign --force --timestamp=none --sign - "${target}"
  else
    codesign --force --timestamp --options runtime --sign "${identity}" "${target}"
  fi
}

print "Signing nested binaries..."
# Any Mach-O executables under Contents/MacOS other than nothing; sign helpers
# (hitch-daemon, hitch-hook) before the main binary.
for bin in "${built_app}/Contents/MacOS/hitch-daemon" "${built_app}/Contents/MacOS/hitch-hook"; do
  if [[ -f "${bin}" ]]; then
    print "  - ${bin:t}"
    sign "${bin}"
  fi
done

# Sign any frameworks / dylibs that may exist, just in case.
if [[ -d "${built_app}/Contents/Frameworks" ]]; then
  find "${built_app}/Contents/Frameworks" -type f \( -name "*.dylib" -o -perm -u+x \) -print0 |
    while IFS= read -r -d '' f; do
      print "  - ${f:t}"
      sign "${f}"
    done
fi

print "Signing main executable..."
sign "${built_app}/Contents/MacOS/hitch-desktop"

print "Signing app bundle..."
sign "${built_app}"

# ---------------------------------------------------------------------------
# Defend against the hardened-runtime library-validation crash class.
#
# We sign with --options runtime (hardened runtime), which enables library
# validation: the process may only load code signed by the same Team ID (or
# an Apple system library). If any Mach-O in the bundle dynamically links a
# Homebrew (/opt/homebrew) or /usr/local dylib — e.g. libssl/libcrypto pulled
# in transitively by libgit2 — dyld will abort the process at launch with
# "code signature ... not valid for use in process ... different Team IDs".
# That is exactly how hitch-daemon crashed before. Catch it HERE, at build
# time, with a clear message, instead of shipping a bundle that SIGABRTs on
# the user's machine.
# ---------------------------------------------------------------------------
print "Checking signed binaries for non-system dylib references..."
nonsystem_found=0
while IFS= read -r -d '' macho; do
  # Only inspect Mach-O files (otool errors on plain files / scripts).
  if ! file "${macho}" 2>/dev/null | grep -q "Mach-O"; then
    continue
  fi
  bad="$(otool -L "${macho}" 2>/dev/null | grep -E '/opt/homebrew|/usr/local' || true)"
  if [[ -n "${bad}" ]]; then
    nonsystem_found=1
    print -u2 "ERROR: ${macho#${built_app}/} links non-system dylibs (forbidden under hardened runtime):"
    print -u2 "${bad}"
  fi
done < <(find "${built_app}/Contents/MacOS" "${built_app}/Contents/Frameworks" -type f -print0 2>/dev/null)

if (( nonsystem_found )); then
  print -u2 ""
  print -u2 "Refusing to ship: at least one signed Mach-O links a Homebrew/usr-local"
  print -u2 "dylib. Hardened-runtime library validation will reject these at launch"
  print -u2 "(dyld SIGABRT: 'different Team IDs'). Statically link the dependency"
  print -u2 "(e.g. vendored OpenSSL) or drop the feature that pulls it in"
  print -u2 "(for git2: disable its 'https'/'ssh' default features), then rebuild."
  exit 1
fi

# ---------------------------------------------------------------------------
# Verify signature.
# ---------------------------------------------------------------------------
print "Verifying signature..."
codesign --verify --deep --strict --verbose=2 "${built_app}"
codesign -dv --verbose=2 "${built_app}" 2>&1 | grep -E "Identifier|Authority|Sealed|Info.plist|Signature|linker-signed" || true

bound_id="$(codesign -d -r- "${built_app}" 2>&1 | sed -nE 's/.*identifier "([^"]+)".*/\1/p' | head -n 1 || true)"
if [[ -n "${bound_id}" && "${bound_id}" != "${bundle_id}" ]]; then
  print -u2 "WARNING: signed identifier is '${bound_id}', expected '${bundle_id}'."
  print -u2 "Notifications may not be delivered. Check the bundle's Info.plist CFBundleIdentifier."
fi

# ---------------------------------------------------------------------------
# Install into /Applications, replacing any prior copy.
#
# Quit any running Hitch app/daemon first so we can replace the bundle and so
# stale processes from the OLD build don't keep serving the production socket.
# ---------------------------------------------------------------------------
print "Stopping any running Hitch processes..."
osascript -e 'tell application "Hitch" to quit' >/dev/null 2>&1 || true
# Kill the production daemon/desktop spawned from the installed bundle.
pkill -f "${installed_app}/Contents/MacOS/hitch-daemon" 2>/dev/null || true
pkill -f "${installed_app}/Contents/MacOS/hitch-desktop" 2>/dev/null || true
pkill -f "${installed_app}/Contents/MacOS/hitch-hook" 2>/dev/null || true

print "Installing to ${installed_app}..."
rm -rf "${installed_app}"
ditto "${built_app}" "${installed_app}"

# Register the freshly-installed bundle with Launch Services so the new
# signature/identifier is what the system sees.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
  -f "${installed_app}" >/dev/null 2>&1 || true

print "Installed signature:"
codesign -dv --verbose=2 "${installed_app}" 2>&1 | grep -E "Identifier|Authority|Sealed|Info.plist|Signature|linker-signed" || true

dmg_path="$(ls -t "${dmg_dir}"/*.dmg 2>/dev/null | head -n 1 || true)"
if [[ -n "${dmg_path}" ]]; then
  print "DMG also available at: ${dmg_path}"
fi

print ""
print "Done. Installed ${app_name} to ${install_dir}."
print "Launch with:  open \"${installed_app}\""
print "If Gatekeeper blocks first launch, right-click the app > Open, or run:"
print "  xattr -dr com.apple.quarantine \"${installed_app}\""
