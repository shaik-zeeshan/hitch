use std::io;
#[cfg(any(unix, windows))]
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use hitch_core::SESSION_ID_ENV;
#[cfg(any(unix, windows))]
use hitch_core::{Project, Session, SessionId, SessionParent, Worktree};
use hitch_proto::transport::{connect_daemon, DaemonStream};
#[cfg(unix)]
use hitch_proto::ErrorCode;
#[cfg(any(unix, windows))]
use hitch_proto::{
    encode_control_message, encode_pty_frame, CommitDraft, Event, GitStatus, JobRequest,
    PullRequestDraft,
};
use hitch_proto::{ControlMessage, Request, Response, PROTOCOL_VERSION};

#[test]
fn daemon_transport_answers_hello_ping_and_shutdown() {
    let socket = test_socket_path("transport-basic");
    let mut daemon = DaemonGuard::start(&socket);

    let mut stream = connect_test_daemon(&socket);
    send_transport_request(
        &mut stream,
        1,
        Request::Hello {
            client_name: "reattach-test".into(),
            protocol_version: PROTOCOL_VERSION,
        },
    );
    expect_transport_response(&mut stream, 1, |response| {
        matches!(response, Response::Hello { .. })
    });
    send_transport_request(&mut stream, 2, Request::Ping);
    expect_transport_response(&mut stream, 2, |response| {
        matches!(response, Response::Pong)
    });
    send_transport_request(&mut stream, 3, Request::ShutdownDaemon);
    expect_transport_response(&mut stream, 3, |response| matches!(response, Response::Ack));
    daemon.wait_for_exit();
}

#[cfg(windows)]
#[test]
fn windows_default_shell_session_accepts_input_resize_and_kills_descendants() {
    let socket = test_socket_path("windows-session");
    let project_root = test_dir_path("windows-session-project");
    let orphan_marker = project_root.join("hitch-orphan-marker.txt");
    let _ = std::fs::remove_file(&orphan_marker);
    std::fs::create_dir_all(&project_root).unwrap();
    let _daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &project_root);
    let session = client.open_default_session(3, SessionParent::Project(project.id));

    let input = format!(
        "@echo off\r\n\
         echo {env}=%{env}%\r\n\
         start \"\" /b cmd.exe /d /q /c \"echo HITCH_CHILD_STARTED & ping -n 5 127.0.0.1 >nul & echo HITCH_ORPHANED>hitch-orphan-marker.txt\"\r\n",
        env = SESSION_ID_ENV,
    );
    client.send_session_input(4, session.id, input.as_bytes());

    let output =
        client.read_output_until(session.id, "HITCH_CHILD_STARTED", Duration::from_secs(5));
    assert!(
        output.contains(&format!("{SESSION_ID_ENV}={}", session.id)),
        "default shell did not inherit {SESSION_ID_ENV}; saw {output:?}"
    );
    assert!(
        output.contains("HITCH_CHILD_STARTED"),
        "descendant process did not start; saw {output:?}"
    );

    client.resize_session(5, session.id, 100, 30);
    client.close_session(6, session.id, true);
    client.read_session_closed(session.id, Duration::from_secs(5));

    thread::sleep(Duration::from_secs(5));
    assert!(
        !orphan_marker.exists(),
        "closing a Windows PTY session left a shell descendant running"
    );
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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
    assert_eq!(status.additions, 1);
    assert_eq!(status.deletions, 1);
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

#[cfg(unix)]
#[test]
fn draft_generation_round_trips_over_socket() {
    let socket = test_socket_path("drafts");
    let repo = test_dir_path("drafts-repo");
    init_git_repo(&repo);
    run_git(&repo, ["checkout", "-b", "feature/draft-text"]);
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    std::fs::write(repo.join("tracked.txt"), "staged\n").unwrap();
    std::fs::write(repo.join("unstaged.txt"), "unstaged\n").unwrap();
    client.ack(
        4,
        Request::StageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );
    let commit = client.generate_commit_draft(5, worktree.id);
    assert_eq!(commit.subject, "chore: update tracked.txt");
    assert!(commit.body.contains("tracked.txt"));
    assert!(!commit.body.contains("unstaged.txt"));

    client.ack(
        6,
        Request::Commit {
            worktree_id: worktree.id,
            subject: commit.subject,
            body: Some(commit.body),
        },
    );
    let pr = client.generate_pr_draft(7, worktree.id, Some("main".into()));
    assert!(pr.title.contains("update") || pr.title.contains("tracked"));
    assert!(pr.body.contains("## Summary"));
    assert!(pr.body.contains("tracked.txt"));
    assert!(pr.body.contains("## Testing"));
    assert!(pr.body.contains("- [ ] Not run"));

    client.shutdown(8);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
}

#[cfg(unix)]
#[test]
fn slow_draft_generation_does_not_block_follow_up_requests() {
    let socket = test_socket_path("async-drafts");
    let repo = test_dir_path("async-drafts-repo");
    let codex = write_slow_codex_stub();
    init_git_repo(&repo);
    let mut daemon = DaemonGuard::start_with_codex(&socket, &codex);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    std::fs::write(repo.join("tracked.txt"), "staged async draft\n").unwrap();
    client.ack(
        4,
        Request::StageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );

    // Start a slow draft as a Job; the JobStarted reply returns immediately so
    // the request loop is free for follow-up requests.
    client.send_request(
        5,
        Request::GenerateCommitDraft {
            worktree_id: worktree.id,
            settings: None,
        },
    );
    let job_id = client.read_job_started(5);

    // A git status issued while the slow draft Job is still running must not wait
    // behind it — proof the Job runs off the request loop.
    let started = Instant::now();
    let status = client.git_status(6, worktree.id);
    assert!(status.dirty);
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "git status waited behind slow draft generation for {:?}",
        started.elapsed()
    );

    // The draft eventually completes via its JobCompleted event.
    let draft = match client.read_job_completed(job_id) {
        Response::CommitDraft { draft } => draft,
        other => panic!("unexpected slow-draft job response: {other:?}"),
    };
    assert_eq!(draft.subject, "test: async draft");

    client.shutdown(7);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
    let _ = std::fs::remove_file(codex);
}

#[cfg(unix)]
#[test]
fn cancel_job_kills_running_draft_and_completes_promptly() {
    // A draft Job spawns a slow provider child in its own process group.
    // `CancelJob` must SIGKILL that tree so the Job completes (as cancelled) far
    // sooner than the provider's own 1s sleep (ADR 0008).
    let socket = test_socket_path("cancel-draft");
    let repo = test_dir_path("cancel-draft-repo");
    let codex = write_slow_codex_stub();
    init_git_repo(&repo);
    let mut daemon = DaemonGuard::start_with_codex(&socket, &codex);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    std::fs::write(repo.join("tracked.txt"), "staged cancel\n").unwrap();
    client.ack(
        4,
        Request::StageFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into()],
        },
    );

    client.send_request(
        5,
        Request::GenerateCommitDraft {
            worktree_id: worktree.id,
            settings: None,
        },
    );
    let job_id = client.read_job_started(5);

    // Give the worker a moment to spawn the provider child, then cancel it.
    std::thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    client.ack(6, Request::CancelJob { job_id });

    match client.read_job_completed(job_id) {
        Response::Error { error } => assert!(error.retryable),
        other => panic!("cancelled draft job should complete with an error: {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "cancel did not kill the provider promptly: took {:?}",
        started.elapsed()
    );

    client.shutdown(7);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
    let _ = std::fs::remove_file(codex);
}

#[cfg(unix)]
#[test]
fn discard_files_round_trip_over_socket_keeps_connection_open() {
    let socket = test_socket_path("git-discard");
    let repo = test_dir_path("git-discard-repo");
    init_git_repo(&repo);
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    std::fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
    std::fs::write(repo.join("new.txt"), "new\n").unwrap();
    assert!(client.git_status(4, worktree.id).dirty);

    client.ack(
        5,
        Request::DiscardFiles {
            worktree_id: worktree.id,
            paths: vec!["tracked.txt".into(), "new.txt".into()],
        },
    );

    assert!(!client.git_status(6, worktree.id).dirty);
    // A second request after discard proves the daemon did not close the client socket.
    let sessions = client.list_sessions(7, None);
    assert!(sessions.is_empty());

    client.shutdown(8);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
}

#[cfg(unix)]
#[test]
fn invalid_request_returns_error_without_closing_connection() {
    let socket = test_socket_path("invalid-request");
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.hello(1);
    client.send_raw_control(
        br#"{"kind":"request","id":2,"request":{"type":"future-request"}}
"#,
    );

    loop {
        match client.read_packet() {
            Packet::Control(ControlMessage::Response {
                id: 2,
                response: Response::Error { error },
            }) => {
                assert_eq!(error.code, ErrorCode::InvalidRequest);
                break;
            }
            Packet::Control(_) | Packet::Output { .. } => continue,
        }
    }

    // A follow-up request proves malformed/unknown requests do not drop the client.
    let sessions = client.list_sessions(3, None);
    assert!(sessions.is_empty());

    client.shutdown(4);
    daemon.wait_for_exit();
}

#[cfg(unix)]
#[test]
fn prior_protocol_is_rejected_at_hello() {
    let socket = test_socket_path("proto");
    let mut daemon = DaemonGuard::start(&socket);

    let mut client = TestClient::connect(&socket);
    client.send_request(
        1,
        Request::Hello {
            client_name: "old-client".into(),
            protocol_version: PROTOCOL_VERSION - 1,
        },
    );

    loop {
        match client.read_packet() {
            Packet::Control(ControlMessage::Response {
                id: 1,
                response: Response::Error { error },
            }) => {
                assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
                break;
            }
            Packet::Control(_) | Packet::Output { .. } => continue,
        }
    }

    drop(client);
    let mut compatible = TestClient::connect(&socket);
    compatible.hello(2);
    compatible.shutdown(3);
    daemon.wait_for_exit();
}

#[cfg(unix)]
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
            subject: "feat: change tracked".into(),
            body: None,
        },
    );
    // Push now runs as a Job (ADR 0008): JobStarted then JobCompleted{Ack}.
    match client.run_job(
        7,
        JobRequest::Push {
            worktree_id: worktree.id,
        },
    ) {
        Response::Ack => {}
        other => panic!("push job did not ack: {other:?}"),
    }

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

#[cfg(unix)]
#[test]
fn fetch_job_updates_behind_count_from_remote() {
    let socket = test_socket_path("git-fetch");
    let repo = test_dir_path("git-fetch-repo");
    let remote = test_dir_path("git-fetch-remote");
    let peer = test_dir_path("git-fetch-peer");
    init_git_repo(&repo);
    init_bare_remote(&remote);
    let remote_str = remote.to_str().unwrap();
    run_git(&repo, ["remote", "add", "origin", remote_str]);
    run_git(&repo, ["push", "-u", "origin", "main"]);

    let peer_str = peer.to_str().unwrap();
    run_git(
        std::env::temp_dir().as_path(),
        ["clone", remote_str, peer_str],
    );
    run_git(&peer, ["config", "user.name", "Hitch Test"]);
    run_git(&peer, ["config", "user.email", "hitch@example.test"]);
    std::fs::write(peer.join("tracked.txt"), "remote change\n").unwrap();
    run_git(&peer, ["add", "tracked.txt"]);
    run_git(&peer, ["commit", "-m", "remote change"]);
    run_git(&peer, ["push", "origin", "main"]);

    let mut daemon = DaemonGuard::start(&socket);
    let mut client = TestClient::connect(&socket);
    client.hello(1);
    let project = client.add_project(2, &repo);
    let worktree = client.list_worktrees(3, project.id).remove(0);

    let before = client.git_status(4, worktree.id);
    assert_eq!(before.behind, 0);
    match client.run_job(
        5,
        JobRequest::Fetch {
            worktree_id: worktree.id,
        },
    ) {
        Response::Ack => {}
        other => panic!("fetch job did not ack: {other:?}"),
    }
    let after = client.git_status(6, worktree.id);
    assert_eq!(after.behind, 1);

    client.shutdown(7);
    daemon.wait_for_exit();
    let _ = std::fs::remove_dir_all(repo);
    let _ = std::fs::remove_dir_all(remote);
    let _ = std::fs::remove_dir_all(peer);
}

#[cfg(unix)]
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
fn connect_test_daemon(socket: &Path) -> DaemonStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect_daemon(socket) {
            Ok(stream) => return stream,
            Err(err) if Instant::now() < deadline => {
                if err.kind() != io::ErrorKind::NotFound
                    && err.kind() != io::ErrorKind::ConnectionRefused
                {
                    // The daemon may have bound the endpoint but not yet entered accept.
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(err) => panic!("connect {}: {err}", socket.display()),
        }
    }
}

fn send_transport_request(stream: &mut DaemonStream, id: u64, request: Request) {
    stream
        .send_control(&ControlMessage::request(id, request))
        .expect("send request");
}

fn expect_transport_response(
    stream: &mut DaemonStream,
    expected_id: u64,
    expected: impl Fn(&Response) -> bool,
) {
    loop {
        let messages = stream.read_control_messages().expect("read response");
        for message in messages {
            match message {
                ControlMessage::Response { id, response }
                    if id == expected_id && expected(&response) =>
                {
                    return;
                }
                ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                } => panic!("request failed: {error:?}"),
                ControlMessage::Response { id, response } if id == expected_id => {
                    panic!("unexpected response: {response:?}");
                }
                ControlMessage::Event { .. }
                | ControlMessage::Request { .. }
                | ControlMessage::Response { .. } => {}
            }
        }
    }
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

    #[cfg(unix)]
    fn start_with_gh(socket: &Path, gh: &Path) -> Self {
        Self::start_inner(socket, Some(gh))
    }

    #[cfg(unix)]
    fn start_with_codex(socket: &Path, codex: &Path) -> Self {
        let store = test_file_path("daemon-store", "sqlite");
        let managed_root = test_dir_path("daemon-managed");
        let child = spawn_daemon_with_codex(socket, &store, &managed_root, codex);
        Self {
            child,
            store,
            managed_root,
        }
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

#[cfg(any(unix, windows))]
struct TestClient {
    stream: BufReader<DaemonStream>,
}

#[cfg(any(unix, windows))]
#[allow(dead_code)]
impl TestClient {
    fn connect(socket: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match connect_daemon(socket) {
                Ok(stream) => {
                    stream.set_nonblocking(true).expect("set nonblocking");
                    return Self {
                        stream: BufReader::new(stream),
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

    fn ping(&mut self, id: u64) {
        self.send_request(id, Request::Ping);
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::Pong,
                }) if response_id == id => return,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("ping failed: {error:?}"),
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

    fn generate_commit_draft(
        &mut self,
        id: u64,
        worktree_id: hitch_core::WorktreeId,
    ) -> CommitDraft {
        self.send_request(
            id,
            Request::GenerateCommitDraft {
                worktree_id,
                settings: None,
            },
        );
        self.generate_commit_draft_response(id)
    }

    fn generate_commit_draft_response(&mut self, id: u64) -> CommitDraft {
        let job_id = self.read_job_started(id);
        match self.read_job_completed(job_id) {
            Response::CommitDraft { draft } => draft,
            Response::Error { error } => panic!("generate commit draft failed: {error:?}"),
            other => panic!("unexpected commit draft job response: {other:?}"),
        }
    }

    fn generate_pr_draft(
        &mut self,
        id: u64,
        worktree_id: hitch_core::WorktreeId,
        base: Option<String>,
    ) -> PullRequestDraft {
        match self.run_job(
            id,
            JobRequest::GeneratePullRequestDraft {
                worktree_id,
                base,
                settings: None,
            },
        ) {
            Response::PullRequestDraft { draft } => draft,
            Response::Error { error } => panic!("generate PR draft failed: {error:?}"),
            other => panic!("unexpected PR draft job response: {other:?}"),
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
        match self.run_job(
            id,
            JobRequest::CreatePullRequest {
                worktree_id,
                title: title.into(),
                body: None,
                base,
                draft,
            },
        ) {
            Response::PullRequestCreated { url } => url,
            Response::Error { error } => panic!("create pr failed: {error:?}"),
            other => panic!("unexpected create-pr job response: {other:?}"),
        }
    }

    fn ack(&mut self, id: u64, request: Request) {
        self.send_request(id, request);
        self.read_ack(id);
    }

    fn read_ack(&mut self, id: u64) {
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

    // ---- Job helpers (ADR 0008) ------------------------------------------
    //
    // Long-running ops (push/pull/PR/drafts) now reply `JobStarted { job_id }`
    // synchronously and deliver their real `Response` later inside a
    // `JobCompleted` event. These mirror the desktop client's StartJob ->
    // JobStarted -> JobCompleted handling.

    /// Read the synchronous `JobStarted` reply for `request_id`, returning its id.
    fn read_job_started(&mut self, request_id: u64) -> hitch_core::JobId {
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id,
                    response: Response::JobStarted { job_id },
                }) if id == request_id => return job_id,
                Packet::Control(ControlMessage::Response {
                    id,
                    response: Response::Error { error },
                }) if id == request_id => panic!("job rejected: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    /// Read the `JobCompleted` event for `job_id`, returning the wrapped response.
    fn read_job_completed(&mut self, job_id: hitch_core::JobId) -> Response {
        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Event {
                    event:
                        Event::JobCompleted {
                            job_id: completed,
                            response,
                        },
                }) if completed == job_id => return *response,
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    /// Send a `StartJob` wrapper and block until its Job completes, returning
    /// the wrapped response.
    fn run_job(&mut self, id: u64, request: JobRequest) -> Response {
        self.send_request(id, Request::StartJob { request });
        let job_id = self.read_job_started(id);
        self.read_job_completed(job_id)
    }

    fn open_session(&mut self, id: u64, parent: SessionParent, command: Vec<String>) -> Session {
        self.open_session_with_command(id, parent, Some(command))
    }

    fn open_default_session(&mut self, id: u64, parent: SessionParent) -> Session {
        self.open_session_with_command(id, parent, None)
    }

    fn open_session_with_command(
        &mut self,
        id: u64,
        parent: SessionParent,
        command: Option<Vec<String>>,
    ) -> Session {
        self.send_request(
            id,
            Request::OpenSession {
                parent,
                name: "test-shell".into(),
                command,
                cols: 80,
                rows: 24,
            },
        );

        loop {
            match self.read_packet() {
                Packet::Control(ControlMessage::Response {
                    id: response_id,
                    response: Response::SessionOpened { session, .. },
                }) if response_id == id => return session,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("open session failed: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
    }

    fn send_session_input(&mut self, id: u64, session_id: SessionId, payload: &[u8]) {
        self.send_request_with_pty_frame(
            id,
            Request::SendSessionInput {
                session_id,
                byte_count: payload.len() as u32,
            },
            payload,
        );
        self.read_ack(id);
    }

    fn resize_session(&mut self, id: u64, session_id: SessionId, cols: u16, rows: u16) {
        self.ack(
            id,
            Request::ResizeSession {
                session_id,
                cols,
                rows,
            },
        );
    }

    fn close_session(&mut self, id: u64, session_id: SessionId, kill_process: bool) {
        self.ack(
            id,
            Request::CloseSession {
                session_id,
                kill_process,
            },
        );
    }

    fn read_session_closed(&mut self, session_id: SessionId, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.read_packet_until(deadline) {
                Packet::Control(ControlMessage::Event {
                    event:
                        Event::SessionClosed {
                            session_id: closed,
                            exit_code,
                        },
                }) if closed == session_id => return exit_code,
                Packet::Control(ControlMessage::Response {
                    response: Response::Error { error },
                    ..
                }) => panic!("daemon error while waiting for session close: {error:?}"),
                Packet::Control(_) | Packet::Output { .. } => continue,
            }
        }
        panic!("timed out waiting for session {session_id} to close");
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
            match self.read_packet_until(deadline) {
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
            match self.read_packet_until(deadline) {
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
        self.stream.get_mut().write_all(&bytes).unwrap();
        self.stream.get_mut().flush().unwrap();
    }

    fn send_raw_control(&mut self, bytes: &[u8]) {
        self.stream.get_mut().write_all(bytes).unwrap();
        self.stream.get_mut().flush().unwrap();
    }

    #[allow(dead_code)]
    fn send_request_with_pty_frame(&mut self, id: u64, request: Request, payload: &[u8]) {
        let control = encode_control_message(&ControlMessage::request(id, request)).unwrap();
        let frame = encode_pty_frame(payload).unwrap();
        self.stream.get_mut().write_all(&control).unwrap();
        self.stream.get_mut().write_all(&frame).unwrap();
        self.stream.get_mut().flush().unwrap();
    }

    fn read_packet(&mut self) -> Packet {
        self.read_packet_until(Instant::now() + Duration::from_secs(5))
    }

    fn read_packet_until(&mut self, deadline: Instant) -> Packet {
        let mut line = Vec::new();
        loop {
            match self.stream.read_until(b'\n', &mut line) {
                Ok(0) if cfg!(windows) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(0) => panic!("daemon closed connection"),
                Ok(_) => break,
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("read control line: {err}"),
            }
        }
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
            self.read_exact_until(&mut prefix, deadline);
            let len = u32::from_be_bytes(prefix);
            assert_eq!(
                len, byte_count,
                "event byte_count and frame length should match"
            );
            let mut bytes = vec![0_u8; len as usize];
            self.read_exact_until(&mut bytes, deadline);
            Packet::Output { session_id, bytes }
        } else {
            Packet::Control(message)
        }
    }

    fn read_exact_until(&mut self, buf: &mut [u8], deadline: Instant) {
        let mut read = 0;
        while read < buf.len() {
            match self.stream.read(&mut buf[read..]) {
                Ok(0) if cfg!(windows) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(0) => panic!("daemon closed connection"),
                Ok(len) => read += len,
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("read pty frame: {err}"),
            }
        }
    }
}

#[cfg(any(unix, windows))]
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

#[cfg(unix)]
fn spawn_daemon(socket: &Path, store: &Path, managed_root: &Path) -> Child {
    spawn_daemon_full(socket, store, managed_root, None)
}

fn spawn_daemon_full(socket: &Path, store: &Path, managed_root: &Path, gh: Option<&Path>) -> Child {
    let mut command = daemon_command(socket, store, managed_root);
    if let Some(gh) = gh {
        command.arg("--gh").arg(gh);
    }
    spawn_daemon_command(command)
}

#[cfg(unix)]
fn spawn_daemon_with_codex(
    socket: &Path,
    store: &Path,
    managed_root: &Path,
    codex: &Path,
) -> Child {
    let mut command = daemon_command(socket, store, managed_root);
    command
        .arg("--draft-provider")
        .arg("codex")
        .arg("--codex")
        .arg(codex)
        .arg("--draft-timeout-secs")
        .arg("5");
    spawn_daemon_command(command)
}

fn daemon_command(socket: &Path, store: &Path, managed_root: &Path) -> Command {
    let mut command = Command::new(daemon_bin());
    command
        .env_remove("HITCH_DRAFT_PROVIDER")
        .env_remove("HITCH_DRAFT_TIMEOUT_SECS")
        .env_remove("HITCH_DRAFT_MODEL")
        .env_remove("HITCH_CLAUDE_PATH")
        .env_remove("HITCH_CODEX_PATH")
        .arg("--socket")
        .arg(socket)
        .arg("--store")
        .arg(store)
        .arg("--managed-root")
        .arg(managed_root);
    command
}

fn spawn_daemon_command(mut command: Command) -> Child {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hitch-daemon")
}

#[cfg(unix)]
fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init", "--initial-branch=main"]);
    run_git(path, ["config", "user.name", "Hitch Test"]);
    run_git(path, ["config", "user.email", "hitch@example.test"]);
    std::fs::write(path.join("tracked.txt"), "initial\n").unwrap();
    run_git(path, ["add", "tracked.txt"]);
    run_git(path, ["commit", "-m", "initial"]);
}

#[cfg(unix)]
fn init_bare_remote(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init", "--bare", "--initial-branch=main"]);
}

/// Write an executable shell script that impersonates `gh`, echoing a fixed PR
/// URL so create-PR can be exercised over the socket without hitting GitHub.
#[cfg(unix)]
fn write_gh_stub() -> PathBuf {
    let path = test_file_path("gh-stub", "sh");
    write_executable_script(
        &path,
        "#!/bin/sh\necho \"https://github.com/example/hitch/pull/1\"\n",
    );
    path
}

#[cfg(unix)]
fn write_slow_codex_stub() -> PathBuf {
    let path = test_file_path("codex-stub", "sh");
    write_executable_script(
        &path,
        "#!/bin/sh\n[ \"$1\" = \"exec\" ] || exit 12\nsleep 1\nprintf '%s\n' '{\"subject\":\"test: async draft\",\"body\":\"Generated slowly\"}'\n",
    );
    path
}

#[cfg(unix)]
fn write_executable_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
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
