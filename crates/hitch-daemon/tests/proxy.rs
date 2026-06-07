//! Integration tests for `hitch daemon proxy` — the SSH stdio bridge (issue #27,
//! ADR 0014). The proxy is the daemon binary invoked as `hitch daemon proxy`; it
//! resolves the host-local daemon endpoint (auto-starting one if absent) and
//! bridges raw protocol bytes between its stdin/stdout and the daemon socket.
//!
//! These tests run the real proxy binary as a subprocess with piped stdio and
//! drive the Hitch protocol Hello handshake (and a request round-trip) THROUGH
//! the proxy, asserting the proxy is a faithful dumb pipe for the existing
//! framing. They mirror `reattach.rs`'s daemon-lifecycle patterns.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hitch_proto::transport::connect_daemon;
use hitch_proto::{
    encode_control_message, ControlMessage, Request, Response, PROTOCOL_VERSION,
};

/// The Hello handshake completes end-to-end through the proxy against an
/// already-running daemon, and a follow-up Ping round-trips — proving the proxy
/// bridges the existing framing without parsing it.
#[test]
fn proxy_bridges_hello_and_ping_against_running_daemon() {
    let socket = test_socket_path("proxy-running");
    let mut daemon = DaemonGuard::start(&socket);
    wait_for_endpoint(&socket);

    let mut proxy = ProxyProcess::spawn(&socket);

    // Drive the protocol over the proxy's stdio, not the socket directly.
    proxy.send_request(1, Request::Hello {
        client_name: "proxy-test".into(),
        protocol_version: PROTOCOL_VERSION,
    });
    let hello = proxy.read_response_for(1);
    assert!(
        matches!(hello, Response::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION),
        "expected matching Hello through proxy, got {hello:?}"
    );

    proxy.send_request(2, Request::Ping);
    let pong = proxy.read_response_for(2);
    assert!(matches!(pong, Response::Pong), "expected Pong through proxy, got {pong:?}");

    // Closing the proxy's stdin tears the bridge down; the daemon survives (the
    // proxy is connection-scoped, ADR 0014). Verify the daemon still answers a
    // fresh direct connection after the proxy exits.
    proxy.shutdown();
    let mut direct = connect_daemon(&socket).expect("daemon still reachable after proxy exit");
    direct
        .send_control(&ControlMessage::request(
            1,
            Request::Hello {
                client_name: "post-proxy".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        ))
        .expect("send hello after proxy exit");
    let response = read_one_response(&mut direct, 1);
    assert!(
        matches!(response, Response::Hello { .. }),
        "daemon should outlive the proxy and answer a fresh client"
    );

    // Tidy: shut the daemon down through the direct connection.
    direct
        .send_control(&ControlMessage::request(2, Request::ShutdownDaemon))
        .expect("send shutdown");
    let _ = read_one_response(&mut direct, 2);
    daemon.wait_for_exit();
}

/// With NO daemon listening, the proxy auto-starts a detached daemon and bridges
/// the Hello handshake to it. Exercises the auto-start path (`--detach` spawn,
/// pid captured off stdout) end-to-end.
#[test]
fn proxy_auto_starts_daemon_and_bridges_hello() {
    let socket = test_socket_path("proxy-autostart");
    let store = test_file_path("proxy-autostart-store", "sqlite");
    let managed_root = test_dir_path("proxy-autostart-managed");
    // Ensure nothing is listening on the socket up front.
    assert!(
        connect_daemon(&socket).is_err(),
        "test socket must be free before auto-start"
    );

    let mut proxy = ProxyProcess::spawn_with_paths(&socket, &store, &managed_root);

    proxy.send_request(1, Request::Hello {
        client_name: "proxy-autostart".into(),
        protocol_version: PROTOCOL_VERSION,
    });
    let hello = proxy.read_response_for(1);
    assert!(
        matches!(hello, Response::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION),
        "expected matching Hello from auto-started daemon, got {hello:?}"
    );

    // The proxy must have started a real daemon on the socket; shut it down
    // through a direct connection so the test leaves nothing running.
    proxy.shutdown();
    let mut direct = connect_daemon(&socket).expect("auto-started daemon reachable");
    direct
        .send_control(&ControlMessage::request(1, Request::ShutdownDaemon))
        .expect("send shutdown to auto-started daemon");
    let _ = read_one_response(&mut direct, 1);
    wait_for_socket_gone(&socket);
    let _ = std::fs::remove_file(&store);
    let _ = std::fs::remove_dir_all(&managed_root);
}

// ---- proxy subprocess harness ---------------------------------------------

struct ProxyProcess {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl ProxyProcess {
    fn spawn(socket: &Path) -> Self {
        Self::spawn_inner(socket, None)
    }

    fn spawn_with_paths(socket: &Path, store: &Path, managed_root: &Path) -> Self {
        Self::spawn_inner(socket, Some((store, managed_root)))
    }

    fn spawn_inner(socket: &Path, auto_start_paths: Option<(&Path, &Path)>) -> Self {
        let mut command = Command::new(daemon_bin());
        command
            .arg("daemon")
            .arg("proxy")
            // Point the proxy (and any daemon it auto-starts) at the test socket.
            .env("HITCH_SOCKET", socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherit stderr so proxy diagnostics show in test output.
            .stderr(Stdio::inherit());
        // For the auto-start path, steer the daemon the proxy spawns onto isolated
        // store/managed-root paths via the proxy's env passthrough, so the test
        // daemon never races a real daemon's store.
        if let Some((store, managed_root)) = auto_start_paths {
            command
                .env("HITCH_STORE", store)
                .env("HITCH_MANAGED_ROOT", managed_root);
        }
        let mut child = command.spawn().expect("spawn hitch daemon proxy");
        let stdin = child.stdin.take().expect("proxy stdin");
        let stdout = BufReader::new(child.stdout.take().expect("proxy stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send_request(&mut self, id: u64, request: Request) {
        let bytes = encode_control_message(&ControlMessage::request(id, request))
            .expect("encode request");
        self.stdin.write_all(&bytes).expect("write request to proxy stdin");
        self.stdin.flush().expect("flush proxy stdin");
    }

    /// Read newline-delimited control responses from the proxy's stdout until the
    /// response for `id` arrives. Non-matching control frames (events, other
    /// responses) are skipped — proving the proxy forwards them verbatim.
    fn read_response_for(&mut self, id: u64) -> Response {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut line = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for proxy response id {id}"
            );
            line.clear();
            let n = self
                .stdout
                .read_until(b'\n', &mut line)
                .expect("read proxy stdout");
            assert!(n != 0, "proxy stdout closed before response id {id}");
            let trimmed = line.strip_suffix(b"\n").unwrap_or(&line);
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_slice::<ControlMessage>(trimmed) {
                Ok(ControlMessage::Response { id: got, response }) if got == id => return response,
                Ok(_) => {}
                Err(err) => panic!("proxy emitted non-protocol stdout line: {err}: {trimmed:?}"),
            }
        }
    }

    /// Close stdin (EOF to the proxy) and reap the process.
    fn shutdown(&mut self) {
        // Dropping stdin signals EOF; take it so the drop happens now.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// ---- daemon lifecycle (mirrors reattach.rs) -------------------------------

struct DaemonGuard {
    child: Child,
    store: PathBuf,
    managed_root: PathBuf,
}

impl DaemonGuard {
    fn start(socket: &Path) -> Self {
        let store = test_file_path("proxy-daemon-store", "sqlite");
        let managed_root = test_dir_path("proxy-daemon-managed");
        let child = Command::new(daemon_bin())
            .arg("--socket")
            .arg(socket)
            .arg("--store")
            .arg(&store)
            .arg("--managed-root")
            .arg(&managed_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hitch-daemon");
        Self {
            child,
            store,
            managed_root,
        }
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("try_wait daemon") {
                assert!(status.success(), "daemon exited with {status}");
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon did not exit after shutdown");
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.store);
        let _ = std::fs::remove_dir_all(&self.managed_root);
    }
}

fn read_one_response(stream: &mut hitch_proto::transport::DaemonStream, id: u64) -> Response {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "timed out reading direct response id {id}");
        let messages = stream.read_control_messages().expect("read direct response");
        for message in messages {
            if let ControlMessage::Response { id: got, response } = message {
                if got == id {
                    return response;
                }
            }
        }
    }
}

fn wait_for_endpoint(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if connect_daemon(socket).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("daemon endpoint {} never came up", socket.display());
}

fn wait_for_socket_gone(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if connect_daemon(socket).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hitch-daemon"))
}

fn test_socket_path(name: &str) -> PathBuf {
    let extension = if cfg!(windows) { "pipe" } else { "sock" };
    test_path(name, extension)
}

fn test_file_path(name: &str, extension: &str) -> PathBuf {
    test_path(name, extension)
}

fn test_dir_path(name: &str) -> PathBuf {
    let nonce = nonce();
    std::env::temp_dir().join(format!("hitch-{name}-{nonce}"))
}

fn test_path(name: &str, extension: &str) -> PathBuf {
    let nonce = nonce();
    std::env::temp_dir().join(format!("hitch-{name}-{nonce}.{extension}"))
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        ^ (std::process::id() as u128)
}
