//! Windows de-risk spike for the ssh-agent relay (plan slice 7).
//!
//! Question it answers: **does Win32-OpenSSH's `ssh.exe`/`ssh-add.exe` honor
//! `SSH_AUTH_SOCK` when it points at a named pipe `\\.\pipe\...`?** If yes, the
//! daemon can host its per-connection ssh-agent socket as a named pipe (mirroring
//! `DaemonListener::bind`) and inject it uniformly as `SSH_AUTH_SOCK` on both
//! platforms — slice 7 "green" path. If no, the Windows injection must fall back
//! to `-o IdentityAgent=\\.\pipe\hitch-<hash>` via `GIT_SSH_COMMAND` — slice 7
//! "red" path.
//!
//! What it does: binds an owner-only named pipe (the exact namespace +
//! `GenericNamespaced` + SDDL `D:P(A;;GA;;;OW)` that `crates/hitch-proto`'s
//! transport uses), sets `SSH_AUTH_SOCK` to its `\\.\pipe\…` address, runs
//! `%SystemRoot%\System32\OpenSSH\ssh-add.exe -l` with that env, and acts as a
//! minimal fake agent: it answers `SSH2_AGENTC_REQUEST_IDENTITIES` (11) with
//! `SSH2_AGENT_IDENTITIES_ANSWER` (12) carrying zero keys. If `ssh-add` connects
//! and sends the request, Win32-OpenSSH honored the pipe → GREEN.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // Mirror crates/hitch-proto/src/transport.rs's interprocess imports exactly:
    // `prelude::*` brings the `to_ns_name` / accept / stream traits into scope, and
    // `ListenerOptionsExt` provides the `.security_descriptor(...)` builder method.
    use interprocess::{
        local_socket::{prelude::*, GenericNamespaced, ListenerOptions},
        os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
    };
    use std::io::{Read, Write};
    use std::process::{Command, ExitCode};

    let pid = std::process::id();
    let logical = format!("hitch-spike-{pid}");
    let pipe_address = format!(r"\\.\pipe\{logical}");

    // Owner-only DACL, identical to the daemon pipe (ADR 0012). ssh-add runs as
    // the same user, so this both restricts the pipe AND proves the SDDL doesn't
    // block a same-user client.
    let sddl = widestring::U16CString::from_str("D:P(A;;GA;;;OW)").expect("static SDDL is valid");
    let security_descriptor =
        SecurityDescriptor::deserialize(&sddl).expect("deserialize owner-only SDDL");

    let name = logical
        .clone()
        .to_ns_name::<GenericNamespaced>()
        .expect("namespaced pipe name");
    let listener = ListenerOptions::new()
        .name(name)
        .security_descriptor(security_descriptor)
        .create_sync()
        .expect("bind named pipe");

    println!("spike: bound {pipe_address}");
    println!("spike: setting SSH_AUTH_SOCK={pipe_address} and running System32 ssh-add -l …");

    // Run the real Win32-OpenSSH ssh-add against our pipe, in a child so this
    // thread can serve the agent request.
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let ssh_add = format!(r"{system_root}\System32\OpenSSH\ssh-add.exe");
    let mut child = Command::new(&ssh_add)
        .arg("-l")
        .env("SSH_AUTH_SOCK", &pipe_address)
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {ssh_add}: {err}"));

    // Serve exactly one connection: the proof that ssh-add honored the pipe.
    match listener.accept() {
        Ok(mut conn) => {
            let mut len = [0u8; 4];
            if conn.read_exact(&mut len).is_ok() {
                let n = u32::from_be_bytes(len) as usize;
                let mut payload = vec![0u8; n];
                let _ = conn.read_exact(&mut payload);
                let kind = payload.first().copied().unwrap_or(0);
                // SSH2_AGENTC_REQUEST_IDENTITIES = 11.
                println!("spike: GREEN — ssh-add connected over the named pipe (first message type = {kind}).");
                // Reply SSH2_AGENT_IDENTITIES_ANSWER(12) with 0 keys: [12, u32=0].
                let answer = [12u8, 0, 0, 0, 0];
                let _ = conn.write_all(&(answer.len() as u32).to_be_bytes());
                let _ = conn.write_all(&answer);
                let _ = conn.flush();
            } else {
                println!("spike: ssh-add connected but sent no framed request.");
            }
            let _ = child.wait();
            println!("spike RESULT: GREEN — uniform SSH_AUTH_SOCK works on Windows; enable the named-pipe daemon agent socket (slice 7 green path).");
            ExitCode::SUCCESS
        }
        Err(err) => {
            let _ = child.wait();
            eprintln!("spike: accept failed: {err}");
            eprintln!("spike RESULT: RED (or inconclusive) — ssh-add did not connect to the pipe. Use the GIT_SSH_COMMAND `-o IdentityAgent=\\\\.\\pipe\\hitch-<hash>` fallback (slice 7 red path).");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "win-ssh-agent-pipe-spike is Windows-only. Build and run it on the Windows host:\n  \
         cargo run --manifest-path spikes/win-ssh-agent-pipe/Cargo.toml"
    );
}
