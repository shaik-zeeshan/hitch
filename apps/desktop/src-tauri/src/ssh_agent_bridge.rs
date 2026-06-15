//! Local ssh-agent relay bridge (proto v29, plan slices 4 + 6).
//!
//! On a REMOTE connection whose per-host toggle is on, the GUI declares a relay
//! by sending [`ControlMessage::SshAgentRelay`] to the remote daemon right after
//! the Hello (see `ssh_pool::connect_attempt`). The remote daemon then, whenever
//! one of its git children connects to its proxied `SSH_AUTH_SOCK`, sends an
//! [`ControlMessage::SshAgentOpen`] up the proxy stream. THIS module answers that
//! by connecting to the **local** ssh-agent (1Password / OpenSSH) and bridging
//! the raw ssh-agent wire bytes both ways:
//!
//! * daemon -> GUI [`ControlMessage::SshAgentData`] -> write to the local agent,
//! * local agent reply bytes -> [`ControlMessage::SshAgentData`] back up,
//! * [`ControlMessage::SshAgentClose`] / EOF tears the channel down.
//!
//! ## Naming (CONTEXT.md, mandatory)
//!
//! The relayed thing is the **SSH agent**, NEVER the AI Agent. Every type/thread
//! here is `SshAgent*` / `ssh-agent` / `ssh_agent`.
//!
//! ## Touch ID must not stall the control reader (the whole point)
//!
//! A 1Password sign can block for seconds behind a Touch ID prompt. So the
//! per-channel bridge owns the local-agent `UnixStream` on a **dedicated
//! `std::thread`** (no tokio in this crate): the inbound write half is written
//! to from the reader loop and returns immediately (a `write_all` to the agent
//! socket does not block on the sign), while a separate dedicated reader thread
//! `read`s the agent's reply — which is where the multi-second Touch ID wait
//! lands — entirely off the control-reader loop. The reply bytes are sent back
//! up via a cheaply-cloneable write-up callback the caller supplies (it locks
//! the remote connection writer and writes one control line; see
//! `ssh_pool::write_remote_control`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hitch_proto::ControlMessage;

/// Reserved-TLD sentinel destination handed to `ssh -G` to resolve the user's
/// DEFAULT ssh-agent (their `Host *` / global `IdentityAgent`) rather than any
/// specific git provider's `Host` block. `ssh -G` requires *a* destination but
/// never connects; a `.invalid` host (RFC 2606) matches no real `Host` block, so
/// the resolved `IdentityAgent` is the agent-app-agnostic global default — which
/// is exactly the one the user's own `ssh`/`git` use for everything.
const AGENT_PROBE_HOST: &str = "hitch-default-agent-probe.invalid";

/// Write timeout on the bridged local-agent socket. The inbound write runs on the
/// control-reader's call into [`SshAgentRelay::write`], so a wedged agent socket
/// must not block it forever; a timed-out write tears the channel down (mirrors
/// the connect-failed policy).
const LOCAL_AGENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on request bytes buffered while a channel is still connecting to the local
/// agent. A well-behaved ssh client sends a small request and waits for the reply,
/// so the connect-window buffer is tiny in practice; this bound stops a hostile or
/// runaway daemon from growing it without limit. On overflow the channel is closed
/// (mirrors the connect-failed policy).
const MAX_CONNECTING_BUFFER: usize = 256 * 1024;

/// Resolve the local ssh-agent socket to bridge to. Used BOTH as the capability
/// gate (is an agent reachable at all? — drives whether the GUI even declares the
/// relay) and as the connect target when the daemon opens a channel.
///
/// Resolves the agent **the way OpenSSH itself does** — so it works for any agent
/// app (1Password, Bitwarden, Secretive, gpg-agent, a forwarded socket) and is
/// immune to the macOS "GUI launched from the Dock loses the shell environment"
/// trap that otherwise leaves `SSH_AUTH_SOCK` pointing at the empty system agent:
///
/// 1. The effective `IdentityAgent` from `ssh -G` (honors `Host *`/`Match`,
///    `Include`, `~`-expansion and the `none`/`SSH_AUTH_SOCK` tokens):
///    * an explicit socket path -> use it;
///    * `none` -> the user disabled agent auth, return `None`;
///    * `SSH_AUTH_SOCK` / unset -> defer to the environment (step 2).
/// 2. `SSH_AUTH_SOCK` if set, non-empty, and NOT macOS's empty launchd system
///    agent (a Dock-launched GUI inherits that in place of the user's real shell
///    value — bridging it is the original bare-"Permission denied" bug).
/// 3. Labeled fallback: the 1Password fixed socket if it exists — 1Password does
///    NOT export `SSH_AUTH_SOCK` itself, so a no-config 1Password Mac has only
///    this path. App-specific on purpose and intentionally LAST; every
///    config/env-driven path above wins.
///
/// `None` => no reachable agent; the relay is not declared and any stray
/// `SshAgentOpen` is closed immediately.
pub fn local_agent_socket() -> Option<PathBuf> {
    resolve_local_agent(
        ssh_config_identity_agent(),
        std::env::var_os("SSH_AUTH_SOCK"),
        one_password_fallback_socket(),
    )
}

/// Pure resolution policy for [`local_agent_socket`], factored out so it is
/// testable without spawning `ssh` or mutating process env. See that function's
/// doc comment for the ladder.
fn resolve_local_agent(
    identity_agent: Option<String>,
    ssh_auth_sock: Option<OsString>,
    one_password_fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    // 1. Honor ssh's own IdentityAgent resolution.
    match identity_agent.as_deref().map(str::trim) {
        // The user explicitly disabled agent auth — respect it, never fall back.
        Some("none") => return None,
        // The default / explicit token: defer to SSH_AUTH_SOCK below.
        Some("SSH_AUTH_SOCK") | Some("") | None => {}
        // An explicit socket path (ssh -G already ~-expanded it).
        Some(path) => return Some(PathBuf::from(path)),
    }

    // 2. SSH_AUTH_SOCK from the environment — but never macOS's empty launchd
    //    system agent, which a Dock-launched GUI inherits in place of the user's
    //    real shell value.
    if let Some(sock) = ssh_auth_sock {
        if !sock.is_empty() && !is_macos_launchd_agent(Path::new(&sock)) {
            return Some(PathBuf::from(sock));
        }
    }

    // 3. Labeled fallback.
    one_password_fallback
}

/// Ask OpenSSH itself for the effective `IdentityAgent` via `ssh -G`, so we honor
/// the user's full ssh config (Host/Match blocks, `Include`, `~`-expansion, the
/// `none`/`SSH_AUTH_SOCK` tokens) with zero reimplementation and no app-specific
/// knowledge. Returns the raw value (an absolute path, or the `none`/
/// `SSH_AUTH_SOCK` token), or `None` if ssh is unavailable, fails, or emits no
/// `identityagent` line (i.e. the OpenSSH default — treated as `SSH_AUTH_SOCK`).
///
/// `/usr/bin/ssh` is invoked by absolute path because a Dock-launched GUI has the
/// minimal launchd PATH; `ssh -G` never connects, so a `.invalid` sentinel host
/// is resolved offline and fast.
fn ssh_config_identity_agent() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/ssh")
        .arg("-G")
        .arg(AGENT_PROBE_HOST)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // `ssh -G` emits "keyword value" with lowercase keywords and no
        // indentation; the value (a socket path) may itself contain spaces, so
        // take the entire remainder after the keyword rather than splitting.
        if let Some(value) = line.trim_start().strip_prefix("identityagent ") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The 1Password fixed agent socket if it exists. `$HOME` is expanded via `HOME`
/// because this crate has no `~` expander.
fn one_password_fallback_socket() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let mut path = PathBuf::from(home);
    path.push("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
    path.exists().then_some(path)
}

/// Whether `path` is macOS's per-session system ssh-agent
/// (`…/com.apple.launchd.<rand>/Listeners`). A GUI launched from the Dock/Finder
/// inherits this in `SSH_AUTH_SOCK`, but it is empty unless the user ran
/// `ssh-add` into it — 1Password/Bitwarden/Secretive users never do — so it must
/// not shadow the real agent. A macOS platform fact, not an agent-app heuristic.
fn is_macos_launchd_agent(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|s| s.contains("/com.apple.launchd.") && s.ends_with("/Listeners"))
}

/// One-shot `ssh-add -l` equivalent for self-diagnosis ONLY (never the hot path):
/// connect to `socket`, send a single `SSH_AGENTC_REQUEST_IDENTITIES`, and parse
/// the key count from the `SSH_AGENT_IDENTITIES_ANSWER`. Returns `Some(n)` on a
/// well-formed answer, or `None` if the agent is unreachable or replies with
/// anything else (e.g. `SSH_AGENT_FAILURE`). A reachable agent answering `Some(0)`
/// is the #1 cause of an opaque remote `Permission denied (publickey)`.
pub fn agent_identity_count(socket: &Path) -> Option<usize> {
    let mut stream = UnixStream::connect(socket).ok()?;
    // This probe runs synchronously on the connect/reconnect worker thread after
    // declaring the relay, so keep it short: a healthy agent answers in µs, and a
    // wedged one should not delay that host's `Running` status by seconds.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    // Request: uint32 len=1, byte type=11 (SSH_AGENTC_REQUEST_IDENTITIES).
    stream.write_all(&[0, 0, 0, 1, 11]).ok()?;
    stream.flush().ok()?;
    // Answer header: uint32 len, byte type=12 (SSH_AGENT_IDENTITIES_ANSWER),
    // uint32 nkeys. A shorter message (e.g. FAILURE, len=1) is "not an answer".
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    if u32::from_be_bytes(len_buf) < 5 {
        return None;
    }
    let mut type_and_count = [0u8; 5];
    stream.read_exact(&mut type_and_count).ok()?;
    if type_and_count[0] != 12 {
        return None;
    }
    Some(u32::from_be_bytes([
        type_and_count[1],
        type_and_count[2],
        type_and_count[3],
        type_and_count[4],
    ]) as usize)
}

/// A write-up sink: locks the remote connection writer and writes one control
/// line (no PTY payload frame). Cheaply cloneable (it is an `Arc<dyn Fn>`), so a
/// per-channel reader thread that outlives a reconnect can keep one and re-lock
/// the (possibly swapped) writer per write — exactly the existing
/// re-lock-per-write idiom (`write_remote_input`). Returns nothing: a write to a
/// dead/swapped writer is best-effort and silently dropped, never re-entering
/// teardown (deadlock note in `lib.rs::write_input_frame`).
pub type WriteUp = Arc<dyn Fn(ControlMessage) + Send + Sync>;

/// One bridged channel's state in the registry.
///
/// A channel is registered the instant the daemon's `SshAgentOpen` arrives, but
/// the connect to the local agent happens on the channel's own dedicated thread
/// (so a wedged agent can never stall the control-reader loop). Inbound request
/// bytes that arrive in that connect window are buffered here — under the registry
/// lock — and flushed in order once connected, so no request is lost.
enum ChannelHandle {
    /// Connecting on the dedicated thread; request bytes buffer here meanwhile.
    Connecting { buffered: Vec<u8> },
    /// Connected: the local-agent write half. Inbound bytes are written through.
    /// The read half is owned by the channel's dedicated reader thread, so a
    /// write here never blocks on a sign.
    Connected { agent_write: UnixStream },
}

impl ChannelHandle {
    /// Shut the local-agent socket down (Connected) so a reader thread parked on a
    /// pending sign returns at once. A Connecting handle has no socket yet — its
    /// thread aborts when it finds the registry slot gone.
    fn shutdown(self) {
        if let ChannelHandle::Connected { agent_write } = self {
            let _ = agent_write.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Per-remote-connection registry of live ssh-agent relay channels, keyed by the
/// daemon-assigned channel id. Owned by the `RemoteConnection` (one relay per
/// remote daemon). Cloneable `Arc` handle so the reader loop and the per-channel
/// reader threads share it.
#[derive(Clone, Default)]
pub struct SshAgentRelay {
    channels: Arc<Mutex<HashMap<u64, ChannelHandle>>>,
}

impl SshAgentRelay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a bridge for `channel`. Registers the channel SYNCHRONOUSLY (so an
    /// inbound `SshAgentData` racing right behind the `SshAgentOpen` is buffered,
    /// never lost) and then does ALL blocking work — the connect to the local
    /// agent AND the sign reply read — on a dedicated thread. This MUST NOT block
    /// the caller (the control-reader loop): a wedged 1Password helper or a stale
    /// `SSH_AUTH_SOCK` pointing at a hung server could otherwise stall the loop
    /// that drains the daemon→GUI stream, back-pressure the daemon's pump thread
    /// onto the shared `ClientSink` writer, and freeze every terminal on the
    /// connection. So even `UnixStream::connect` runs on the spawned thread.
    ///
    /// If no local agent is reachable, or the connect fails, the thread emits an
    /// [`ControlMessage::SshAgentClose`] up so the daemon tears its side down
    /// rather than hanging.
    pub fn open(&self, channel: u64, write_up: WriteUp) {
        // Register a Connecting slot up front so `write()` can buffer inbound
        // request bytes before the connect completes.
        if let Ok(mut channels) = self.channels.lock() {
            channels.insert(channel, ChannelHandle::Connecting { buffered: Vec::new() });
        }

        // Clone the cheap `Arc` write-up sink so the spawn-failure branch still
        // owns one after the closure moves its copy.
        let registry = self.clone();
        let thread_write_up = write_up.clone();
        let spawned = thread::Builder::new()
            .name(format!("hitch-ssh-agent-relay-{channel}"))
            .spawn(move || registry.run_channel(channel, thread_write_up));
        if let Err(err) = spawned {
            debug_log(format!(
                "ssh-agent relay: failed to spawn channel thread for {channel}: {err}; closing"
            ));
            self.close_local(channel);
            write_up(ControlMessage::SshAgentClose { channel });
        }
    }

    /// The channel's dedicated thread: connect to the local agent (the blocking
    /// connect is here, off the reader loop), flush any bytes buffered during the
    /// connect window, then pump agent reply bytes up — where the multi-second
    /// Touch ID sign wait lands — until EOF.
    fn run_channel(&self, channel: u64, write_up: WriteUp) {
        let socket = match local_agent_socket() {
            Some(socket) => socket,
            None => {
                debug_log(format!(
                    "ssh-agent relay: SshAgentOpen channel {channel} but no local agent reachable; closing"
                ));
                self.close_local(channel);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        };
        let stream = match UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(err) => {
                debug_log(format!(
                    "ssh-agent relay: connect to local agent {} failed for channel {channel}: {err}; closing",
                    socket.display()
                ));
                self.close_local(channel);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        };
        let mut read_half = match stream.try_clone() {
            Ok(read_half) => read_half,
            Err(err) => {
                debug_log(format!(
                    "ssh-agent relay: try_clone local agent stream failed for channel {channel}: {err}; closing"
                ));
                self.close_local(channel);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        };

        // Transition Connecting -> Connected under the registry lock: drain the
        // bytes buffered during the connect window (in order) to the agent, then
        // store the write half so later `write()`s go straight through. If the
        // slot is gone (a `close`/`close_all` raced the connect), abort — the
        // closer owns teardown and `write_up` may already be dead.
        {
            let mut write_half = stream;
            // Bound the inbound write to the local agent: it runs on the control
            // reader's call into `write()`, so a wedged agent socket must not block
            // it forever (mirrors the read/write timeout in `agent_identity_count`).
            let _ = write_half.set_write_timeout(Some(LOCAL_AGENT_WRITE_TIMEOUT));
            let mut channels = match self.channels.lock() {
                Ok(channels) => channels,
                Err(_) => return,
            };
            match channels.remove(&channel) {
                Some(ChannelHandle::Connecting { buffered }) => {
                    if !buffered.is_empty() {
                        let _ = write_half.write_all(&buffered).and_then(|()| write_half.flush());
                    }
                    channels.insert(channel, ChannelHandle::Connected { agent_write: write_half });
                }
                // Closed mid-connect (a `close`/`close_all` took the slot): drop
                // our streams and stop. `Connected` is impossible — only this
                // thread transitions — but if seen, restore it untouched.
                Some(other) => {
                    channels.insert(channel, other);
                    return;
                }
                None => return,
            }
        }
        debug_log(format!(
            "ssh-agent relay: opened channel {channel} -> {}",
            socket.display()
        ));

        let mut buf = [0_u8; 8192];
        loop {
            match read_half.read(&mut buf) {
                Ok(0) => break, // agent closed (EOF)
                Ok(n) => write_up(ControlMessage::ssh_agent_data(channel, &buf[..n])),
                // A signal interrupted the blocking read — not a real error; retry
                // (mirrors the daemon pump, which also retries `Interrupted`).
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    debug_log(format!(
                        "ssh-agent relay: channel {channel} read error: {err}; closing"
                    ));
                    break;
                }
            }
        }
        // Reader done: drop our write half and tell the daemon we're EOF.
        self.close_local(channel);
        write_up(ControlMessage::SshAgentClose { channel });
    }

    /// Write inbound request bytes (from the daemon) to the channel. If the
    /// channel is still connecting, the bytes are BUFFERED (flushed in order once
    /// connected, bounded by [`MAX_CONNECTING_BUFFER`]); if connected, they are
    /// written straight to the local-agent write half. Returns immediately — the
    /// SIGN/response is read on the channel's dedicated thread, so this never
    /// blocks on Touch ID. A write to a torn-down/unknown channel is silently
    /// ignored.
    ///
    /// Returns `true` when this call dropped the channel (a dead agent socket, or
    /// the connect-window buffer overflowed): the caller should then emit an
    /// [`ControlMessage::SshAgentClose`] up so the daemon tears its side down. A
    /// connecting channel that overflowed has no reader thread yet to emit it.
    #[must_use]
    pub fn write(&self, channel: u64, bytes: &[u8]) -> bool {
        let mut channels = match self.channels.lock() {
            Ok(channels) => channels,
            Err(_) => return false,
        };
        let Some(handle) = channels.get_mut(&channel) else {
            return false;
        };
        match handle {
            ChannelHandle::Connecting { buffered } => {
                if buffered.len().saturating_add(bytes.len()) > MAX_CONNECTING_BUFFER {
                    // A runaway/hostile daemon is flooding the connect window; drop
                    // the channel. The connecting thread aborts when it finds the
                    // slot gone, so we must tell the daemon to close its side.
                    debug_log(format!(
                        "ssh-agent relay: connect-window buffer for channel {channel} \
                         exceeded {MAX_CONNECTING_BUFFER} bytes; closing"
                    ));
                    channels.remove(&channel);
                    return true;
                }
                buffered.extend_from_slice(bytes);
                false
            }
            ChannelHandle::Connected { agent_write } => {
                if agent_write.write_all(bytes).is_err() || agent_write.flush().is_err() {
                    // The local agent socket is dead; drop the channel. The reader
                    // thread will also observe EOF/err and emit SshAgentClose up.
                    channels.remove(&channel);
                }
                false
            }
        }
    }

    /// Close one channel: if connected, shut its local-agent socket down (so a
    /// reader thread blocked on a pending sign unblocks) and drop it from the
    /// registry. A still-connecting channel is just removed — its thread detects
    /// the missing slot after the connect and aborts.
    pub fn close(&self, channel: u64) {
        if let Some(handle) = self.take(channel) {
            handle.shutdown();
        }
    }

    /// On connection drop/reconnect: close every live channel. The registry is
    /// invalidated so a stale `SshAgentData` after a reconnect finds nothing, and
    /// any reader thread parked on a sign is unblocked by the socket shutdown.
    pub fn close_all(&self) {
        let drained: Vec<ChannelHandle> = match self.channels.lock() {
            Ok(mut channels) => channels.drain().map(|(_, handle)| handle).collect(),
            Err(_) => return,
        };
        for handle in drained {
            handle.shutdown();
        }
    }

    /// Remove a channel's handle from the registry WITHOUT shutting the socket
    /// down (the reader thread already saw EOF/err, so its read half is done and
    /// its write half is the only thing left to drop).
    fn close_local(&self, channel: u64) {
        let _ = self.take(channel);
    }

    fn take(&self, channel: u64) -> Option<ChannelHandle> {
        self.channels.lock().ok().and_then(|mut c| c.remove(&channel))
    }
}

/// Opt-in observability gated on `HITCH_DEBUG` (mirrors the git crate's
/// convention), to stderr. The byte payloads themselves are NEVER logged — only
/// channel lifecycle and failures.
fn debug_log(message: String) {
    if std::env::var_os("HITCH_DEBUG").is_some_and(|v| !v.is_empty()) {
        eprintln!("[hitch ssh-agent relay] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative macOS Dock-launch SSH_AUTH_SOCK (the empty system agent).
    const LAUNCHD_AGENT: &str = "/var/run/com.apple.launchd.9RLIbR5haR/Listeners";
    const ONEPW: &str = "/Users/x/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock";

    #[test]
    fn identity_agent_path_wins_over_env_and_fallback() {
        // An explicit `IdentityAgent <path>` is the user's deliberate choice and
        // beats everything below it.
        assert_eq!(
            resolve_local_agent(
                Some("/tmp/op.sock".to_string()),
                Some(OsString::from(LAUNCHD_AGENT)),
                Some(PathBuf::from(ONEPW)),
            ),
            Some(PathBuf::from("/tmp/op.sock"))
        );
    }

    #[test]
    fn identity_agent_none_disables_the_relay() {
        // `IdentityAgent none` means "no agent" — never fall back to env/1Password.
        assert_eq!(
            resolve_local_agent(
                Some("none".to_string()),
                Some(OsString::from("/tmp/some-agent.sock")),
                Some(PathBuf::from(ONEPW)),
            ),
            None
        );
    }

    #[test]
    fn ssh_auth_sock_token_and_unset_defer_to_env() {
        // Both the explicit `SSH_AUTH_SOCK` token and no identityagent line mean
        // "use the environment".
        for ident in [Some("SSH_AUTH_SOCK".to_string()), None] {
            assert_eq!(
                resolve_local_agent(ident, Some(OsString::from("/tmp/real.sock")), None),
                Some(PathBuf::from("/tmp/real.sock"))
            );
        }
    }

    #[test]
    fn dock_launched_empty_launchd_agent_falls_through_to_fallback() {
        // The regression: a Dock launch inherits the empty macOS system agent in
        // SSH_AUTH_SOCK; it must not shadow the 1Password fallback.
        assert_eq!(
            resolve_local_agent(
                None,
                Some(OsString::from(LAUNCHD_AGENT)),
                Some(PathBuf::from(ONEPW)),
            ),
            Some(PathBuf::from(ONEPW))
        );
        // ...and with no fallback either, there is simply no agent.
        assert_eq!(
            resolve_local_agent(None, Some(OsString::from(LAUNCHD_AGENT)), None),
            None
        );
    }

    #[test]
    fn fallback_used_only_when_env_absent() {
        assert_eq!(
            resolve_local_agent(None, None, Some(PathBuf::from(ONEPW))),
            Some(PathBuf::from(ONEPW))
        );
        assert_eq!(resolve_local_agent(None, None, None), None);
    }

    #[test]
    fn detects_macos_launchd_system_agent() {
        assert!(is_macos_launchd_agent(Path::new(LAUNCHD_AGENT)));
        assert!(is_macos_launchd_agent(Path::new(
            "/private/tmp/com.apple.launchd.AbC123/Listeners"
        )));
        assert!(!is_macos_launchd_agent(Path::new(ONEPW)));
        assert!(!is_macos_launchd_agent(Path::new("/tmp/agent.sock")));
    }

    #[test]
    fn write_to_unknown_channel_is_a_noop() {
        // No panic, no registration: writing to a channel that was never opened
        // (e.g. a stale frame after a reconnect invalidated the registry) is
        // silently ignored.
        let relay = SshAgentRelay::new();
        assert!(!relay.write(99, b"\x00\x01\x02"));
        relay.close(99); // also a no-op
    }

    #[test]
    fn close_all_clears_the_registry() {
        // A connection drop invalidates every channel. We can't open a real agent
        // here, so assert the empty-registry path is safe and idempotent.
        let relay = SshAgentRelay::new();
        relay.close_all();
        relay.close_all();
    }
}
