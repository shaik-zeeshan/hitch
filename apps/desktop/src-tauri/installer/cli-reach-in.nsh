; cli-reach-in.nsh — Hitch Windows CLI reach-in (NSIS installer hooks).
;
; Wired from tauri.conf.json via bundle.windows.nsis.installerHooks. Tauri's
; NSIS template invokes the macros NSIS_HOOK_POSTINSTALL (after files are laid
; down in $INSTDIR) and NSIS_HOOK_POSTUNINSTALL (after files are removed).
;
; What this owns (the runtime cli_install.rs is status-only on Windows):
;   1. Copy $INSTDIR\hitch-daemon.exe -> $INSTDIR\hitch.exe. The daemon and
;      hitch-hook.exe sidecars ship adjacent via Tauri externalBin, so the
;      single CLI entrypoint is just a copy of the daemon next to its hook.
;   2. Append $INSTDIR to the per-user PATH (HKCU Environment\Path) so a bare
;      `hitch` resolves. installMode is currentUser, so HKCU is the right hive.
;   3. Broadcast WM_SETTINGCHANGE so already-running shells pick up the new PATH.
; Both are reversed on uninstall.
;
; PATH editing is a manual read-modify-write that PRESERVES THE VALUE TYPE: if
; Environment\Path is REG_EXPAND_SZ we write it back as REG_EXPAND_SZ; if REG_SZ
; (or absent) we write REG_SZ. Blindly rewriting a REG_EXPAND_SZ PATH as REG_SZ
; would freeze any %VAR% references in the user's PATH, so we detect the type
; first via a raw RegQueryValueEx (see HitchReadPath). Crucially the READ itself
; also goes through RegQueryValueEx, NOT NSIS `ReadRegStr`: ReadRegStr returns ""
; for a REG_EXPAND_SZ value, and a populated PATH read as "" turns the append into
; a destructive bare overwrite of the whole PATH. De-dup guards against double-
; appending on repair/upgrade. Everything is self-contained — only LogicLib.nsh
; (always included by Tauri's template) is assumed; no StrFunc/EnVar dependency.

!ifndef HITCH_CLI_REACH_IN_NSH
!define HITCH_CLI_REACH_IN_NSH

!include "LogicLib.nsh"

; HKCU subkey holding the per-user environment.
!define HITCH_ENV_KEY "Environment"

; --- self-contained string helpers (no StrFunc dependency) ---------------

; Hitch_StrContains: set $R0 to "1" if needle ($R2) occurs in haystack ($R1),
; else "". Linear scan comparing fixed-length substrings.
;   Push <haystack>
;   Push <needle>
;   Call Hitch_StrContains        ; or `Call un.Hitch_StrContains` in uninstall
;   Pop $R0   ; "1" or ""
;
; NSIS keeps install and uninstall as separate contexts; a function Call-ed from
; the uninstall section must be named `un.<fn>`. We share one body macro and
; instantiate it for both contexts so the two copies never drift.
!macro HitchStrContainsBody
  Exch $R2          ; needle
  Exch
  Exch $R1          ; haystack
  Push $R3          ; needle length
  Push $R4          ; cursor index
  Push $R5          ; current window

  StrLen $R3 $R2
  StrCpy $R4 0
  StrCpy $R0 ""
  hitch_sc_loop:
    StrCpy $R5 $R1 $R3 $R4    ; window of needle-length at offset $R4
    StrCmp $R5 "" hitch_sc_done   ; ran past end of haystack
    StrCmp $R5 $R2 hitch_sc_hit
    IntOp $R4 $R4 + 1
    Goto hitch_sc_loop
  hitch_sc_hit:
    StrCpy $R0 "1"
  hitch_sc_done:

  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R1
  Pop $R2
  Exch $R0          ; result on top of stack
!macroend
Function Hitch_StrContains
  !insertmacro HitchStrContainsBody
FunctionEnd
Function un.Hitch_StrContains
  !insertmacro HitchStrContainsBody
FunctionEnd

; Hitch_StrReplace: replace every occurrence of $R2 with $R3 in $R1, leaving the
; result in $R0.
;   Push <haystack>
;   Push <find>
;   Push <replace>
;   Call Hitch_StrReplace         ; or `Call un.Hitch_StrReplace` in uninstall
;   Pop $R0
!macro HitchStrReplaceBody
  Exch $R3          ; replace
  Exch
  Exch $R2          ; find
  Exch 2
  Exch $R1          ; haystack
  Push $R4          ; find length
  Push $R5          ; cursor
  Push $R6          ; window
  Push $R7          ; accumulator

  StrLen $R4 $R2
  StrCpy $R7 ""
  StrCpy $R5 0
  hitch_sr_loop:
    StrCpy $R6 $R1 $R4 $R5    ; window of find-length at $R5
    StrCmp $R6 "" hitch_sr_tail
    StrCmp $R6 $R2 hitch_sr_match
    ; no match: copy one char to accumulator, advance by 1.
    StrCpy $R6 $R1 1 $R5
    StrCpy $R7 "$R7$R6"
    IntOp $R5 $R5 + 1
    Goto hitch_sr_loop
  hitch_sr_match:
    StrCpy $R7 "$R7$R3"
    IntOp $R5 $R5 + $R4
    Goto hitch_sr_loop
  hitch_sr_tail:
    ; copy any trailing remainder past the last full window.
    StrCpy $R6 $R1 "" $R5
    StrCpy $R0 "$R7$R6"

  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R1
  Pop $R2
  Pop $R3
  Push $R0
  Exch
  Pop $R0
  Exch $R0
!macroend
Function Hitch_StrReplace
  !insertmacro HitchStrReplaceBody
FunctionEnd
Function un.Hitch_StrReplace
  !insertmacro HitchStrReplaceBody
FunctionEnd

; Broadcast WM_SETTINGCHANGE("Environment") so Explorer / running shells reload
; the user environment without a logout. HWND_BROADCAST=0xFFFF, WM_SETTINGCHANGE
; / WM_WININICHANGE=0x1A, SMTO_ABORTIFHUNG=0x0002, 5000ms timeout. Best-effort.
!macro HitchBroadcastEnv
  System::Call 'user32::SendMessageTimeout(i 0xffff, i 0x1a, i 0, t "Environment", i 0x0002, i 5000, *i .r0)'
!macroend

; Read HKCU\Environment\Path. Outputs:
;   $1 — the RAW (unexpanded) value, or "" if absent/unreadable.
;   $2 — REG type code (REG_SZ=1, REG_EXPAND_SZ=2, 0 if absent -> default REG_SZ).
;   $0 — "1" if a NON-EMPTY Path value EXISTS in the registry, else "". This is a
;        fail-safe the caller MUST honour: never bare-overwrite PATH when $0=="1".
;
; We read via RegQueryValueExW, NOT NSIS `ReadRegStr`, ON PURPOSE: `ReadRegStr`
; returns an EMPTY string for a REG_EXPAND_SZ value (it only handles REG_SZ), and
; a user's PATH is normally REG_EXPAND_SZ. Reading a populated PATH as "" would
; make the append in NSIS_HOOK_POSTINSTALL bare-overwrite it — silently destroying
; the user's entire PATH. So we (a) size-query for the real type + byte length,
; (b) set the $0 "exists" guard from that length BEFORE any read can fail, then
; (c) read the bytes into $1. Even if the marshal below ever comes back empty, $0
; still tells the caller a real PATH is there, so it refuses the destructive write.
;
; Uses $3..$6 as scratch and restores them, so the results are $0/$1/$2.
!macro HitchReadPath
  Push $3   ; hKey
  Push $4   ; Win32 return code (NOT the handle — that's $3)
  Push $5   ; value byte-size (from the size query; reused as the read's in/out)
  Push $6   ; buffer pointer

  StrCpy $0 ""
  StrCpy $1 ""
  StrCpy $2 0

  ; KEY_READ = 0x20019, HKCU = 0x80000001. Capture the function's RETURN code in
  ; $4 (the old code tested $3, the output handle, which is nonzero on success —
  ; so the type query never ran and every write fell back to REG_SZ).
  System::Call 'advapi32::RegOpenKeyExW(i 0x80000001, w "${HITCH_ENV_KEY}", i 0, i 0x20019, *i .r3) i .r4'
  ${If} $4 == 0
    ; Size query (lpData=NULL): real type -> $2, required byte length -> $5.
    StrCpy $5 0
    System::Call 'advapi32::RegQueryValueExW(i r3, w "Path", i 0, *i .r2, i 0, *i .r5) i .r4'
    ; >2 bytes = more than the lone trailing wide NUL, i.e. a non-empty value.
    ${If} $4 == 0
    ${AndIf} $5 > 2
      StrCpy $0 "1"           ; guard set from the size, before any read can fail.
      System::Alloc $5
      Pop $6
      ${If} $6 <> 0
        System::Call 'advapi32::RegQueryValueExW(i r3, w "Path", i 0, *i .r2, i r6, *i r5) i .r4'
        ${If} $4 == 0
          System::Call '*$6(&t$5 .r1)'   ; read $5 bytes of wide text into $1
        ${EndIf}
        System::Free $6
      ${EndIf}
    ${EndIf}
    System::Call 'advapi32::RegCloseKey(i r3)'
  ${EndIf}

  Pop $6
  Pop $5
  Pop $4
  Pop $3
!macroend

; Write $1 back to HKCU\Environment\Path preserving the type captured in $2.
!macro HitchWritePath
  ${If} $2 == 2
    WriteRegExpandStr HKCU "${HITCH_ENV_KEY}" "Path" "$1"
  ${Else}
    WriteRegStr HKCU "${HITCH_ENV_KEY}" "Path" "$1"
  ${EndIf}
!macroend

; Stop a running hitch-daemon.exe / hitch.exe so the installer can overwrite the
; locked sidecars. Tauri's template closes the GUI app ($INSTDIR\Hitch.exe) but
; knows nothing about the daemon sidecar (or the hitch.exe CLI copy of it), which
; runs detached and keeps the .exe open — without this, an upgrade-over-a-running
; -daemon dies with NSIS "Error opening file for writing: hitch-daemon.exe".
;
; taskkill /F kills by image name (the per-user install dir holds the only copy
; of each), /T also reaps any child processes the daemon spawned. A non-zero exit
; (nothing to kill) is ignored. The short Sleep lets the kernel release the file
; handle before Tauri starts laying files down.
!macro HitchStopDaemon
  Push $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM hitch.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /T /IM hitch-daemon.exe'
  Pop $0
  Sleep 500
  Pop $0
!macroend

; --- Tauri hooks ---------------------------------------------------------

; Invoked by Tauri's NSIS template BEFORE files are laid down in $INSTDIR. Stop
; the running daemon first so its locked .exe can be overwritten on upgrade.
!macro NSIS_HOOK_PREINSTALL
  !insertmacro HitchStopDaemon
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 1) Create the CLI entrypoint by copying the daemon sidecar to hitch.exe.
  CopyFiles /SILENT "$INSTDIR\hitch-daemon.exe" "$INSTDIR\hitch.exe"

  ; 2) Append $INSTDIR to the per-user PATH (type-preserving, de-duped).
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0

  !insertmacro HitchReadPath

  ; De-dup: skip if ";$INSTDIR;" already appears in ";<path>;".
  Push ";$1;"
  Push ";$INSTDIR;"
  Call Hitch_StrContains
  Pop $R0
  ${If} $R0 == ""
    ; Not already present. Add it WITHOUT ever clobbering a populated PATH:
    ${If} $1 != ""
      ; Read OK and non-empty -> append, preserving the captured type.
      StrCpy $1 "$1;$INSTDIR"
      !insertmacro HitchWritePath
      !insertmacro HitchBroadcastEnv
    ${ElseIf} $0 == ""
      ; PATH is genuinely empty/absent -> safe to set the bare install dir.
      StrCpy $1 "$INSTDIR"
      !insertmacro HitchWritePath
      !insertmacro HitchBroadcastEnv
    ${Else}
      ; FAIL-SAFE: a non-empty PATH exists ($0=="1") but we read it as "" — do
      ; NOT write, or we would wipe it. Leave PATH untouched; the Remote Hosts
      ; settings panel shows "not-installed" so the user can re-run/repair.
      DetailPrint "Hitch: skipped PATH update — couldn't read the existing PATH safely."
    ${EndIf}
  ${EndIf}

  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; Invoked by Tauri's NSIS template BEFORE files are removed. Stop the daemon so
; its .exe (and the hitch.exe copy) can be deleted, mirroring the install side.
; nsExec/Sleep work unchanged in the uninstall context (only Functions need the
; `un.` prefix), so the same macro body is reused.
!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro HitchStopDaemon
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; 1) Remove the CLI entrypoint we created.
  Delete "$INSTDIR\hitch.exe"

  ; 2) Remove $INSTDIR from the per-user PATH (type-preserving), then broadcast.
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0

  !insertmacro HitchReadPath
  ${If} $1 != ""
    ; Work on a ';'-padded copy so first/last segments are handled uniformly,
    ; replace ";$INSTDIR;" with ";", then strip the padding back off.
    StrCpy $0 ";$1;"
    Push $0
    Push ";$INSTDIR;"
    Push ";"
    Call un.Hitch_StrReplace
    Pop $0
    ; Drop the leading and trailing ';' padding (StrCpy with negative len/skip).
    StrCpy $0 $0 "" 1        ; skip leading ';'
    StrCpy $0 $0 -1          ; drop trailing ';'
    StrCpy $1 $0

    ${If} $1 == ""
      DeleteRegValue HKCU "${HITCH_ENV_KEY}" "Path"
    ${Else}
      !insertmacro HitchWritePath
    ${EndIf}
    !insertmacro HitchBroadcastEnv
  ${EndIf}

  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

!endif ; HITCH_CLI_REACH_IN_NSH
