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
; first via a raw RegQueryValueEx. De-dup guards against double-appending on
; repair/upgrade. Everything is self-contained — only LogicLib.nsh (always
; included by Tauri's template) is assumed; no StrFunc/EnVar dependency.

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

; Read HKCU\Environment\Path into $1 and its REG type code into $2
; (REG_SZ=1, REG_EXPAND_SZ=2, 0 if absent/unknown -> caller defaults to REG_SZ).
!macro HitchReadPath
  ReadRegStr $1 HKCU "${HITCH_ENV_KEY}" "Path"
  StrCpy $2 0
  System::Call 'advapi32::RegOpenKeyExW(i 0x80000001, w "${HITCH_ENV_KEY}", i 0, i 0x20019, *i .r3)'
  ${If} $3 == 0
    System::Call 'advapi32::RegQueryValueExW(i r3, w "Path", i 0, *i .r2, i 0, i 0)'
    System::Call 'advapi32::RegCloseKey(i r3)'
  ${EndIf}
!macroend

; Write $1 back to HKCU\Environment\Path preserving the type captured in $2.
!macro HitchWritePath
  ${If} $2 == 2
    WriteRegExpandStr HKCU "${HITCH_ENV_KEY}" "Path" "$1"
  ${Else}
    WriteRegStr HKCU "${HITCH_ENV_KEY}" "Path" "$1"
  ${EndIf}
!macroend

; --- Tauri hooks ---------------------------------------------------------

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
    ; Not present -> append (or set bare if PATH was empty), then write + broadcast.
    ${If} $1 == ""
      StrCpy $1 "$INSTDIR"
    ${Else}
      StrCpy $1 "$1;$INSTDIR"
    ${EndIf}
    !insertmacro HitchWritePath
    !insertmacro HitchBroadcastEnv
  ${EndIf}

  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
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
