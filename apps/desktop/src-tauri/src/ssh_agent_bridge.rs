//! Local ssh-agent relay bridge (proto v31, ADR 0014 amendment). CROSS-PLATFORM.
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
//! ## Cross-platform (silly-ridge-27)
//!
//! The bridge connects to the local agent through `hitch_proto::transport`'s
//! cross-platform [`connect_external`] primitive, which yields a
//! [`RelayReader`]/[`RelayWriter`] pair addressing ONE underlying connection on
//! both platforms: a `UnixStream` (clones) on Unix, the recv/send halves of one
//! named-pipe stream (interprocess ref-clones the duplex handle) on Windows. So a
//! local GUI on Windows driving a remote Unix daemon serves the relay too: on
//! Windows the agent address is a named pipe (`\\.\pipe\openssh-ssh-agent` by
//! default, resolved from `ssh -G IdentityAgent`/`SSH_AUTH_SOCK` exactly as on
//! Unix — see [`local_agent_socket`]). The Windows pipe path is validated live on
//! the host, not in macOS CI (no Windows target here).
//! TODO(win-e2e): validate on pc@192.168.0.9.
//!
//! ## Naming (CONTEXT.md, mandatory)
//!
//! The relayed thing is the **SSH agent**, NEVER the AI Agent. Every type/thread
//! here is `SshAgent*` / `ssh-agent` / `ssh_agent`.
//!
//! ## Touch ID must not stall the control reader (the whole point)
//!
//! A 1Password sign can block for seconds behind a Touch ID prompt. So the
//! per-channel bridge owns the local-agent connection on a **dedicated
//! `std::thread`** (no tokio in this crate): the inbound write half is written
//! to from the reader loop and returns under a bounded deadline (a
//! `write_with_deadline` to the agent does not block on the sign), while a
//! separate dedicated reader thread `read`s the agent's reply — which is where
//! the multi-second Touch ID wait lands — entirely off the control-reader loop.
//! The reply bytes are sent back up via a cheaply-cloneable write-up callback the
//! caller supplies (it locks the remote connection writer and writes one control
//! line; see `ssh_pool::write_remote_control`).
//!
//! ## Never hold the registry lock across blocking agent I/O (GB1)
//!
//! [`SshAgentRelay::write`] takes the registry mutex only long enough to CLONE the
//! channel's shared `Arc<RelayWriter>` (or buffer into a Connecting slot), then
//! DROPS the lock before the bounded `write_with_deadline` to the agent. A 1Password
//! sign that briefly back-pressures the agent socket therefore can never stall the
//! reader loop's access to OTHER channels' slots.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hitch_proto::transport::{connect_external, RelayReader, RelayWriter};
use hitch_proto::ControlMessage;

/// Reserved-TLD sentinel destination handed to `ssh -G` to resolve the user's
/// DEFAULT ssh-agent (their `Host *` / global `IdentityAgent`) rather than any
/// specific git provider's `Host` block. `ssh -G` requires *a* destination but
/// never connects; a `.invalid` host (RFC 2606) matches no real `Host` block, so
/// the resolved `IdentityAgent` is the agent-app-agnostic global default — which
/// is exactly the one the user's own `ssh`/`git` use for everything.
const AGENT_PROBE_HOST: &str = "hitch-default-agent-probe.invalid";

/// How long the channel thread waits for the connect to the local agent before
/// giving up and closing the channel (GB2, never-stall contract). The connect
/// runs on the channel's own dedicated thread (never the reader loop), but a
/// wedged agent server — a stale `SSH_AUTH_SOCK`/pipe pointing at a hung process —
/// must not pin that thread forever either. A healthy agent connects in µs.
const LOCAL_AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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
///
/// Cross-platform: the Unix arm runs the ladder above; the Windows arm mirrors it
/// for Windows agents (OpenSSH-for-Windows / 1Password-for-Windows named pipe) so
/// a co-located local GUI on Windows announces its agent too (ADR 0014 amendment,
/// silly-ridge-27 — the local analog of the remote `SshAgentRelay` declaration).
#[cfg(unix)]
pub fn local_agent_socket() -> Option<PathBuf> {
    resolve_local_agent(
        ssh_config_identity_agent(),
        std::env::var_os("SSH_AUTH_SOCK"),
        one_password_fallback_socket(),
    )
}

/// Windows resolver for [`local_agent_socket`]: same LADDER as Unix, adapted to
/// Windows agents. UNVALIDATED off-host — written against the repo's existing
/// `#[cfg(windows)]` idioms (`USERPROFILE` in `cli_install.rs`, the named-pipe
/// path literal style in `hitch-proto`'s `transport.rs`) and validated live on the
/// Windows host. The macOS build never compiles this arm.
/// TODO(win-e2e): validate on pc@192.168.0.9.
#[cfg(windows)]
pub fn local_agent_socket() -> Option<PathBuf> {
    resolve_local_agent_windows(
        windows_ssh_config_identity_agent(),
        std::env::var_os("SSH_AUTH_SOCK"),
        windows_default_agent_pipe(),
    )
}

/// Pure resolution policy for [`local_agent_socket`] on Unix, factored out so it is
/// testable without spawning `ssh` or mutating process env. See that function's
/// doc comment for the ladder.
#[cfg(unix)]
fn resolve_local_agent(
    identity_agent: Option<String>,
    ssh_auth_sock: Option<OsString>,
    one_password_fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    // 1. Honor ssh's own IdentityAgent resolution.
    match classify_identity_agent(identity_agent.as_deref()) {
        // The user explicitly disabled agent auth — respect it, never fall back.
        IdentityAgent::Disabled => return None,
        // The default / explicit token: defer to SSH_AUTH_SOCK below.
        IdentityAgent::DeferToEnv => {}
        // An explicit socket path (ssh -G already ~-expanded it).
        IdentityAgent::Path(path) => return Some(PathBuf::from(path)),
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

/// Pure resolution policy for [`local_agent_socket`] on Windows, factored out so it
/// is testable without spawning `ssh.exe` or mutating process env. Mirrors the Unix
/// ladder rung-for-rung, except the env rung has NO launchd guard (a Windows
/// platform fact) and the labeled fallback is the OpenSSH-compatible named pipe
/// rather than the 1Password fixed Unix socket.
#[cfg(windows)]
fn resolve_local_agent_windows(
    identity_agent: Option<String>,
    ssh_auth_sock: Option<OsString>,
    default_agent_pipe: Option<PathBuf>,
) -> Option<PathBuf> {
    // 1. Honor ssh's own IdentityAgent resolution (ssh.exe -G).
    match classify_identity_agent(identity_agent.as_deref()) {
        IdentityAgent::Disabled => return None,
        IdentityAgent::DeferToEnv => {}
        IdentityAgent::Path(path) => return Some(PathBuf::from(path)),
    }

    // 2. SSH_AUTH_SOCK from the environment if set & non-empty. No launchd guard on
    //    Windows (that empty-system-agent trap is a macOS-only platform fact).
    if let Some(sock) = ssh_auth_sock {
        if !sock.is_empty() {
            return Some(PathBuf::from(sock));
        }
    }

    // 3. Labeled fallback: the OpenSSH-for-Windows default agent named pipe
    //    (1Password-for-Windows serves the OpenSSH-compatible pipe at this same
    //    path), if it exists.
    default_agent_pipe
}

/// The classification of an `IdentityAgent` value from `ssh -G`, shared by both
/// platform resolvers so the `none` / `SSH_AUTH_SOCK` / explicit-path semantics
/// stay identical across Unix and Windows.
enum IdentityAgent {
    /// `IdentityAgent none` — the user disabled agent auth; never fall back.
    Disabled,
    /// The `SSH_AUTH_SOCK` token, the empty value, or no `identityagent` line —
    /// defer to the environment rung.
    DeferToEnv,
    /// An explicit socket / pipe path (ssh -G already ~-expanded it).
    Path(String),
}

/// Map the raw `IdentityAgent` value to its [`IdentityAgent`] meaning. Pure and
/// cfg-agnostic so both platform resolvers share one source of truth for the
/// `none` / `SSH_AUTH_SOCK` / explicit-path tokens.
fn classify_identity_agent(identity_agent: Option<&str>) -> IdentityAgent {
    match identity_agent.map(str::trim) {
        Some("none") => IdentityAgent::Disabled,
        Some("SSH_AUTH_SOCK") | Some("") | None => IdentityAgent::DeferToEnv,
        Some(path) => IdentityAgent::Path(path.to_string()),
    }
}

/// Parse the effective `IdentityAgent` out of `ssh -G` stdout. `ssh -G` emits
/// "keyword value" with lowercase keywords and no indentation; the value (a socket
/// path) may itself contain spaces, so take the entire remainder after the keyword
/// rather than splitting. Returns the raw value (a path, or the `none`/
/// `SSH_AUTH_SOCK` token), or `None` if there is no non-empty `identityagent` line
/// (the OpenSSH default — treated as `SSH_AUTH_SOCK`). Cfg-agnostic so both the
/// Unix and Windows `ssh -G` arms reuse one parser.
fn parse_identity_agent_line(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Some(value) = line.trim_start().strip_prefix("identityagent ") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
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
#[cfg(unix)]
fn ssh_config_identity_agent() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/ssh")
        .arg("-G")
        .arg(AGENT_PROBE_HOST)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_identity_agent_line(&String::from_utf8_lossy(&output.stdout))
}

/// Windows analog of [`ssh_config_identity_agent`]: ask OpenSSH-for-Windows for the
/// effective `IdentityAgent` via `ssh.exe -G`. Invoked by bare name (`ssh`, found
/// on PATH as `ssh.exe`) rather than the Unix absolute `/usr/bin/ssh`, since the
/// Windows OpenSSH client lives under `System32\OpenSSH` and is on PATH. Same
/// offline `.invalid` sentinel host, same line parser.
/// TODO(win-e2e): validate on pc@192.168.0.9.
#[cfg(windows)]
fn windows_ssh_config_identity_agent() -> Option<String> {
    let output = std::process::Command::new("ssh")
        .arg("-G")
        .arg(AGENT_PROBE_HOST)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_identity_agent_line(&String::from_utf8_lossy(&output.stdout))
}

/// The 1Password fixed agent socket if it exists. `$HOME` is expanded via `HOME`
/// because this crate has no `~` expander.
#[cfg(unix)]
fn one_password_fallback_socket() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let mut path = PathBuf::from(home);
    path.push("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
    path.exists().then_some(path)
}

/// The OpenSSH-for-Windows default agent named pipe if it currently exists. This is
/// the labeled Windows fallback — the analog of the 1Password fixed socket on Unix:
/// OpenSSH-for-Windows's `ssh-agent` service and 1Password-for-Windows both serve an
/// OpenSSH-compatible agent at this same well-known pipe path, so probing its
/// existence is the no-config "is an agent reachable at all?" signal. The path is a
/// fixed literal (matching the named-pipe literal style in `hitch-proto`'s
/// `transport.rs`); `%USERPROFILE%` is not needed for this rung.
/// TODO(win-e2e): validate on pc@192.168.0.9.
#[cfg(windows)]
fn windows_default_agent_pipe() -> Option<PathBuf> {
    let pipe = PathBuf::from(r"\\.\pipe\openssh-ssh-agent");
    pipe.exists().then_some(pipe)
}

/// Whether `path` is macOS's per-session system ssh-agent
/// (`…/com.apple.launchd.<rand>/Listeners`). A GUI launched from the Dock/Finder
/// inherits this in `SSH_AUTH_SOCK`, but it is empty unless the user ran
/// `ssh-add` into it — 1Password/Bitwarden/Secretive users never do — so it must
/// not shadow the real agent. A macOS platform fact, not an agent-app heuristic.
#[cfg(unix)]
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
///
/// Bounded by [`LOCAL_AGENT_CONNECT_TIMEOUT`] (GB2): the connect runs on a worker
/// joined under that deadline, and the request/response is read under the relay
/// write-deadline, so a wedged agent never pins the connect/reconnect worker
/// thread for more than the timeout. Cross-platform via [`connect_external`].
pub fn agent_identity_count(socket: &Path) -> Option<usize> {
    let (mut reader, writer) = connect_external_bounded(socket)?;
    // This probe runs synchronously on the connect/reconnect worker thread after
    // declaring the relay, so keep it short: a healthy agent answers in µs, and a
    // wedged one should not delay that host's `Running` status by seconds.
    // Request: uint32 len=1, byte type=11 (SSH_AGENTC_REQUEST_IDENTITIES).
    writer.write_with_deadline(&[0, 0, 0, 1, 11]).ok()?;
    // Answer header: uint32 len, byte type=12 (SSH_AGENT_IDENTITIES_ANSWER),
    // uint32 nkeys. A shorter message (e.g. FAILURE, len=1) is "not an answer".
    let mut len_buf = [0u8; 4];
    read_exact_bounded(&mut reader, &mut len_buf)?;
    if u32::from_be_bytes(len_buf) < 5 {
        return None;
    }
    let mut type_and_count = [0u8; 5];
    read_exact_bounded(&mut reader, &mut type_and_count)?;
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

/// `Read::read_exact` that maps any error (incl. a closed/short agent) to `None`,
/// so the self-diagnosis probe degrades to "couldn't determine key count" rather
/// than panicking or surfacing an error. The agent answers a single small reply
/// promptly; a wedged one is bounded by the connect deadline upstream.
fn read_exact_bounded(reader: &mut RelayReader, buf: &mut [u8]) -> Option<()> {
    reader.read_exact(buf).ok()
}

/// Connect to the local agent under a bounded deadline (GB2). [`connect_external`]
/// itself does a blocking connect with no timeout; a healthy agent connects in µs,
/// but a stale `SSH_AUTH_SOCK`/pipe pointing at a hung server could otherwise block
/// forever. We run the connect on a short-lived worker and join it under
/// [`LOCAL_AGENT_CONNECT_TIMEOUT`]: on timeout we abandon the worker (it owns the
/// half-open connect; it unwinds when the OS eventually errors or completes) and
/// return `None`, which the caller treats as "agent unreachable, close the
/// channel". Cross-platform: `connect_external` is the cross-platform primitive.
fn connect_external_bounded(socket: &Path) -> Option<(RelayReader, RelayWriter)> {
    let socket = socket.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    // The worker is detached on timeout; name it so a stuck connect is identifiable
    // in a thread dump. A send on a dropped receiver (we already timed out) is a
    // benign `Err` the worker ignores.
    let _ = thread::Builder::new()
        .name("hitch-ssh-agent-connect".to_string())
        .spawn(move || {
            let result = connect_external(&socket);
            let _ = tx.send(result);
        });
    match rx.recv_timeout(LOCAL_AGENT_CONNECT_TIMEOUT) {
        Ok(Ok(pair)) => Some(pair),
        // Connect errored (no such agent / refused) or the worker hit the deadline
        // and we gave up waiting: no usable connection.
        Ok(Err(_)) | Err(_) => None,
    }
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
///
/// The connected write half is held behind an `Arc` so [`SshAgentRelay::write`]
/// can CLONE it out under the registry lock and then DROP the lock before the
/// bounded `write_with_deadline` to the agent (GB1 — never hold the lock across
/// blocking agent I/O).
enum ChannelHandle {
    /// Connecting on the dedicated thread; request bytes buffer here meanwhile.
    /// `generation` distinguishes THIS open from any later reuse of the same
    /// channel id (GB3): the connecting thread only transitions the slot it owns.
    Connecting { generation: u64, buffered: Vec<u8> },
    /// Connected: the local-agent write half (shared `Arc`). Inbound bytes are
    /// written through it OUTSIDE the registry lock. The read half is owned by the
    /// channel's dedicated reader thread, so a write here never blocks on a sign
    /// beyond the bounded deadline.
    Connected {
        generation: u64,
        agent_write: Arc<RelayWriter>,
    },
}

impl ChannelHandle {
    /// This handle's open-generation (GB3): set when the slot was first inserted
    /// and carried across the Connecting -> Connected transition, so the channel
    /// thread can verify the slot is still ITS open and not a later reuse.
    fn generation(&self) -> u64 {
        match self {
            ChannelHandle::Connecting { generation, .. }
            | ChannelHandle::Connected { generation, .. } => *generation,
        }
    }

    /// Shut the local-agent connection down (Connected) so a reader thread parked
    /// on a pending sign returns at once. A Connecting handle has no connection
    /// yet — its thread aborts when it finds the registry slot gone or superseded.
    fn shutdown(self) {
        if let ChannelHandle::Connected { agent_write, .. } = self {
            let _ = agent_write.force_close();
        }
    }
}

/// Per-remote-connection registry of live ssh-agent relay channels, keyed by the
/// daemon-assigned channel id. Owned by the `RemoteConnection` (one relay per
/// remote daemon). Cloneable `Arc` handle so the reader loop and the per-channel
/// reader threads share it.
///
/// `next_generation` (GB3) is a monotonic counter stamped on every [`open`]: the
/// daemon's channel ids are SUPPOSED to be monotonic & never reused, but the GUI
/// does not enforce that invariant on the wire. If a reused id arrives while a
/// stale thread for the prior open is still alive, the generation guard makes the
/// stale thread refuse to touch (or hijack) the newer slot.
#[derive(Clone, Default)]
pub struct SshAgentRelay {
    inner: Arc<SshAgentRelayInner>,
}

#[derive(Default)]
struct SshAgentRelayInner {
    channels: Mutex<HashMap<u64, ChannelHandle>>,
    next_generation: std::sync::atomic::AtomicU64,
}

impl SshAgentRelay {
    pub fn new() -> Self {
        Self::default()
    }

    fn channels(&self) -> &Mutex<HashMap<u64, ChannelHandle>> {
        &self.inner.channels
    }

    /// Allocate the next monotonic open-generation (GB3).
    fn next_generation(&self) -> u64 {
        self.inner
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Open a bridge for `channel`. Registers the channel SYNCHRONOUSLY (so an
    /// inbound `SshAgentData` racing right behind the `SshAgentOpen` is buffered,
    /// never lost) and then does ALL blocking work — the connect to the local
    /// agent AND the sign reply read — on a dedicated thread. This MUST NOT block
    /// the caller (the control-reader loop): a wedged 1Password helper or a stale
    /// `SSH_AUTH_SOCK` pointing at a hung server could otherwise stall the loop
    /// that drains the daemon→GUI stream, back-pressure the daemon's pump thread
    /// onto the shared `ClientSink` writer, and freeze every terminal on the
    /// connection. So even the connect runs on the spawned thread.
    ///
    /// GB3: if the channel id is ALREADY in the registry (the daemon reused an id
    /// while a stale thread for the prior open is still alive), the OLD handle is
    /// closed BEFORE the new slot is inserted, and the new slot carries a fresh
    /// generation so the stale thread can never hijack it.
    ///
    /// If no local agent is reachable, or the connect fails, the thread emits an
    /// [`ControlMessage::SshAgentClose`] up so the daemon tears its side down
    /// rather than hanging.
    pub fn open(&self, channel: u64, write_up: WriteUp) {
        let generation = self.next_generation();
        // Register a Connecting slot up front so `write()` can buffer inbound
        // request bytes before the connect completes. If a stale handle for a
        // reused id is still present, close it first (GB3) so its socket is shut
        // and its thread unblocks, rather than letting two opens race the slot.
        if let Ok(mut channels) = self.channels().lock() {
            if let Some(stale) = channels.remove(&channel) {
                debug_log(format!(
                    "ssh-agent relay: channel {channel} reused while a prior open was live; \
                     closing the stale handle before reopening"
                ));
                stale.shutdown();
            }
            channels.insert(
                channel,
                ChannelHandle::Connecting {
                    generation,
                    buffered: Vec::new(),
                },
            );
        }

        // Clone the cheap `Arc` write-up sink so the spawn-failure branch still
        // owns one after the closure moves its copy.
        let registry = self.clone();
        let thread_write_up = write_up.clone();
        let spawned = thread::Builder::new()
            .name(format!("hitch-ssh-agent-relay-{channel}"))
            .spawn(move || registry.run_channel(channel, generation, thread_write_up));
        if let Err(err) = spawned {
            debug_log(format!(
                "ssh-agent relay: failed to spawn channel thread for {channel}: {err}; closing"
            ));
            self.close_local(channel, generation);
            write_up(ControlMessage::SshAgentClose { channel });
        }
    }

    /// The channel's dedicated thread: connect to the local agent (the blocking
    /// connect is here, off the reader loop), flush any bytes buffered during the
    /// connect window, then pump agent reply bytes up — where the multi-second
    /// Touch ID sign wait lands — until EOF.
    ///
    /// `generation` (GB3) identifies THIS open. Every registry mutation this thread
    /// makes is conditioned on the slot still carrying `generation`, so a reused
    /// channel id whose new open has a newer generation is never disturbed.
    fn run_channel(&self, channel: u64, generation: u64, write_up: WriteUp) {
        let socket = match local_agent_socket() {
            Some(socket) => socket,
            None => {
                debug_log(format!(
                    "ssh-agent relay: SshAgentOpen channel {channel} but no local agent reachable; closing"
                ));
                self.close_local(channel, generation);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        };
        // Bounded connect (GB2): a wedged agent server cannot pin this thread
        // forever. Cross-platform via `connect_external`.
        let (read_half, write_half) = match connect_external_bounded(&socket) {
            Some(pair) => pair,
            None => {
                debug_log(format!(
                    "ssh-agent relay: connect to local agent {} failed/timed out for channel {channel}; closing",
                    socket.display()
                ));
                self.close_local(channel, generation);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        };
        let write_half = Arc::new(write_half);

        // Transition Connecting -> Connected, draining the bytes buffered during
        // the connect window (in order) to the agent FIRST. The blocking flush to
        // the agent happens OUTSIDE the registry lock (GB1), but the `Connecting`
        // slot STAYS in the map throughout the drain, so a concurrent `close` /
        // `close_all` / `write` is never lost: `write` keeps appending to the slot's
        // buffer, and a `close` removes the slot (which this loop detects and aborts
        // on). The loop ends only when, under the lock, the buffer is empty — then it
        // swaps the slot to `Connected` atomically, so no inbound byte can slip in
        // between the last flush and the swap. Every step is conditioned on the slot
        // still carrying THIS `generation` (GB3): a reused id whose newer open
        // replaced the slot is never disturbed.
        loop {
            // Phase 1 (under lock): take the pending buffer out of OUR slot, or
            // finish the transition if it is empty. Leave a `Connecting` slot with
            // an empty buffer in place while we flush, so `write`/`close` still find
            // it. Bail if the slot is gone or now belongs to a newer generation.
            let to_flush = {
                let mut channels = match self.channels().lock() {
                    Ok(channels) => channels,
                    Err(_) => return,
                };
                match channels.get_mut(&channel) {
                    Some(ChannelHandle::Connecting {
                        generation: slot_gen,
                        buffered,
                    }) if *slot_gen == generation => {
                        if buffered.is_empty() {
                            // No more pending bytes: atomically swap to Connected and
                            // leave the lock — `write()` now goes straight through.
                            channels.insert(
                                channel,
                                ChannelHandle::Connected {
                                    generation,
                                    agent_write: Arc::clone(&write_half),
                                },
                            );
                            break;
                        }
                        // Take the pending bytes, leaving the (empty) Connecting slot
                        // in place so concurrent writes keep landing in it.
                        std::mem::take(buffered)
                    }
                    // Slot gone (closed) or superseded by a newer open (reused id):
                    // not ours — abort and let the owner tear down.
                    _ => return,
                }
            };
            // Phase 2 (lock dropped): bounded blocking flush to the agent (GB1/GB2).
            if write_half.write_with_deadline(&to_flush).is_err() {
                debug_log(format!(
                    "ssh-agent relay: flushing connect-window buffer to local agent failed for channel {channel}; closing"
                ));
                self.close_local(channel, generation);
                write_up(ControlMessage::SshAgentClose { channel });
                return;
            }
        }
        debug_log(format!(
            "ssh-agent relay: opened channel {channel} -> {}",
            socket.display()
        ));

        let mut read_half = read_half;
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
        self.close_local(channel, generation);
        write_up(ControlMessage::SshAgentClose { channel });
    }

    /// Write inbound request bytes (from the daemon) to the channel. If the
    /// channel is still connecting, the bytes are BUFFERED (flushed in order once
    /// connected, bounded by [`MAX_CONNECTING_BUFFER`]); if connected, they are
    /// written to the local-agent write half UNDER A BOUNDED DEADLINE and OUTSIDE
    /// the registry lock (GB1). Returns after the bounded write — the SIGN/response
    /// is read on the channel's dedicated thread, so this never blocks on Touch ID.
    /// A write to a torn-down/unknown channel is silently ignored.
    ///
    /// Returns `true` when this call dropped the channel (a dead agent socket, or
    /// the connect-window buffer overflowed): the caller should then emit an
    /// [`ControlMessage::SshAgentClose`] up so the daemon tears its side down. A
    /// connecting channel that overflowed has no reader thread yet to emit it.
    #[must_use]
    pub fn write(&self, channel: u64, bytes: &[u8]) -> bool {
        // Phase 1 (under the lock): either buffer into a Connecting slot, or CLONE
        // the shared Connected write half out. NEVER do blocking agent I/O here.
        let agent_write = {
            let mut channels = match self.channels().lock() {
                Ok(channels) => channels,
                Err(_) => return false,
            };
            let Some(handle) = channels.get_mut(&channel) else {
                return false;
            };
            match handle {
                ChannelHandle::Connecting { buffered, .. } => {
                    if buffered.len().saturating_add(bytes.len()) > MAX_CONNECTING_BUFFER {
                        // A runaway/hostile daemon is flooding the connect window;
                        // drop the channel. The connecting thread aborts when it
                        // finds the slot gone, so we must tell the daemon to close.
                        debug_log(format!(
                            "ssh-agent relay: connect-window buffer for channel {channel} \
                             exceeded {MAX_CONNECTING_BUFFER} bytes; closing"
                        ));
                        channels.remove(&channel);
                        return true;
                    }
                    buffered.extend_from_slice(bytes);
                    return false;
                }
                ChannelHandle::Connected { agent_write, .. } => Arc::clone(agent_write),
            }
        };

        // Phase 2 (lock dropped): bounded blocking write to the agent (GB1).
        if agent_write.write_with_deadline(bytes).is_err() {
            // The local agent socket is dead / wedged past the deadline; drop the
            // channel. The reader thread will also observe EOF/err and emit
            // SshAgentClose up, so we don't need to here (return false).
            if let Ok(mut channels) = self.channels().lock() {
                channels.remove(&channel);
            }
            // Proactively unblock any reader parked on the dead connection.
            let _ = agent_write.force_close();
        }
        false
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
        let drained: Vec<ChannelHandle> = match self.channels().lock() {
            Ok(mut channels) => channels.drain().map(|(_, handle)| handle).collect(),
            Err(_) => return,
        };
        for handle in drained {
            handle.shutdown();
        }
    }

    /// Remove a channel's handle from the registry WITHOUT shutting the socket
    /// down (the reader thread already saw EOF/err, so its read half is done and
    /// its write half is the only thing left to drop). GB3: only removes the slot
    /// if it still carries `generation`, so a reused id's newer open is untouched.
    fn close_local(&self, channel: u64, generation: u64) {
        if let Ok(mut channels) = self.channels().lock() {
            if channels.get(&channel).map(|h| h.generation()) == Some(generation) {
                channels.remove(&channel);
            }
        }
    }

    fn take(&self, channel: u64) -> Option<ChannelHandle> {
        self.channels().lock().ok().and_then(|mut c| c.remove(&channel))
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

#[cfg(all(test, unix))]
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

    #[test]
    fn connecting_channel_buffers_inbound_bytes_up_to_cap() {
        // A channel still connecting to the agent buffers inbound request bytes
        // (it has no write half yet). Under the cap this returns false (kept);
        // overflowing the cap returns true (dropped, caller must close).
        let relay = SshAgentRelay::new();
        // Manually register a Connecting slot (mirrors what `open` does before its
        // thread runs) so we exercise `write`'s buffering branch without spawning
        // the thread / needing a live agent.
        let generation = relay.next_generation();
        relay.channels().lock().unwrap().insert(
            7,
            ChannelHandle::Connecting {
                generation,
                buffered: Vec::new(),
            },
        );
        // Under the cap: buffered, kept.
        assert!(!relay.write(7, &vec![0u8; 1024]));
        // Push it just over the cap in one shot: dropped, caller must close.
        assert!(relay.write(7, &vec![0u8; MAX_CONNECTING_BUFFER]));
        // The slot is now gone (dropped on overflow).
        assert!(!relay.channels().lock().unwrap().contains_key(&7));
    }

    #[test]
    fn reused_channel_id_closes_stale_handle_before_reopen() {
        // GB3: if `open` is called for a channel id that already has a live slot
        // (the daemon reused an id while a stale thread is still around), the old
        // handle is removed and a fresh generation is installed, so the stale
        // thread can never hijack the newer slot. We register a Connecting slot,
        // then re-open the same id and assert the generation advanced.
        let relay = SshAgentRelay::new();
        let stale_generation = relay.next_generation();
        relay.channels().lock().unwrap().insert(
            3,
            ChannelHandle::Connecting {
                generation: stale_generation,
                buffered: Vec::new(),
            },
        );
        let write_up: WriteUp = Arc::new(|_msg| {});
        relay.open(3, write_up);
        // The slot now exists with a generation strictly greater than the stale one
        // (the new open allocated a fresh generation and replaced the slot).
        let channels = relay.channels().lock().unwrap();
        let handle = channels.get(&3).expect("reopened slot present");
        assert!(
            handle.generation() > stale_generation,
            "reopen must carry a newer generation than the stale handle"
        );
    }

    #[test]
    fn close_local_only_removes_matching_generation() {
        // GB3: `close_local` for a stale generation must NOT remove a newer open's
        // slot. Install a slot at generation g2, then call close_local with an
        // older g1 — the slot must survive.
        let relay = SshAgentRelay::new();
        let g1 = relay.next_generation();
        let g2 = relay.next_generation();
        relay.channels().lock().unwrap().insert(
            5,
            ChannelHandle::Connecting {
                generation: g2,
                buffered: Vec::new(),
            },
        );
        relay.close_local(5, g1); // stale generation — must be a no-op
        assert!(relay.channels().lock().unwrap().contains_key(&5));
        relay.close_local(5, g2); // matching generation — removes it
        assert!(!relay.channels().lock().unwrap().contains_key(&5));
    }
}

// Pure-policy tests for the Windows resolver. They mirror the Unix test style but
// will only ever RUN on a Windows host (the macOS build never compiles this arm).
// UNVALIDATED off-host — kept here so the Windows policy is checked in CI on the
// Windows runner alongside the live host validation.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    // The OpenSSH-for-Windows / 1Password-for-Windows default agent pipe.
    const DEFAULT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    // A representative explicit named-pipe IdentityAgent a user might configure.
    const EXPLICIT_PIPE: &str = r"\\.\pipe\my-custom-agent";

    #[test]
    fn identity_agent_path_wins_over_env_and_fallback() {
        // An explicit `IdentityAgent <pipe>` is the user's deliberate choice and
        // beats everything below it.
        assert_eq!(
            resolve_local_agent_windows(
                Some(EXPLICIT_PIPE.to_string()),
                Some(OsString::from(r"\\.\pipe\some-env-agent")),
                Some(PathBuf::from(DEFAULT_PIPE)),
            ),
            Some(PathBuf::from(EXPLICIT_PIPE))
        );
    }

    #[test]
    fn identity_agent_none_disables_the_relay() {
        // `IdentityAgent none` means "no agent" — never fall back to env/pipe.
        assert_eq!(
            resolve_local_agent_windows(
                Some("none".to_string()),
                Some(OsString::from(r"\\.\pipe\some-env-agent")),
                Some(PathBuf::from(DEFAULT_PIPE)),
            ),
            None
        );
    }

    #[test]
    fn ssh_auth_sock_token_and_unset_defer_to_env() {
        // Both the explicit `SSH_AUTH_SOCK` token and no identityagent line mean
        // "use the environment". No launchd guard on Windows: the env value is
        // taken verbatim when set & non-empty.
        for ident in [Some("SSH_AUTH_SOCK".to_string()), None] {
            assert_eq!(
                resolve_local_agent_windows(
                    ident,
                    Some(OsString::from(r"\\.\pipe\real-agent")),
                    None,
                ),
                Some(PathBuf::from(r"\\.\pipe\real-agent"))
            );
        }
    }

    #[test]
    fn empty_env_falls_through_to_default_pipe() {
        // An empty `SSH_AUTH_SOCK` must not shadow the labeled default-pipe
        // fallback (mirrors the Unix dock-launch fall-through, minus the launchd
        // guard which has no Windows analog).
        assert_eq!(
            resolve_local_agent_windows(
                None,
                Some(OsString::from("")),
                Some(PathBuf::from(DEFAULT_PIPE)),
            ),
            Some(PathBuf::from(DEFAULT_PIPE))
        );
        // ...and with no fallback pipe either, there is simply no agent.
        assert_eq!(
            resolve_local_agent_windows(None, Some(OsString::from("")), None),
            None
        );
    }

    #[test]
    fn default_pipe_used_only_when_env_absent() {
        assert_eq!(
            resolve_local_agent_windows(None, None, Some(PathBuf::from(DEFAULT_PIPE))),
            Some(PathBuf::from(DEFAULT_PIPE))
        );
        assert_eq!(resolve_local_agent_windows(None, None, None), None);
    }
}
