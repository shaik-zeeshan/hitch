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
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use hitch_proto::ControlMessage;

/// Resolve the local ssh-agent socket to bridge to. Used BOTH as the capability
/// gate (is an agent reachable at all? — drives whether the GUI even declares the
/// relay) and as the connect target when the daemon opens a channel.
///
/// Resolution ladder:
/// 1. `SSH_AUTH_SOCK` if set and non-empty (the OpenSSH/forwarded-agent default,
///    and what `ssh-add` itself honors), else
/// 2. the 1Password fixed socket `$HOME/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock`
///    IF it exists — 1Password does NOT export `SSH_AUTH_SOCK` itself, so many
///    Macs only have this path, else
/// 3. `None` (no reachable agent; the relay is not declared and any stray
///    `SshAgentOpen` is closed immediately).
///
/// `$HOME` is expanded via `HOME` because this crate has no `~` expander (the
/// only precedent for reading agent env is the daemon proxy reading
/// `SSH_AUTH_SOCK`).
pub fn local_agent_socket() -> Option<PathBuf> {
    if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") {
        if !sock.is_empty() {
            return Some(PathBuf::from(sock));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    let mut path = PathBuf::from(home);
    path.push("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
    if path.exists() {
        Some(path)
    } else {
        None
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
    /// connected); if connected, they are written straight to the local-agent
    /// write half. Returns immediately — the SIGN/response is read on the
    /// channel's dedicated thread, so this never blocks on Touch ID. A write to a
    /// torn-down/unknown channel is silently ignored.
    pub fn write(&self, channel: u64, bytes: &[u8]) {
        let mut channels = match self.channels.lock() {
            Ok(channels) => channels,
            Err(_) => return,
        };
        let Some(handle) = channels.get_mut(&channel) else {
            return;
        };
        match handle {
            ChannelHandle::Connecting { buffered } => buffered.extend_from_slice(bytes),
            ChannelHandle::Connected { agent_write } => {
                if agent_write.write_all(bytes).is_err() || agent_write.flush().is_err() {
                    // The local agent socket is dead; drop the channel. The reader
                    // thread will also observe EOF/err and emit SshAgentClose up.
                    channels.remove(&channel);
                }
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

    #[test]
    fn local_agent_socket_prefers_non_empty_ssh_auth_sock() {
        // This test mutates process env, so keep it self-contained and restore.
        let prev = std::env::var_os("SSH_AUTH_SOCK");
        std::env::set_var("SSH_AUTH_SOCK", "/tmp/some-agent.sock");
        assert_eq!(
            local_agent_socket(),
            Some(PathBuf::from("/tmp/some-agent.sock"))
        );
        match prev {
            Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
            None => std::env::remove_var("SSH_AUTH_SOCK"),
        }
    }

    #[test]
    fn write_to_unknown_channel_is_a_noop() {
        // No panic, no registration: writing to a channel that was never opened
        // (e.g. a stale frame after a reconnect invalidated the registry) is
        // silently ignored.
        let relay = SshAgentRelay::new();
        relay.write(99, b"\x00\x01\x02");
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
