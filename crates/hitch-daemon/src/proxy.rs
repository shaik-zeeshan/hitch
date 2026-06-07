//! `hitch daemon proxy` — the remote-side SSH stdio bridge (issue #27, ADR 0014).
//!
//! When the desktop GUI attaches to an **SSH Host** it runs
//! `ssh -o BatchMode=yes <target> hitch daemon proxy`. That remote command is
//! THIS code. It:
//!
//! 1. resolves the host-local daemon endpoint exactly as a local GUI would
//!    (`hitch_proto::transport::default_socket_path`), auto-starting a detached
//!    daemon if none is listening (reusing the same `--detach` spawn the GUI
//!    uses locally),
//! 2. then bridges raw bytes bidirectionally between its own stdin/stdout and the
//!    daemon socket — a *dumb pipe* for the existing framing (newline-JSON control
//!    lines + length-prefixed PTY frames). It NEVER parses or rewrites a frame:
//!    the GUI performs the Hello handshake end-to-end with the real daemon through
//!    this pipe, so the proxy must stay protocol-agnostic.
//!
//! The hard rule is that **nothing but daemon bytes may ever reach stdout** — the
//! protocol stream the GUI reads. Auto-start spawn prints a pid today
//! (`detach_spawn`), so the proxy captures that child's stdout/stderr instead of
//! letting it inherit ours. All proxy diagnostics go to stderr, which SSH keeps
//! separate from the protocol stream (the GUI reads it only for failure copy).
//!
//! ## Binary naming
//!
//! ADR 0014 requires the remote host expose a single `hitch` CLI with a
//! `daemon proxy` subcommand. Hitch's crate structure (ADR 0005) makes the
//! daemon binary the sole composer; there is no separate unified `hitch` binary
//! today. So the daemon binary itself grows the `daemon proxy` subcommand, and
//! the install expectation is that it is available on the remote login PATH as
//! `hitch` (a copy or symlink of `hitch-daemon`). `hitch-hook` is the only other
//! installed binary and is resolved as a sibling of the daemon exe, so a
//! `hitch`-named daemon finds `hitch-hook` beside it unchanged.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hitch_proto::transport::{connect_daemon, default_socket_path, endpoint_accepts_connections};

/// How long to wait for an auto-started daemon to begin accepting connections
/// before giving up. Generous enough for a cold daemon (store open, layout
/// restore) on a slow remote, tight enough that a daemon that never binds fails
/// the proxy promptly rather than hanging the GUI's attach.
const DAEMON_START_DEADLINE: Duration = Duration::from_secs(10);

/// Poll interval while waiting for the auto-started daemon's endpoint to come up.
const DAEMON_START_POLL: Duration = Duration::from_millis(50);

/// Run the stdio proxy: resolve/auto-start the host-local daemon, then bridge
/// stdin/stdout to the daemon socket until either side closes. Returns the
/// process exit code the caller should use. Diagnostics go to stderr only.
pub fn run() -> i32 {
    match run_inner() {
        Ok(()) => 0,
        Err(err) => {
            // stderr is free for proxy diagnostics — SSH keeps it off the protocol
            // stream, and the GUI's classifier reads it for failure copy.
            eprintln!("hitch daemon proxy: {err}");
            1
        }
    }
}

fn run_inner() -> Result<(), String> {
    let socket_path = resolve_socket_path();
    let stream = connect_or_autostart(&socket_path)?;
    bridge_stdio(stream)
}

/// Resolve the host-local daemon endpoint the proxy attaches to. Honors the
/// `HITCH_SOCKET` override (the same env var the hook uses, ADR 0012) so the
/// remote endpoint resolution stays consistent across hitch binaries on the
/// host, and so integration tests can point the proxy at a test daemon. Falls
/// back to the per-user/per-instance default.
fn resolve_socket_path() -> PathBuf {
    if let Some(value) = std::env::var_os("HITCH_SOCKET") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    default_socket_path()
}

/// Connect to the host-local daemon, auto-starting a detached one if no endpoint
/// is currently accepting connections. Mirrors the GUI's local connect+spawn
/// logic, but the spawn's pid line is captured so it can never reach the proxy's
/// stdout (the protocol stream).
fn connect_or_autostart(socket_path: &Path) -> Result<hitch_proto::transport::DaemonStream, String> {
    // Fast path: a daemon is already listening.
    if let Ok(stream) = connect_daemon(socket_path) {
        eprintln!("hitch daemon proxy: attached to existing daemon at {}", socket_path.display());
        return Ok(stream);
    }

    // No daemon listening — spawn one detached, exactly as the local GUI does.
    spawn_detached_daemon(socket_path)?;

    // Wait for the freshly spawned daemon to begin accepting connections.
    let deadline = Instant::now() + DAEMON_START_DEADLINE;
    loop {
        match connect_daemon(socket_path) {
            Ok(stream) => {
                eprintln!(
                    "hitch daemon proxy: started and attached to daemon at {}",
                    socket_path.display()
                );
                return Ok(stream);
            }
            Err(_) if endpoint_accepts_connections(socket_path) => {
                // The endpoint exists but every instance is momentarily busy
                // (Windows ERROR_PIPE_BUSY) — a live daemon; retry.
            }
            Err(_) if Instant::now() < deadline => {}
            Err(err) => {
                return Err(format!(
                    "daemon did not start accepting connections at {} within {:?}: {err}",
                    socket_path.display(),
                    DAEMON_START_DEADLINE
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon did not start accepting connections at {} within {:?}",
                socket_path.display(),
                DAEMON_START_DEADLINE
            ));
        }
        thread::sleep(DAEMON_START_POLL);
    }
}

/// Spawn a detached host-local daemon by re-executing this binary with
/// `--detach`. The detach shim prints the spawned pid to ITS stdout (see
/// `detach_spawn` in main.rs); we capture that here so it never pollutes the
/// proxy's stdout — which is the protocol stream the GUI reads. stderr is
/// captured too and surfaced as a proxy diagnostic on failure.
fn spawn_detached_daemon(socket_path: &Path) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("cannot resolve hitch executable to auto-start the daemon: {err}"))?;
    let mut command = Command::new(&exe);
    command
        .arg("--detach")
        // Bind exactly the endpoint the proxy resolved (honoring HITCH_SOCKET), so
        // the spawned daemon and the proxy's subsequent connect rendezvous on the
        // same socket. `--detach` itself ignores HITCH_SOCKET, so pass it through.
        .arg("--socket")
        .arg(socket_path);
    // Pass through optional store / managed-root overrides when present. These are
    // unset in production (the daemon uses its per-instance defaults) but let an
    // integration test point an auto-started daemon at isolated paths so it never
    // races a real daemon's store. The daemon binary takes them as flags, not env,
    // so the proxy forwards the env values onto the flags here.
    if let Some(store) = std::env::var_os("HITCH_STORE").filter(|v| !v.is_empty()) {
        command.arg("--store").arg(store);
    }
    if let Some(root) = std::env::var_os("HITCH_MANAGED_ROOT").filter(|v| !v.is_empty()) {
        command.arg("--managed-root").arg(root);
    }
    let output = command
        .stdin(Stdio::null())
        // Capture stdout (the pid line) and stderr so neither reaches the proxy's
        // own stdout/stderr inheritance and corrupts the protocol stream.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("failed to spawn detached daemon ({}): {err}", exe.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "detached daemon spawn exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    // The detach shim's pid line is captured (not forwarded). Log it to stderr for
    // diagnostics so a flaky auto-start leaves a trail without touching stdout.
    let pid = String::from_utf8_lossy(&output.stdout);
    eprintln!("hitch daemon proxy: auto-started detached daemon (pid {})", pid.trim());
    Ok(())
}

/// Bridge raw bytes between this process's stdin/stdout and the daemon socket.
///
/// Two threads: stdin→socket and socket→stdout. The proxy never inspects the
/// bytes (newline-JSON control + length-prefixed PTY frames flow through
/// verbatim). It exits as soon as EITHER direction closes — when the GUI's SSH
/// channel closes its stdin reaches EOF, and when the daemon socket closes the
/// socket reader reaches EOF. Either is a normal teardown.
fn bridge_stdio(daemon: hitch_proto::transport::DaemonStream) -> Result<(), String> {
    let socket_reader = daemon
        .try_clone()
        .map_err(|err| format!("failed to clone daemon socket for bridging: {err}"))?;
    let socket_writer = daemon;

    // socket → stdout: the daemon's frames go straight to the GUI.
    let to_stdout = thread::Builder::new()
        .name("hitch-proxy-socket-to-stdout".into())
        .spawn(move || copy_until_eof(socket_reader, io::stdout()))
        .map_err(|err| format!("failed to spawn socket→stdout bridge thread: {err}"))?;

    // stdin → socket: the GUI's frames go straight to the daemon. Runs on this
    // thread so the process stays alive until stdin closes (GUI dropped the SSH
    // channel) — at which point we drop the socket writer to signal the daemon.
    let stdin_result = copy_until_eof(io::stdin(), socket_writer);

    // Once stdin closes, the socket writer is dropped (end of this scope), which
    // closes the daemon's read side of this connection. The socket→stdout thread
    // then sees EOF and exits. Join it so the process doesn't exit out from under
    // a final flush.
    let _ = to_stdout.join();
    stdin_result
}

/// Copy `reader` to `writer` until EOF, flushing as it goes. A read/write error
/// or EOF on either side ends the copy; the caller treats that as normal
/// teardown (one side of the bridge closed). Returns Ok on clean EOF, Err on an
/// I/O error worth logging.
fn copy_until_eof<R: Read, W: Write>(mut reader: R, mut writer: W) -> Result<(), String> {
    let mut buf = [0_u8; 32 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if writer.write_all(&buf[..n]).is_err() {
                    // The far side closed mid-write — normal teardown.
                    return Ok(());
                }
                if writer.flush().is_err() {
                    return Ok(());
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(format!("bridge copy failed: {err}")),
        }
    }
}

/// True when this process's argv requests the `daemon proxy` subcommand. The
/// daemon binary doubles as the remote `hitch`, so `hitch daemon proxy` lands
/// here. Accepts trailing args after `proxy` (ignored) so a future flag does not
/// break the dispatch.
pub fn is_proxy_invocation(args: &[String]) -> bool {
    matches!(args, [first, second, ..] if first == "daemon" && second == "proxy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_daemon_proxy_subcommand() {
        assert!(is_proxy_invocation(&[
            "daemon".to_string(),
            "proxy".to_string()
        ]));
        // Trailing args after `proxy` are tolerated.
        assert!(is_proxy_invocation(&[
            "daemon".to_string(),
            "proxy".to_string(),
            "--verbose".to_string()
        ]));
    }

    #[test]
    fn rejects_non_proxy_invocations() {
        assert!(!is_proxy_invocation(&[]));
        assert!(!is_proxy_invocation(&["daemon".to_string()]));
        assert!(!is_proxy_invocation(&["proxy".to_string()]));
        assert!(!is_proxy_invocation(&[
            "daemon".to_string(),
            "status".to_string()
        ]));
        // The daemon's own flags must not be mistaken for the subcommand.
        assert!(!is_proxy_invocation(&["--detach".to_string()]));
        assert!(!is_proxy_invocation(&[
            "--socket".to_string(),
            "/tmp/x.sock".to_string()
        ]));
    }

    #[test]
    fn copy_until_eof_streams_all_bytes_then_stops_on_eof() {
        let input = b"hello\nworld\x00\x01\x02".to_vec();
        let reader = std::io::Cursor::new(input.clone());
        let mut output: Vec<u8> = Vec::new();
        copy_until_eof(reader, &mut output).unwrap();
        assert_eq!(output, input);
    }
}
