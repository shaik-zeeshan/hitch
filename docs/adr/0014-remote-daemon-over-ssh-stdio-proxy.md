# Remote Daemons are reached through an SSH stdio proxy

Hitch remote support connects the GUI to a long-lived Hitch **Daemon** running on the SSH host, not to a local-daemon-owned remote shell. The GUI starts `ssh <host> hitch daemon proxy`; that remote proxy auto-starts or attaches to the host-local daemon endpoint and bridges the existing Hitch protocol over SSH stdio. This keeps Sessions, Jobs, git operations, Worktrees, and Agent State owned by the daemon on the machine where the work happens, while avoiding a daemon network listener or cross-platform socket forwarding.

## Considered Options

- **SSH stdio proxy** — chosen because SSH already supplies authentication/encryption, no port is exposed, and remote agent hooks keep using the remote daemon's local socket/pipe.
- **SSH port/socket forwarding** — rejected because Unix-socket and Windows named-pipe forwarding would make the transport model platform-shaped at the GUI boundary.
- **Remote TCP listener** — rejected because it creates a new security surface and auth model outside Hitch's current local-endpoint daemon design.
- **Manual remote daemon endpoint** — rejected because it makes remote setup brittle and leaves users responsible for daemon lifecycle.

## Consequences

An **SSH Host** is GUI-local attachment configuration and stores only an OpenSSH target string (`prod`, `user@example.com`, etc.); it is not stored in the local daemon's Project registry or in any remote daemon. Hitch shells out to `ssh -o BatchMode=yes` and relies on the user's OpenSSH config, ssh-agent, hardware keys, ProxyJump, and known_hosts; it does not store private keys or passphrases, and it does not allow host-key/password/passphrase prompts on the protocol stream. The remote host must have a compatible Hitch daemon/proxy binary preinstalled and available as `hitch`; the GUI validates version compatibility and reports an install command when it is missing or stale. Hitch deliberately does not auto-upload remote binaries in the first design, keeping the trust boundary explicit. The proxy is connection-scoped, but the daemon it attaches to remains detached and survives GUI disconnects, preserving the existing daemon-owned session model.

The Add SSH Host dialog has one required field: the OpenSSH target. It offers Test Connection before Save; the test runs the same non-interactive SSH/proxy/version path and surfaces classified actionable failures for auth/agent, host-key trust, missing `hitch`, protocol mismatch, proxy startup, and network/VPN. Errors include the exact manual test command: `ssh -o BatchMode=yes <target> hitch daemon proxy`.

Remote SSH Hosts are in scope for every daemon-supported OS, including Unix and Windows. The SSH stdio proxy hides whether the remote daemon's host-local endpoint is a Unix socket, Windows named pipe, or another local transport; platform-specific path/PTy/process behavior remains inside the remote daemon.

The GUI presents local and remote daemon registries together in a multi-daemon tree, with Local first and saved SSH Hosts sorted alphabetically by target string as top-level scopes. Project, Worktree, Session, and Job identifiers are interpreted within their owning daemon scope, never as globally unique across all attached daemons.

Each SSH Host top-level row shows the host name, its Daemon Status, and the same derived Agent State rollup used for Projects/Worktrees so a collapsed host can still page for `needs-approval` or `error`.

Agent State from every connected daemon participates in the same global attention surfaces as local Agent State; labels and navigation include the local/SSH Host scope so a remote approval is attributable.

Session tabs stay scoped to the active Project/Worktree parent, as today; remote support does not put Sessions from different daemon scopes into one tab strip. While a remote parent is selected, host context is carried by the selected tree row rather than repeated in tab titles or a new center breadcrumb.

Global search surfaces such as the command palette include the local/SSH Host scope as muted metadata (`prod · project · branch`) for remote Projects/Sessions, without changing Session tab labels.

Global actions operate on the currently selected daemon scope, or on the owning daemon of the selected Project/Worktree/Session. Dialogs show that target scope and may let the user change it, rather than duplicating command-palette actions per host.

Remote destructive confirmations always include the SSH Host name and remote path (for example, `Remove worktree on prod?`) so a path that also exists locally cannot be mistaken for local state.

Saved SSH Hosts are enabled by default: the GUI connects to every saved host on launch and auto-reconnects when the SSH/proxy connection drops. Reconnect uses a short exponential backoff (~2s, 5s, 15s, 30s, then capped around 60s with jitter). The remote Daemon and its Sessions continue running; the GUI marks the host unreachable, keeps the last tree greyed as stale UI, disables daemon-backed actions in that scope, and replaces it from daemon replay after reconnect. The unreachable/failed host row includes a Retry Now action that resets backoff after the user fixes VPN, ssh-agent, or host setup.

Removing an SSH Host forgets only the GUI-local host entry and disconnects the proxy; it does not shut down the remote Daemon or kill remote Sessions.

Adding a Project inside an SSH Host scope uses a remote directory browser backed by daemon requests on that host, then sends the existing AddProject/CloneProject operations to that remote daemon. The browser opens at the remote daemon user's home directory, can navigate anywhere that user can read, and allows typing an absolute path. The UI is a folders-first list with a path bar, parent/home controls, hidden-folder toggle defaulting off, and explicit loading/error rows. The GUI never maps remote paths onto local paths.

Dropping local files onto a remote Session uploads them over the existing Hitch protocol stream to the remote Daemon, which writes them to a per-session remote directory under `~/.hitch/uploads/<session-id>/` and then inserts the uploaded remote paths into the shell. This preserves the local terminal drop behavior without pretending local paths exist remotely, avoids a second `scp`/SFTP connection, and works through the same SSH stdio proxy on Unix and Windows remotes. The remote Daemon owns upload cleanup and deletes that per-session directory when the Session closes.

Remote file-drop upload supports regular files only in the first design. It shows progress, allows cancellation before paths are inserted, rejects directories with explicit copy explaining that recursive upload is not supported yet, and auto-suffixes filename collisions inside the per-session upload dir (`file.txt`, `file-1.txt`, …) before inserting the actual remote paths.
