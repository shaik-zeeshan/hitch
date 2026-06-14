# Windows ssh-agent named-pipe de-risk spike (relay plan, slice 7)

This standalone binary answers the one open question gating the **Windows** code
path of the Hitch ssh-agent relay (proto v29, ADR 0014 amendment):

> Does Win32-OpenSSH's `ssh.exe` / `ssh-add.exe` honor `SSH_AUTH_SOCK` when it
> points at a **named pipe** `\\.\pipe\…`?

The macOS/Linux stages (proto v29, the transport address helper, the daemon
ssh-agent server, the GUI relay endpoint, the toggle) do **not** depend on this —
they are already built and testable on Unix. This spike only decides *how the
Windows daemon injects the relay socket*.

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
