//! Regression test for the hook's fire-and-forget delivery against a daemon that
//! services its socket with a *non-blocking polling* accept loop.
//!
//! The real daemon accepts connections with a non-blocking accept that polls on
//! an interval (it must stay responsive to its shutdown flag). On Windows a hook
//! that connected, wrote its report, and disconnected immediately was gone before
//! the next poll, so the daemon never accepted the connection and the report —
//! and every agent-state update — was silently dropped. The fix makes the hook
//! wait for the daemon's reply before closing, holding the socket open across the
//! poll. This test reproduces that exact shape: it runs the real `hitch-hook`
//! binary against a listener that only polls, and asserts the report still lands.
//!
//! On Unix the kernel queues the connection and its bytes for `accept`, so the
//! report survived a fire-and-forget write even before the fix; the test still
//! exercises the binary end-to-end there and must pass on every platform.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hitch_core::{SessionId, SESSION_ID_ENV};
use hitch_proto::transport::{DaemonListener, DaemonStream};
use hitch_proto::{ControlMessage, KnownAgent, Request, Response};

#[test]
fn fire_and_forget_report_reaches_polling_accept_loop() {
    let socket = test_socket_path();
    let listener = DaemonListener::bind(&socket).expect("bind listener");
    // Mirror the daemon: a non-blocking accept loop that only sees a connection
    // when it polls. A hook that closes before the poll is invisible to it.
    listener.set_nonblocking(true).expect("set listener non-blocking");

    let (tx, rx) = mpsc::channel::<ControlMessage>();
    let server = thread::spawn(move || {
        let accept_deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok(stream) => break stream,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= accept_deadline {
                        return; // Never accepted: the hook closed before a poll.
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        };

        // The connection stayed open across the poll, so the report is readable.
        let request = read_first_request(&mut stream);
        // Acknowledge so the hook returns at once instead of waiting on its
        // watchdog — keep the connection open briefly so it can read the reply.
        let _ = stream.send_control(&ControlMessage::response(1, Response::Ack));
        thread::sleep(Duration::from_millis(200));
        if let Some(request) = request {
            let _ = tx.send(request);
        }
    });

    let session_id = SessionId::new();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_hitch-hook"))
        .args([
            "--agent",
            "claude-code",
            "--event",
            "user-prompt-submit",
            "--state",
            "running",
            "--socket",
            socket.to_str().expect("socket path is utf-8"),
        ])
        .env(SESSION_ID_ENV, session_id.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run hitch-hook");
    assert!(status.success(), "hook exited with failure: {status:?}");

    let message = rx
        .recv_timeout(Duration::from_secs(12))
        .expect("daemon never received the agent-state report");
    server.join().expect("server thread panicked");

    let ControlMessage::Request { request, .. } = message else {
        panic!("expected a request, got {message:?}");
    };
    let Request::ReportAgentState {
        agent,
        state,
        session_id: reported_session,
        ..
    } = request
    else {
        panic!("expected report-agent-state, got {request:?}");
    };
    assert_eq!(agent, KnownAgent::ClaudeCode);
    assert_eq!(state, Some(hitch_core::AgentState::Running));
    assert_eq!(reported_session, Some(session_id));

    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket);
}

/// Read control messages until the first request arrives or the hook disconnects.
fn read_first_request(stream: &mut DaemonStream) -> Option<ControlMessage> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match stream.read_control_messages() {
            Ok(messages) => {
                if let Some(message) = messages.into_iter().next() {
                    return Some(message);
                }
            }
            Err(_) => break, // Connection closed or framing fault.
        }
    }
    None
}

fn test_socket_path() -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "hitch-hook-delivery-{}-{now}.sock",
        std::process::id()
    ))
}
