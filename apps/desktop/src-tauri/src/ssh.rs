//! SSH Host connection testing (issue #26, ADR 0014).
//!
//! An **SSH Host** is GUI-local attachment configuration that stores only an
//! OpenSSH target string (`prod`, `user@example.com`, …). Before saving one, the
//! user can run **Test Connection**, which spawns the same non-interactive
//! SSH/proxy/version path the real remote attach (issue #27) will use:
//!
//! ```text
//! ssh -o BatchMode=yes -o ConnectTimeout=10 <target> hitch daemon proxy
//! ```
//!
//! and performs a minimal Hitch protocol Hello/version handshake on the
//! subprocess's stdio. `BatchMode=yes` guarantees OpenSSH never drops an
//! interactive password/passphrase/host-key prompt onto the protocol stream;
//! stderr is captured separately for failure classification. Hitch stores no
//! private keys, passphrases, ports, or usernames outside the target string —
//! OpenSSH config, ssh-agent, hardware keys, ProxyJump, and known_hosts remain
//! the source of truth.
//!
//! ## Seam for issue #27
//!
//! The handshake itself ([`run_test`] → spawn ssh, write Hello, read Hello) and
//! the classifier ([`classify`]) are the client side of the remote attach. Issue
//! #27 reuses both: it spawns the identical `ssh … hitch daemon proxy` command
//! and, on a successful Hello, keeps the connection as the remote Daemon's
//! transport rather than terminating it. The pure [`classify`] function over
//! `(exit code, stderr, handshake outcome)` is unit-tested below.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use hitch_proto::{
    encode_control_message, ControlMessage, Request, Response, PROTOCOL_VERSION,
};
use serde::Serialize;

/// Overall test deadline. A hung connection (no DNS answer, a black-hole route,
/// an ssh that connected but the remote command never produced a Hello) returns
/// a classified network failure rather than blocking the GUI forever. Generous
/// enough to clear the per-connect `ConnectTimeout=10` plus a slow remote proxy
/// start, tight enough that the dialog stays responsive.
const TEST_DEADLINE: Duration = Duration::from_secs(15);

/// Per-connect ssh timeout (`-o ConnectTimeout=10`). Bounds the TCP/auth phase so
/// a black-holed host fails inside the overall deadline with an ssh-sourced
/// reason instead of our generic deadline message.
const SSH_CONNECT_TIMEOUT_SECS: u32 = 10;

/// The exact manual command surfaced in every failure message so the user can
/// reproduce the test in their own terminal. `<target>` is substituted per host.
///
/// This mirrors the client's candidate probe (approach C, ADR 0014 amendment):
/// a Hitch self-install puts the binary at the known location `~/.local/bin/hitch`,
/// while a manual install is expected to put `hitch` on its own login PATH. The
/// advertised command tries the known location first, then bare `hitch`.
pub fn manual_command(target: &str) -> String {
    format!("ssh -o BatchMode=yes {target} '~/.local/bin/hitch daemon proxy || hitch daemon proxy'")
}

/// Actionable failure category for a failed connection test (ADR 0014). Each maps
/// to user-facing copy that tells the user what to fix. Serialized kebab-case to
/// match the frontend's `SshTestCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureCategory {
    /// Auth failed (publickey rejected): check ssh-agent / keys.
    Auth,
    /// Host key not trusted / changed: ssh in manually once to trust it.
    HostKey,
    /// `hitch` is not installed (or not on PATH) on the remote host.
    MissingHitch,
    /// Hello succeeded but the remote protocol version differs: update hitch.
    ProtocolMismatch,
    /// ssh connected and ran the command, but no/garbage Hello came back.
    ProxyStartup,
    /// Could not reach the host (DNS, refused, timed out, no route, VPN down).
    Network,
}

/// Structured result of a connection test, returned to the frontend. `ok` true
/// means the Hello handshake succeeded at the matching protocol version; every
/// other case carries a `category` and human `message` (which embeds the manual
/// command), with an optional `detail` (a stderr tail or version numbers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshTestResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<FailureCategory>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SshTestResult {
    fn success() -> Self {
        Self {
            ok: true,
            category: None,
            message: "Connected — remote hitch daemon proxy answered at a compatible protocol version.".into(),
            detail: None,
        }
    }

    fn failure(category: FailureCategory, message: String, detail: Option<String>) -> Self {
        Self {
            ok: false,
            category: Some(category),
            message,
            detail,
        }
    }
}

/// Outcome of the stdio Hello handshake, independent of the ssh process result.
/// `classify` combines this with the exit code + stderr to pick a category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeOutcome {
    /// A `Response::Hello` was read; carries the remote protocol version.
    Hello { protocol_version: u16 },
    /// stdout closed / EOF before any Hello line arrived.
    NoResponse,
    /// A line was read but it was not a well-formed Hello response.
    Malformed,
    /// The handshake never ran (ssh failed to spawn, or we hit the deadline
    /// before reading anything).
    NotAttempted,
}

/// Validate + normalize a user-entered OpenSSH target. Returns the trimmed target
/// on success, or an error string explaining why it is rejected. Pure so the
/// frontend's mirror validation and these rules can be checked against each other.
///
/// Rules (kept deliberately strict — the target becomes an ssh argv arg):
/// - trim surrounding whitespace; reject empty,
/// - reject embedded whitespace (a target is a single host/alias token; a space
///   would smuggle extra ssh arguments),
/// - reject a leading `-` (ssh would parse it as an option, not a target —
///   option injection).
pub fn normalize_target(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter an SSH target (e.g. user@example.com or a host alias).".into());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("An SSH target cannot contain spaces.".into());
    }
    if trimmed.starts_with('-') {
        return Err("An SSH target cannot start with '-'.".into());
    }
    Ok(trimmed.to_string())
}

/// Classify a connection-test outcome into an actionable result. Pure over
/// `(exit_code, stderr, handshake)` so it is unit-testable without spawning ssh.
/// `deadline_hit` true means our overall timer fired before the test completed.
pub fn classify(
    target: &str,
    exit_code: Option<i32>,
    stderr: &str,
    handshake: HandshakeOutcome,
    deadline_hit: bool,
) -> SshTestResult {
    let manual = manual_command(target);
    let lower = stderr.to_ascii_lowercase();
    let tail = stderr_tail(stderr);

    // A completed Hello is authoritative regardless of how ssh exited afterwards
    // (we kill the proxy once we have the Hello, so a non-zero exit is expected).
    if let HandshakeOutcome::Hello { protocol_version } = handshake {
        if protocol_version == PROTOCOL_VERSION {
            return SshTestResult::success();
        }
        return SshTestResult::failure(
            FailureCategory::ProtocolMismatch,
            format!(
                "Protocol mismatch: this Hitch speaks v{PROTOCOL_VERSION}, the remote hitch speaks v{protocol_version}. \
                 Update hitch on the host so the versions match. Manual test: {manual}"
            ),
            Some(format!(
                "local protocol v{PROTOCOL_VERSION}, remote protocol v{protocol_version}"
            )),
        );
    }

    // Host-key trust failures: ssh refused to proceed because the host identity is
    // unknown or changed. Checked before the generic auth/network buckets because
    // its stderr also mentions "verification". Actionable: ssh in manually once.
    if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
        || lower.contains("no matching host key")
    {
        return SshTestResult::failure(
            FailureCategory::HostKey,
            format!(
                "Host key not trusted. Run `ssh {target}` once in a terminal to review and accept \
                 the host key (Hitch never accepts host keys for you), then test again. Manual test: {manual}"
            ),
            tail,
        );
    }

    // Auth failures: publickey rejected. BatchMode=yes means no password prompt,
    // so this is a key/agent problem. Actionable: check ssh-agent / your keys.
    if lower.contains("permission denied")
        || lower.contains("too many authentication failures")
        || lower.contains("no more authentication methods")
    {
        return SshTestResult::failure(
            FailureCategory::Auth,
            format!(
                "Authentication failed. Make sure your key is loaded in ssh-agent and that {target} \
                 accepts it (Hitch uses non-interactive SSH, so passwords/passphrases are never prompted). \
                 Manual test: {manual}"
            ),
            tail,
        );
    }

    // Missing remote `hitch`: ssh connected and ran, but the command was not found.
    // Exit 127 is the conventional POSIX shell "command not found" code.
    //
    // Windows shells phrase a missing program/path differently (and don't use exit
    // 127), so match their wording too:
    //   cmd.exe:     'X' is not recognized as an internal or external command
    //                The system cannot find the path specified.
    //   PowerShell:  The term 'X' is not recognized as the name of a cmdlet …
    // This matters for the real attach's candidate probe (`ssh_pool::connect_once`):
    // it tries `~/.local/bin/hitch` first, and ONLY a MissingHitch classification
    // falls through to the bare-`hitch` candidate. On a Windows remote (`hitch.exe`
    // on the registry PATH, never at `~/.local/bin`), the known-location candidate
    // must classify as MissingHitch or the attach never tries bare `hitch` and the
    // host stays Unreachable even though Test Connection (bare `hitch`) succeeds.
    if exit_code == Some(127)
        || lower.contains("command not found")
        || lower.contains("hitch: not found")
        || lower.contains("no such file or directory")
        || (lower.contains("hitch") && lower.contains("not found"))
        || lower.contains("is not recognized")
        || lower.contains("the system cannot find")
    {
        return SshTestResult::failure(
            FailureCategory::MissingHitch,
            format!(
                "Connected, but `hitch` was not found on {target}. Install Hitch on the host (a self-install \
                 symlinks `hitch`/`hitch-hook` into `~/.local/bin`), or for a manual install put `hitch` on the \
                 login PATH (or symlink it into `~/.local/bin`). Manual test: {manual}"
            ),
            tail,
        );
    }

    // Network / VPN: name resolution, refused, timed out, no route. ssh's own exit
    // 255 with a network-ish reason lands here, as does our overall deadline.
    if lower.contains("could not resolve hostname")
        || lower.contains("name or service not known")
        || lower.contains("connection refused")
        || lower.contains("connection timed out")
        || lower.contains("operation timed out")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("connection closed")
        || deadline_hit
    {
        let reason = if deadline_hit {
            format!(
                "Timed out reaching {target}. Check the network/VPN and that the host is up. Manual test: {manual}"
            )
        } else {
            format!(
                "Could not reach {target}. Check the network/VPN, the hostname, and that the host is up. \
                 Manual test: {manual}"
            )
        };
        return SshTestResult::failure(FailureCategory::Network, reason, tail);
    }

    // ssh connected and the command ran, but the Hello never arrived (or was
    // garbage): a proxy-startup problem on the remote. Surface the stderr tail.
    match handshake {
        HandshakeOutcome::NoResponse | HandshakeOutcome::Malformed | HandshakeOutcome::NotAttempted => {
            SshTestResult::failure(
                FailureCategory::ProxyStartup,
                format!(
                    "Connected, but `hitch daemon proxy` did not return a Hitch protocol stream on {target}. \
                     No Hitch daemon is running there (the proxy does not start one) — launch Hitch or \
                     `hitch daemon` on the remote — or the remote hitch is too old. Manual test: {manual}"
                ),
                tail,
            )
        }
        // Unreachable: the Hello arm returned above.
        HandshakeOutcome::Hello { .. } => unreachable!("hello handled above"),
    }
}

/// The last few non-empty lines of stderr, for the result `detail`. `None` when
/// stderr is empty. Bounded so a chatty remote can't blow up the IPC payload.
fn stderr_tail(stderr: &str) -> Option<String> {
    let trimmed = stderr.trim_end_matches('\n').trim();
    if trimmed.is_empty() {
        return None;
    }
    let lines: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(6);
    Some(lines[start..].join("\n"))
}

/// Spawn `ssh … hitch daemon proxy`, attempt the Hello handshake on its stdio,
/// terminate the subprocess, and classify the outcome. Blocking — callers run it
/// off the UI thread (the Tauri command uses `spawn_blocking`).
pub fn run_test(target: &str) -> SshTestResult {
    let target = match normalize_target(target) {
        Ok(t) => t,
        Err(message) => {
            return SshTestResult::failure(FailureCategory::Network, message, None);
        }
    };

    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"))
        // A leading `--` ends option processing so a target that somehow reached
        // here can never be parsed as an ssh option (defense in depth on top of
        // `normalize_target`'s leading-dash rejection).
        .arg("--")
        .arg(&target)
        .arg("hitch")
        .arg("daemon")
        .arg("proxy")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            // ssh itself is missing or unspawnable — a local setup problem.
            return SshTestResult::failure(
                FailureCategory::Network,
                format!(
                    "Could not run ssh: {err}. Is OpenSSH installed and on PATH? Manual test: {}",
                    manual_command(&target)
                ),
                None,
            );
        }
    };

    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    // Drain stderr on a worker thread so a chatty ssh can't deadlock us by
    // filling its stderr pipe while we block reading stdout.
    let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
    let stderr_handle = stderr_pipe.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            let _ = stderr_tx.send(buf);
        })
    });

    // Send the Hello request, then read until a Hello response (or EOF/garbage),
    // on a worker thread so the overall deadline can abandon a hung remote.
    let (hs_tx, hs_rx) = mpsc::channel::<HandshakeOutcome>();
    let handshake_handle = match stdout {
        Some(stdout) => {
            let request_id = 1u64;
            let hello = encode_control_message(&ControlMessage::request(
                request_id,
                Request::Hello {
                    client_name: "hitch-desktop".into(),
                    protocol_version: PROTOCOL_VERSION,
                },
            ));
            // Write the Hello before moving stdin/stdout into the worker. A write
            // failure (ssh died immediately) leaves stdin best-effort; the read
            // worker still reports NoResponse and the classifier reads stderr.
            if let (Ok(bytes), Some(stdin)) = (hello, stdin.as_mut()) {
                let _ = stdin.write_all(&bytes);
                let _ = stdin.flush();
            }
            Some(thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let outcome = read_hello(&mut reader, request_id);
                let _ = hs_tx.send(outcome);
            }))
        }
        None => None,
    };

    // Wait for the handshake worker up to the overall deadline.
    let (handshake, deadline_hit) = match handshake_handle {
        Some(handle) => match hs_rx.recv_timeout(TEST_DEADLINE) {
            Ok(outcome) => {
                let _ = handle.join();
                (outcome, false)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (HandshakeOutcome::NotAttempted, true),
            Err(mpsc::RecvTimeoutError::Disconnected) => (HandshakeOutcome::NoResponse, false),
        },
        None => (HandshakeOutcome::NotAttempted, false),
    };

    // Terminate the subprocess cleanly: drop stdin (EOF to the remote command),
    // then kill + reap so no ssh lingers after the test.
    drop(stdin);
    let _ = child.kill();
    let exit_status = child.wait().ok();
    let exit_code = exit_status.and_then(|s| s.code());

    // Collect stderr (best-effort within a short grace; the pipe usually closes
    // as soon as the killed child exits).
    let stderr = if let Some(handle) = stderr_handle {
        let collected = stderr_rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
        let _ = handle.join();
        collected
    } else {
        String::new()
    };

    classify(&target, exit_code, &stderr, handshake, deadline_hit)
}

/// Read newline-delimited control JSON until a `Response::Hello` for `request_id`
/// arrives (returning its version), EOF (`NoResponse`), or a non-Hello/garbled
/// line (`Malformed`). Skips any non-matching control frames the remote sends
/// ahead of the Hello (e.g. an event), matching the desktop's own hello reader.
fn read_hello<R: BufRead>(reader: &mut R, request_id: u64) -> HandshakeOutcome {
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return HandshakeOutcome::NoResponse,
            Ok(_) => {}
            Err(_) => return HandshakeOutcome::NoResponse,
        }
        let trimmed = line.strip_suffix(b"\n").unwrap_or(&line);
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_slice::<ControlMessage>(trimmed) {
            Ok(ControlMessage::Response {
                id,
                response: Response::Hello { protocol_version, .. },
            }) if id == request_id => {
                return HandshakeOutcome::Hello { protocol_version };
            }
            // A well-formed but non-Hello frame (an event, or a different
            // response): keep reading for the Hello.
            Ok(_) => continue,
            // A line that is not control JSON at all — the remote wrote noise to
            // stdout instead of the protocol stream.
            Err(_) => return HandshakeOutcome::Malformed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_accepts_plain_targets() {
        assert_eq!(normalize_target("  prod ").unwrap(), "prod");
        assert_eq!(normalize_target("user@example.com").unwrap(), "user@example.com");
    }

    #[test]
    fn normalize_rejects_empty_whitespace_and_leading_dash() {
        assert!(normalize_target("   ").is_err());
        assert!(normalize_target("").is_err());
        assert!(normalize_target("host with space").is_err());
        assert!(normalize_target("-oProxyCommand=evil").is_err());
    }

    #[test]
    fn classify_hello_matching_version_is_ok() {
        let result = classify(
            "prod",
            Some(0),
            "",
            HandshakeOutcome::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            false,
        );
        assert!(result.ok);
        assert!(result.category.is_none());
    }

    #[test]
    fn classify_hello_mismatched_version_is_protocol_mismatch() {
        let remote = PROTOCOL_VERSION.wrapping_sub(1);
        let result = classify(
            "prod",
            None,
            "",
            HandshakeOutcome::Hello {
                protocol_version: remote,
            },
            false,
        );
        assert!(!result.ok);
        assert_eq!(result.category, Some(FailureCategory::ProtocolMismatch));
        // Both versions are surfaced for the user.
        assert!(result.message.contains(&PROTOCOL_VERSION.to_string()));
        assert!(result.message.contains(&remote.to_string()));
        // Every failure embeds the manual command.
        assert!(result.message.contains(&manual_command("prod")));
    }

    #[test]
    fn classify_publickey_denied_is_auth() {
        let result = classify(
            "user@host",
            Some(255),
            "user@host: Permission denied (publickey).",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::Auth));
        assert!(result.detail.is_some());
    }

    #[test]
    fn classify_host_key_changed_is_host_key() {
        let result = classify(
            "host",
            Some(255),
            "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\nHost key verification failed.",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::HostKey));
    }

    #[test]
    fn classify_command_not_found_is_missing_hitch() {
        let result = classify(
            "prod",
            Some(127),
            "bash: hitch: command not found",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::MissingHitch));
        assert!(result.message.contains("Install"));
    }

    #[test]
    fn classify_windows_cmd_not_recognized_is_missing_hitch() {
        // The known-location candidate `~/.local/bin/hitch` run via cmd.exe on a
        // Windows remote: cmd reports "is not recognized as an internal or external
        // command". Must classify as MissingHitch so the attach falls through to the
        // bare-`hitch` (registry-PATH) candidate instead of giving up Unreachable.
        let result = classify(
            "pc@192.168.0.9",
            Some(1),
            "'~/.local/bin/hitch' is not recognized as an internal or external command,\r\noperable program or batch file.",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::MissingHitch));
    }

    #[test]
    fn classify_windows_powershell_not_recognized_is_missing_hitch() {
        let result = classify(
            "pc@192.168.0.9",
            Some(1),
            "The term 'C:\\Users\\pc\\.local\\bin\\hitch' is not recognized as the name of a cmdlet, function, script file, or operable program.",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::MissingHitch));
    }

    #[test]
    fn classify_windows_cannot_find_path_is_missing_hitch() {
        let result = classify(
            "pc@192.168.0.9",
            Some(1),
            "The system cannot find the path specified.",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::MissingHitch));
    }

    #[test]
    fn classify_unresolvable_host_is_network() {
        let result = classify(
            "nope.invalid",
            Some(255),
            "ssh: Could not resolve hostname nope.invalid: Name or service not known",
            HandshakeOutcome::NotAttempted,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::Network));
    }

    #[test]
    fn classify_deadline_is_network() {
        let result = classify("prod", None, "", HandshakeOutcome::NotAttempted, true);
        assert_eq!(result.category, Some(FailureCategory::Network));
        assert!(result.message.contains("Timed out"));
    }

    #[test]
    fn classify_connected_but_no_hello_is_proxy_startup() {
        let result = classify(
            "prod",
            Some(0),
            "",
            HandshakeOutcome::NoResponse,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::ProxyStartup));
    }

    #[test]
    fn classify_garbage_stdout_is_proxy_startup() {
        let result = classify(
            "prod",
            Some(0),
            "",
            HandshakeOutcome::Malformed,
            false,
        );
        assert_eq!(result.category, Some(FailureCategory::ProxyStartup));
    }

    #[test]
    fn read_hello_parses_matching_response() {
        let msg = encode_control_message(&ControlMessage::response(
            7,
            Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                daemon_pid: 1234,
                os_family: hitch_proto::OsFamily::Unix,
                exe_path: None,
            },
        ))
        .unwrap();
        let mut reader = std::io::Cursor::new(msg);
        assert_eq!(
            read_hello(&mut reader, 7),
            HandshakeOutcome::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
    }

    #[test]
    fn read_hello_skips_leading_events_then_reads_hello() {
        let mut buf = encode_control_message(&ControlMessage::response(
            1,
            Response::Ack,
        ))
        .unwrap();
        buf.extend(
            encode_control_message(&ControlMessage::response(
                5,
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 9,
                    os_family: hitch_proto::OsFamily::Unix,
                    exe_path: None,
                },
            ))
            .unwrap(),
        );
        let mut reader = std::io::Cursor::new(buf);
        assert_eq!(
            read_hello(&mut reader, 5),
            HandshakeOutcome::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
    }

    #[test]
    fn read_hello_reports_eof_as_no_response() {
        let mut reader = std::io::Cursor::new(Vec::new());
        assert_eq!(read_hello(&mut reader, 1), HandshakeOutcome::NoResponse);
    }

    #[test]
    fn read_hello_reports_garbage_as_malformed() {
        let mut reader = std::io::Cursor::new(b"not json at all\n".to_vec());
        assert_eq!(read_hello(&mut reader, 1), HandshakeOutcome::Malformed);
    }
}
