//! `hitch daemon proxy` — the remote-side SSH stdio bridge (issue #27, ADR 0014).
//!
//! When the desktop GUI attaches to an **SSH Host** it runs
//! `ssh -o BatchMode=yes <target> hitch daemon proxy`. That remote command is
//! THIS code. It:
//!
//! 1. resolves the host-local daemon endpoint exactly as a local GUI would
//!    (`hitch_proto::transport::default_socket_path`) and connects to it. The
//!    proxy does NOT start the daemon: if no daemon is listening it fails fast and
//!    leaves the host untouched. Starting the host's daemon is the host's job (its
//!    own Hitch app or an explicit `hitch daemon`), keeping the SSH trust boundary
//!    explicit and the proxy a pure bridge (ADR 0014). A missing daemon surfaces to
//!    the GUI as a clear "no daemon running on the remote" failure, not a silent
//!    auto-spawn.
//! 2. then bridges raw bytes bidirectionally between its own stdin/stdout and the
//!    daemon socket — a *dumb pipe* for the existing framing (newline-JSON control
//!    lines + length-prefixed PTY frames). It NEVER parses or rewrites a frame:
//!    the GUI performs the Hello handshake end-to-end with the real daemon through
//!    this pipe, so the proxy must stay protocol-agnostic.
//!
//! The hard rule is that **nothing but daemon bytes may ever reach stdout** — the
//! protocol stream the GUI reads. All proxy diagnostics go to stderr, which SSH
//! keeps separate from the protocol stream (the GUI reads it only for failure copy).
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
use std::thread;

use hitch_proto::transport::{connect_daemon, default_socket_path};

/// Run the stdio proxy: connect to the host-local daemon, then bridge
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
    let stream = connect_existing(&socket_path)?;
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

/// Connect to the host-local daemon if one is listening. The proxy deliberately
/// does NOT start the daemon: when no endpoint is up it fails fast, leaving the
/// host untouched, so the SSH trust boundary stays explicit and starting the
/// host's daemon remains the host's own job (its Hitch app, or an explicit
/// `hitch daemon`). The error is phrased for the GUI's failure copy.
fn connect_existing(socket_path: &Path) -> Result<hitch_proto::transport::DaemonStream, String> {
    match connect_daemon(socket_path) {
        Ok(stream) => {
            eprintln!("hitch daemon proxy: attached to existing daemon at {}", socket_path.display());
            Ok(stream)
        }
        Err(err) => Err(format!(
            "no Hitch daemon is running at {} on this host. \
             The proxy does not start it; launch Hitch (or `hitch daemon`) on the remote and retry ({err})",
            socket_path.display()
        )),
    }
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
