# Windows ssh-agent named-pipe de-risk spike (relay plan)

> **Resolved — reference/historical, not a blocker.** This spike came back
> **GREEN** and the Windows relay code path it gated has since been implemented
> in the product: the daemon hosts its ssh-agent relay socket as an owner-only
> named pipe through `DaemonListener::bind` and advertises `endpoint_os_address`
> as `SSH_AUTH_SOCK` identically on Unix and Windows (no `hitch-git` change), and
> the co-located Windows GUI bridge to the local OS agent lives in
> `apps/desktop/src-tauri/src/ssh_agent_bridge.rs`. The ssh-agent relay is now
> **cross-platform** (the Unix path is validated; the Windows build compiles and
> unit-tests pass, with hardware Touch ID / Windows Hello end-to-end validation
> still pending on the `pc` Windows host). This binary is kept only as the
> reference de-risk artifact for the decision below.

This standalone binary answered the one open question that originally gated the
**Windows** code path of the Hitch ssh-agent relay (ADR 0014 amendment):

> Does Win32-OpenSSH's `ssh.exe` / `ssh-add.exe` honor `SSH_AUTH_SOCK` when it
> points at a **named pipe** `\\.\pipe\…`?

The macOS/Linux stages (the proto control messages, the transport address helper,
the daemon ssh-agent server, the GUI relay endpoint, the toggle) never depended
on this — they were already built and testable on Unix. This spike only decided
*how the Windows daemon injects the relay socket*.

## Run it (on the Windows host)

```powershell
cargo run --manifest-path spikes\win-ssh-agent-pipe\Cargo.toml
```

It binds an owner-only named pipe (the exact `GenericNamespaced` +
`D:P(A;;GA;;;OW)` SDDL that `crates/hitch-proto`'s `DaemonListener::bind` uses),
sets `SSH_AUTH_SOCK` to its `\\.\pipe\…` address, runs
`%SystemRoot%\System32\OpenSSH\ssh-add.exe -l` against it, and acts as a minimal
fake agent (answers `REQUEST_IDENTITIES` with zero keys).

## Reading the result

- **GREEN** — `spike RESULT: GREEN …` and the process printed that `ssh-add`
  connected over the pipe. Win32-OpenSSH honors a named-pipe `SSH_AUTH_SOCK`.
  → Take the **uniform `SSH_AUTH_SOCK`** path: enable the Windows daemon
  agent-socket as a named pipe bound through `DaemonListener::bind` (owner-only
  SDDL for free) and advertise `endpoint_os_address(&path)`
  (`\\.\pipe\hitch-<hash>`, already implemented in `transport.rs`) as
  `SSH_AUTH_SOCK`, exactly as on Unix. No `hitch-git` change.

- **RED / inconclusive** — `ssh-add` did not connect (the spike's `accept()` never
  returned; you'll see `spike RESULT: RED …`).
  → Take the documented **fallback**: inject
  `-o IdentityAgent=\\.\pipe\hitch-<hash>` via `GIT_SSH_COMMAND` in
  `crates/hitch-git/src/lib.rs` `default_network_ssh_command` (the existing
  `#[cfg(windows)]` branch that already prefers System32 `ssh.exe`), instead of
  `SSH_AUTH_SOCK`.

Record the outcome in the ADR 0014 amendment ("Windows injection") once known.

## Why it is not a workspace member

It is Windows-only and exists purely to de-risk. Keeping it out of the root
`[workspace]` members means `cargo build --workspace` on macOS/Linux never tries
to compile its named-pipe code, so the Unix stages stay green regardless.
