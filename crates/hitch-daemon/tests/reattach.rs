use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hitch_core::{Project, Session, SessionId, SessionParent, Worktree};
use hitch_proto::{
    encode_control_message, encode_pty_frame, ControlMessage, Event, GitStatus, Request, Response,
    PROTOCOL_VERSION,
};

#[test]
fn reconnect_replays_scrollback_and_receives_live_output() {
    let socket = test_socket_path("reattach");
    let project_root = test_dir_path("reattach-project");
    std::fs::create_dir_all(&project_root).unwrap();
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &project_root);

    let session = client.open_session(
        3,
        SessionParent::Project(project.id),
        vec![
            "/bin/sh".into(),
            "-lc".into(),
            "for i in 1 2 3 4 5 6; do echo hitch-tick-$i; sleep 0.20; done; sleep 2".into(),
        ],
    );

    let first_output = client.read_output_until(session.id, "hitch-tick-1", Duration::from_secs(3));
    assert!(first_output.contains("hitch-tick-1"));
    drop(client);

    // The child should keep running while no client is connected.
    thread::sleep(Duration::from_millis(550));

    let mut reattached = TestClient::connect(&socket);
    reattached.hello(4);
    let replay_and_live =
        reattached.read_output_until(session.id, "hitch-tick-5", Duration::from_secs(4));

    assert!(
        replay_and_live.contains("hitch-tick-1"),
        "reattach did not replay prior scrollback: {replay_and_live:?}"
    );
    assert!(
        replay_and_live.contains("hitch-tick-5"),
        "reattach did not continue receiving output from the still-running child: {replay_and_live:?}"
    );

    reattached.shutdown(99);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(project_root);
}

#[test]
fn simulated_reboot_restores_persisted_session_layout_as_fresh_session() {
    let socket = test_socket_path("restore-one");
    let store = test_file_path("restore-store", "sqlite");
    let managed_root = test_dir_path("restore-managed");
    let project_root = test_dir_path("restore-project");
    std::fs::create_dir_all(&project_root).unwrap();

    let mut first = spawn_daemon(&socket, &store, &managed_root);
    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &project_root);
    let opened = client.open_session(
        3,
        SessionParent::Project(project.id),
        vec!["/bin/sh".into(), "-lc".into(), "sleep 30".into()],
    );
    drop(client);
    let _ = first.kill();
    let _ = first.wait();
    let _ = std::fs::remove_file(&socket);

    let mut second = spawn_daemon(&socket, &store, &managed_root);
    let mut reconnected = TestClient::connect(&socket);
    reconnected.hello(4);
    let sessions = reconnected.list_sessions(5, None);
    assert!(
        sessions.iter().any(|session| session.id == opened.id),
        "restored sessions were {sessions:?}"
    );
    reconnected.shutdown(6);
    wait_for_socket_gone(&socket, Duration::from_secs(3));
    let _ = second.wait();

    let _ = std::fs::remove_file(store);
    let _ = std::fs::remove_dir_all(managed_root);
    let _ = std::fs::remove_dir_all(project_root);
}

#[test]
fn graceful_quit_preserves_layout_for_next_launch() {
    // A menu-bar "Quit Hitch" sends ShutdownDaemon, which kills live PTYs. Those
    // kills must not erase the persisted layout: the next launch should restore
    // the same sessions as fresh terminals (ADR 0003).
    let socket = test_socket_path("graceful-quit");
    let store = test_file_path("graceful-quit-store", "sqlite");
    let managed_root = test_dir_path("graceful-quit-managed");
    let project_root = test_dir_path("graceful-quit-project");
    std::fs::create_dir_all(&project_root).unwrap();

    let mut first = spawn_daemon(&socket, &store, &managed_root);
    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &project_root);
    let opened = client.open_session(
        3,
        SessionParent::Project(project.id),
        vec!["/bin/sh".into(), "-lc".into(), "sleep 30".into()],
    );
    client.shutdown(4);
    wait_for_socket_gone(&socket, Duration::from_secs(3));
    let _ = first.wait();

    let mut second = spawn_daemon(&socket, &store, &managed_root);
    let mut reconnected = TestClient::connect(&socket);
    reconnected.hello(5);
    let sessions = reconnected.list_sessions(6, None);
    assert!(
        sessions.iter().any(|session| session.id == opened.id),
        "graceful quit dropped the layout; restored sessions were {sessions:?}"
    );
    reconnected.shutdown(7);
    wait_for_socket_gone(&socket, Duration::from_secs(3));
    let _ = second.wait();

    let _ = std::fs::remove_file(store);
    let _ = std::fs::remove_dir_all(managed_root);
    let _ = std::fs::remove_dir_all(project_root);
}

#[test]
fn worktree_branch_tracks_unborn_head_renames() {
    let socket = test_socket_path("unborn-branch");
    let repo = test_dir_path("unborn-branch-repo");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, ["init", "--initial-branch=placeholder"]);
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);
    assert_eq!(worktree.branch, "placeholder");

    run_git(&repo, ["symbolic-ref", "HEAD", "refs/heads/main"]);
    let status = client.git_status(4, worktree.id);
    assert_eq!(status.branch, "main");
    let refreshed = client.list_worktrees(5, project.id).remove(0);
    assert_eq!(refreshed.branch, "main");

    client.shutdown(6);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn git_status_stage_and_unstage_round_trip_over_socket() {
    let socket = test_socket_path("git-flow");
    let repo = test_dir_path("git-repo");
    init_git_repo(&repo);
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    std::fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
    let status = client.git_status(4, worktree.id);
    assert!(status.dirty);
    assert!(status.files.iter().any(|file| !file.staged));

    client.ack(
        5,
        Request::StageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );
    let status = client.git_status(6, worktree.id);
    assert!(status.files.iter().any(|file| file.staged));

    client.ack(
        7,
        Request::UnstageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );
    let status = client.git_status(8, worktree.id);
    assert!(status.files.iter().any(|file| !file.staged));

    client.shutdown(9);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn stage_commit_push_and_create_pr_round_trip_over_socket() {
    let socket = test_socket_path("git-commit");
    let repo = test_dir_path("git-commit-repo");
    let remote = test_dir_path("git-commit-remote");
    let gh_stub = write_gh_stub();
    init_git_repo(&repo);
    init_bare_remote(&remote);
    run_git(&repo, ["remote", "add", "origin", remote.to_str().unwrap()]);
    let mut daemon = DaemonGuard::start_with_gh(&socket, &gh_stub);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    // Stage → commit → push the dirty file all the way to the bare remote.
    std::fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
    assert!(client.git_status(4, worktree.id).dirty);
    client.ack(
        5,
        Request::StageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );
    client.ack(
        6,
        Request::Commit {
            worktree_id: worktree.id,
            message: "feat: change tracked".into(),
        },
    );
    client.ack(
        7,
        Request::Push {
            worktree_id: worktree.id,
        },
    );

    // The committed worktree is clean again, and the bare remote has the commit.
    assert!(!client.git_status(8, worktree.id).dirty);

    // Create a PR through the stubbed `gh` (already pushed, so no extra push).
    let url = client.create_pr(9, worktree.id, "Change tracked", Some("main".into()), false);
    assert_eq!(url, "https://github.com/example/hitch/pull/1");

    client.shutdown(10);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
    let _ = std::fs::remove_dir_all(remote);
    let _ = std::fs::remove_file(gh_stub);
}

#[test]
fn detach_mode_survives_spawning_process_exit() {
    let socket = test_socket_path("detach");
    let store = test_file_path("detach-store", "sqlite");
    let managed_root = test_dir_path("detach-managed");
    let output = Command::new(daemon_bin())
        .arg("--socket")
        .arg(&socket)
        .arg("--store")
        .arg(&store)
        .arg("--managed-root")
        .arg(&managed_root)
        .arg("--detach")
        .output()
        .expect("spawn detach harness");
    assert!(
        output.status.success(),
        "detach failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    client.shutdown(2);
    wait_for_socket_gone(&socket, Duration::from_secs(3));
    let _ = std::fs::remove_file(store);
    let _ = std::fs::remove_dir_all(managed_root);
}

struct DaemonGuard {
    child: Child,
    store: PathBuf,
    managed_root: PathBuf,
}

impl DaemonGuard {
    fn start(socket: &Path) -> Self {
        Self::start_inner(socket, None)
    }

    fn start_with_gh(socket: &Path, gh: &Path) -> Self {
        Self::start_inner(socket, Some(gh))
    }

    fn start_inner(socket: &Path, gh: Option<&Path>) -> Self {
        let store = test_file_path("daemon-store", "sqlite");
        let managed_root = test_dir_path("daemon-managed");
        let child = spawn_daemon_full(socket, &store, &managed_root, gh);
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

struct TestClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl TestClient {
    fn connect(socket: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match UnixStream::connect(socket) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("set timeout");
                    let reader_stream = stream.try_clone().expect("clone stream");
                    return Self {
                        writer: stream,
                        reader: BufReader::new(reader_stream),
                    };
                }
                Err(err) if Instant::now() < deadline => {
                    if err.kind() != io::ErrorKind::NotFound
                        && err.kind() != io::ErrorKind::ConnectionRefused
                    {
                        // The daemon may have bound the path but not yet entered accept.
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!("connect {}: {err}", socket.display()),
            }
        }
    }

    fn hello(&mut self, id: u64) {
        self.send_request(
            id,
            Request::Hello {
                client_name: "reattach-test".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        );
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Hello { .. },
                }) if response_id == id => return,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("hello failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn add_project(&mut self, id: u64, root: &Path) -> Project {
        self.send_request(id, Request::AddProject { root: root.into() });
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Projects { projects },
                }) if response_id == id => return projects.into_iter().next().unwrap(),
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("add project failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn list_worktrees(&mut self, id: u64, project_id: hitch_core::ProjectId) -> Vec<Worktree> {
        self.send_request(id, Request::ListWorktrees { project_id });
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Worktrees { worktrees },
                }) if response_id == id => return worktrees,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("list worktrees failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn list_sessions(&mut self, id: u64, parent: Option<SessionParent>) -> Vec<Session> {
        self.send_request(id, Request::ListSessions { parent });
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Sessions { sessions },
                }) if response_id == id => return sessions,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("list sessions failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn git_status(&mut self, id: u64, worktree_id: hitch_core::WorktreeId) -> GitStatus {
        self.send_request(id, Request::GitStatus { worktree_id });
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::GitStatus { status },
                }) if response_id == id => return status,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("git status failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn create_pr(
        &mut self,
        id: u64,
        worktree_id: hitch_core::WorktreeId,
        title: &str,
        base: Option<String>,
        draft: bool,
    ) -> String {
        self.send_request(
            id,
            Request::CreatePullRequest {
                worktree_id,
                title: title.into(),
                body: None,
                base,
                draft,
            },
        );
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::PullRequestCreated { url },
                }) if response_id == id => return url,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("create pr failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn ack(&mut self, id: u64, request: Request) {
        self.send_request(id, request);
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Ack,
                }) if response_id == id => return,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("request failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn open_session(&mut self, id: u64, parent: SessionParent, command: Vec<String>) -> Session {
        self.send_request(
            id,
            Request::OpenSession {
                parent,
                name: "test-shell".into(),
                command: Some(command),
            },
        );

        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::SessionOpened { session },
                }) if response_id == id => return session,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("open session failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn read_output_until(
        &mut self,
        session_id: SessionId,
        needle: &str,
        timeout: Duration,
    ) -> String {
        let deadline = Instant::now() + timeout;
        let mut bytes = Vec::new();
        while Instant::now() < deadline {
            match self.read_packet() {
                Packet::Output {
                    session_id: packet_session_id,
                    bytes: packet_bytes,
                } if packet_session_id == session_id => {
                    bytes.extend(packet_bytes);
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    if text.contains(needle) {
                        return text;
                    }
                }
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("daemon error while waiting for output: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
        panic!(
            "timed out waiting for {needle:?}; saw {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    fn shutdown(&mut self, id: u64) {
        self.send_request(id, Request::ShutdownDaemon);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Ack,
                }) if response_id == id => return,
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
        panic!("timed out waiting for shutdown ack");
    }

    fn send_request(&mut self, id: u64, request: Request) {
        let bytes = encode_control_message(&ControlMessage::request(id, request)).unwrap();
        self.writer.write_all(&bytes).unwrap();
        self.writer.flush().unwrap();
    }

    #[allow(dead_code)]
    fn send_request_with_pty_frame(&mut self, id: u64, request: Request, payload: &[u8]) {
        let control = encode_control_message(&ControlMessage::request(id, request)).unwrap();
        let frame = encode_pty_frame(payload).unwrap();
        self.writer.write_all(&control).unwrap();
        self.writer.write_all(&frame).unwrap();
        self.writer.flush().unwrap();
    }

    fn read_packet(&mut self) -> Packet {
        let mut line = Vec::new();
        let len = self.reader.read_until(b'\n', &mut line).unwrap();
        assert!(len > 0, "daemon closed connection");
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let message: ControlMessage = serde_json::from_slice(&line).unwrap();

        if let ControlMessage::Event {
            event:
                Event::SessionOutput {
                    session_id,
                    byte_count,
                },
        } = message
        {
            let mut prefix = [0_u8; 4];
            self.reader.read_exact(&mut prefix).unwrap();
            let len = u32::from_be_bytes(prefix);
            assert_eq!(
                len, byte_count,
                "event byte_count and frame length should match"
            );
            let mut bytes = vec![0_u8; len as usize];
            self.reader.read_exact(&mut bytes).unwrap();
            Packet::Output { session_id, bytes }
        } else {
            Packet::Control(message)
        }
    }
}

enum Packet {
    Control(ControlMessage),
    Output {
        session_id: SessionId,
        bytes: Vec<u8>,
    },
}

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hitch-daemon"))
}

fn spawn_daemon(socket: &Path, store: &Path, managed_root: &Path) -> Child {
    spawn_daemon_full(socket, store, managed_root, None)
}

fn spawn_daemon_full(socket: &Path, store: &Path, managed_root: &Path, gh: Option<&Path>) -> Child {
    let mut command = Command::new(daemon_bin());
    command
        .arg("--socket")
        .arg(socket)
        .arg("--store")
        .arg(store)
        .arg("--managed-root")
        .arg(managed_root);
    if let Some(gh) = gh {
        command.arg("--gh").arg(gh);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hitch-daemon")
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init", "--initial-branch=main"]);
    run_git(path, ["config", "user.name", "Hitch Test"]);
    run_git(path, ["config", "user.email", "hitch@example.test"]);
    std::fs::write(path.join("tracked.txt"), "initial\n").unwrap();
    run_git(path, ["add", "tracked.txt"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn init_bare_remote(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init", "--bare", "--initial-branch=main"]);
}

/// Write an executable shell script that impersonates `gh`, echoing a fixed PR
/// URL so create-PR can be exercised over the socket without hitting GitHub.
fn write_gh_stub() -> PathBuf {
    let path = test_file_path("gh-stub", "sh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho \"https://github.com/example/hitch/pull/1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn test_socket_path(name: &str) -> PathBuf {
    test_file_path(name, "sock")
}

fn test_file_path(name: &str, extension: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("hitch-daemon-{name}-{nonce}.{extension}"))
}

fn test_dir_path(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("hitch-daemon-{name}-{nonce}"))
}

fn wait_for_socket_gone(socket: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !socket.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket still exists after shutdown: {}", socket.display());
}
