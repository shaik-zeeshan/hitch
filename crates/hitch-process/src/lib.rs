//! `hitch-process` — kill-the-whole-tree process primitives (ADR 0012).
//!
//! A *leaf* crate (ADR 0005): no in-workspace dependencies, so feature crates
//! (`hitch-pty`, `hitch-git`) and the daemon can all share one implementation
//! of "terminate this child *and every descendant*" without depending on each
//! other. Unix uses process groups; Windows uses Job Objects.

#[cfg(windows)]
pub use job_object::JobHandle;
pub use pipe_reader::{DrainOutcome, PipeReader};
pub use process_tree::ProcessTree;
pub use registration::ProcessTreeRegistration;
pub use windowless::configure_windowless;

/// RAII guard that registers a spawned [`ProcessTree`] with an external
/// canceller and disarms it exactly once — either explicitly via
/// [`ProcessTreeRegistration::disarm`] or on drop.
///
/// Both the daemon's draft provider runner and `hitch-git::run_command`
/// orchestrate a cancellable child through a shared `ProcessTree`: they hand the
/// tree to a cancel registry on spawn so a concurrent cancel can call
/// `tree.terminate()`, then must clear that registration *before* they drain the
/// child's pipe readers.
///
/// ## The recycled-pgid race this guards against
///
/// Once the child (the process-group leader on Unix) is reaped, the registered
/// tree's `terminate()` is no longer safe to call from a concurrent cancel: on
/// Unix it is `kill(-pgid)`, and the leader's pgid can be recycled to an
/// unrelated group as soon as the group empties. Disarming the registration
/// before joining the readers — which is where the thread may park while the
/// child exits — closes the window where a late cancel could signal a recycled
/// process group. A cancel-before-exit path that already terminated the tree
/// while the group was alive loses no intended kill by disarming afterward.
///
/// The guard owns no `ProcessTree`; it only invokes a caller-supplied disarm
/// closure (typically `set_process_tree(None)` on a cancel handle). Keeping it
/// closure-generic lets `hitch-process` stay a leaf crate (ADR 0005) while both
/// `hitch-git` and the daemon share one disarm-before-drain discipline.
mod registration {
    /// See [`crate::ProcessTreeRegistration`].
    pub struct ProcessTreeRegistration<F: FnMut()> {
        disarm: F,
        armed: bool,
    }

    impl<F: FnMut()> ProcessTreeRegistration<F> {
        /// Build an armed registration. `arm` runs immediately (register the
        /// tree); `disarm` runs once later, either via [`Self::disarm`] or on
        /// drop (clear the registration).
        pub fn new(mut arm: impl FnMut(), disarm: F) -> Self {
            arm();
            Self {
                disarm,
                armed: true,
            }
        }

        /// Disarm now (clear the registration) rather than waiting for drop.
        /// Idempotent: a subsequent [`Self::disarm`] or drop does nothing.
        pub fn disarm(&mut self) {
            if self.armed {
                self.armed = false;
                (self.disarm)();
            }
        }
    }

    impl<F: FnMut()> Drop for ProcessTreeRegistration<F> {
        fn drop(&mut self) {
            self.disarm();
        }
    }
}

/// Shared Win32 Job Object plumbing used by both Windows process-tree wrappers.
///
/// Hitch has two distinct kill-the-whole-tree wrappers on Windows — `hitch-pty`'s
/// PTY-session job object and the cancellable-command [`ProcessTree`] — that
/// differ only in *how* they get a process handle into the job (the PTY assigns
/// portable-pty's already-running ConPTY child; ProcessTree spawns
/// `CREATE_SUSPENDED`, assigns, then resumes). The job-object lifecycle itself —
/// create an unnamed job, set `KILL_ON_JOB_CLOSE`, assign a process, terminate,
/// and close the handle on drop — is identical, so it lives here once rather than
/// being duplicated in each wrapper (ADR 0005).
#[cfg(windows)]
mod job_object {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Owns an unnamed Win32 Job Object handle configured with
    /// `KILL_ON_JOB_CLOSE`, closing it exactly once on drop.
    #[derive(Debug)]
    pub struct JobHandle(HANDLE);

    // SAFETY: a job-object HANDLE is a kernel handle with no thread affinity; the
    // wrapper only ever passes it to thread-safe Win32 calls and closes it once.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        /// Create a fresh unnamed job object that terminates all assigned
        /// processes when its last handle closes (`KILL_ON_JOB_CLOSE`).
        pub fn create_kill_on_close() -> io::Result<Self> {
            // SAFETY: null security attributes and name request a new unnamed job
            // object. On failure Windows returns a null handle, converted below.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            // SAFETY: `job.0` is a live job handle owned by `job`, `limits` points
            // to a properly initialized JOBOBJECT_EXTENDED_LIMIT_INFORMATION, and
            // the byte count matches that structure. `job` closes the handle on
            // early return via its Drop impl.
            let configured = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(job)
        }

        /// Assign a process (by raw handle) to this job. Returns whether the
        /// assignment succeeded; callers decide whether a failure is fatal.
        ///
        /// The handle is taken as a raw `*mut c_void` (the `std`/`portable-pty`
        /// raw-handle representation) so callers don't need `windows-sys`.
        ///
        /// # Safety
        /// `process` must be a valid, open process handle.
        pub unsafe fn assign_process(&self, process: *mut core::ffi::c_void) -> bool {
            AssignProcessToJobObject(self.0, process as HANDLE) != 0
        }

        /// Terminate every process currently assigned to this job.
        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: `self.0` is a live job handle for the lifetime of `self`.
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is owned by this wrapper and closed exactly once here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Platform process-tree handle for long-running cancellable child processes.
///
/// This is the process-tree wrapper ADR 0012 calls for: the daemon's
/// cancellable Jobs (ADR 0008 — commit/PR draft providers) and `hitch-git`'s
/// cancellable git commands spawn a plain [`std::process::Command`] inside one of
/// these so a timeout or `CancelJob` reaches the whole tree, not just the tracked
/// leaf. It is distinct from `hitch-pty`'s PTY-session job object, which wraps a
/// `portable-pty` Session child rather than a `std::process::Child`.
///
/// Unix uses a fresh process group and kills that group. Windows uses a Job
/// Object configured with `KILL_ON_JOB_CLOSE`, held alive by cloned handles.
/// Other platforms retain the child pid only so callers can still use direct
/// child termination while keeping this API portable.
mod process_tree {
    use std::process::{Child, Command};

    #[derive(Clone, Debug)]
    pub struct ProcessTree {
        inner: imp::ProcessTree,
    }

    impl ProcessTree {
        pub fn spawn(command: &mut Command) -> std::io::Result<(Child, Self)> {
            imp::ProcessTree::spawn(command).map(|(child, inner)| (child, Self { inner }))
        }

        pub fn terminate(&self) -> std::io::Result<()> {
            self.inner.terminate()
        }

        /// Terminate descendants once the tree's *leader* has already been reaped.
        ///
        /// [`Self::terminate`] is only safe while the leader is alive: on Unix it
        /// is `kill(-pgid)`, and once the leader (the process-group leader) is
        /// reaped and the group empties, that pgid can be recycled to an unrelated
        /// group — signaling it would hit the wrong process group (the
        /// recycled-pgid race [`ProcessTreeRegistration`] guards against). This
        /// variant is the post-reap-safe form: on Unix it does nothing rather than
        /// signal a possibly-recycled pgid; on Windows the Job Object is referenced
        /// by an owned handle, so `TerminateJobObject` still reaches whatever
        /// descendants remain assigned without any pid/group reuse hazard.
        ///
        /// Use this only after the leader is known reaped (e.g. `try_wait`/`wait`
        /// returned its status); use [`Self::terminate`] while it is still alive.
        pub fn terminate_after_leader_reaped(&self) -> std::io::Result<()> {
            self.inner.terminate_after_leader_reaped()
        }
    }

    #[cfg(unix)]
    mod imp {
        use std::process::{Child, Command};

        use std::os::unix::process::CommandExt;

        #[derive(Clone, Debug)]
        pub(super) struct ProcessTree {
            pgid: i32,
        }

        impl ProcessTree {
            pub(super) fn spawn(command: &mut Command) -> std::io::Result<(Child, Self)> {
                command.process_group(0);
                let child = command.spawn()?;
                let pgid = child.id() as i32;
                Ok((child, Self { pgid }))
            }

            pub(super) fn terminate(&self) -> std::io::Result<()> {
                let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
                if result == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }

            pub(super) fn terminate_after_leader_reaped(&self) -> std::io::Result<()> {
                // The leader is reaped, so its pgid can be recycled to an unrelated
                // group; `kill(-pgid)` would risk signaling that group. A
                // descendant that left the group is unreachable by a group kill
                // anyway, and one that stayed shared the now-empty leader's fate.
                // Skip the group signal rather than chance a recycled pgid.
                Ok(())
            }
        }
    }

    #[cfg(windows)]
    mod imp {
        use std::io;
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        use std::process::{Child, Command};
        use std::sync::Arc;

        use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, HANDLE, NTSTATUS};
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        use crate::job_object::JobHandle;

        #[link(name = "ntdll")]
        extern "system" {
            fn NtResumeProcess(process_handle: HANDLE) -> NTSTATUS;
        }

        #[derive(Clone, Debug)]
        pub(super) struct ProcessTree {
            // `None` when the child could not be assigned to a Job Object (e.g. a
            // parent job that forbids nesting / breakaway, or pre-Windows-8 where
            // a process already in a job cannot join another). The spawn still
            // succeeds in that case; cancellation degrades to direct child kill,
            // which every caller already pairs with `terminate()`.
            job: Option<Arc<JobHandle>>,
        }

        impl ProcessTree {
            pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
                let job = JobHandle::create_kill_on_close()?;
                // CREATE_NO_WINDOW: every ProcessTree child is a non-interactive
                // console process with piped/null stdio (git, draft providers).
                // Without it, a child spawned from a console-less parent (the
                // hidden-console daemon, or a GUI process) would materialize a
                // visible console window.
                command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
                let mut child = command.spawn()?;
                let process = child.as_raw_handle();
                // On Windows 8+ a process that inherited a parent job can still be
                // nested into this fresh job, so assignment normally succeeds. It
                // can legitimately fail when the parent job forbids nesting (e.g.
                // it was created with `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`) or on
                // pre-Win8 systems. Treat that as a soft failure: drop the job and
                // keep the child rather than killing every spawn under such a
                // parent. The job only existed to reach *descendants* on kill;
                // without it `terminate` falls back to the caller's direct child
                // kill, so the process is still cancellable.
                //
                // SAFETY: `process` is the freshly-spawned (suspended) child's
                // handle, valid until we kill/wait it below.
                let job = if unsafe { job.assign_process(process) } {
                    Some(Arc::new(job))
                } else {
                    None
                };
                if let Err(error) = resume_process(process as HANDLE) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                Ok((child, Self { job }))
            }

            pub(super) fn terminate(&self) -> io::Result<()> {
                // No job means assignment failed at spawn time; the caller pairs
                // `terminate` with a direct child kill, so report success and let
                // that path reap the process.
                let Some(job) = self.job.as_ref() else {
                    return Ok(());
                };
                job.terminate()
            }

            pub(super) fn terminate_after_leader_reaped(&self) -> io::Result<()> {
                // The Job Object is held alive by an owned handle, so terminating
                // it after the leader is reaped reaches any still-assigned
                // descendant with no pid/handle reuse hazard — unlike Unix's pgid
                // signal. Identical to `terminate` here.
                self.terminate()
            }
        }

        fn resume_process(process: HANDLE) -> io::Result<()> {
            let status = unsafe { NtResumeProcess(process) };
            if status >= 0 {
                Ok(())
            } else {
                let error = unsafe { RtlNtStatusToDosError(status) };
                Err(io::Error::from_raw_os_error(error as i32))
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    mod imp {
        use std::process::{Child, Command};

        #[derive(Clone, Debug)]
        pub(super) struct ProcessTree {
            #[allow(dead_code)]
            pid: u32,
        }

        impl ProcessTree {
            pub(super) fn spawn(command: &mut Command) -> std::io::Result<(Child, Self)> {
                let child = command.spawn()?;
                let pid = child.id();
                Ok((child, Self { pid }))
            }

            pub(super) fn terminate(&self) -> std::io::Result<()> {
                Ok(())
            }

            pub(super) fn terminate_after_leader_reaped(&self) -> std::io::Result<()> {
                Ok(())
            }
        }
    }
}

/// Windowless console-child spawn flag, shared so the magic number lives in one
/// place.
///
/// On Windows every console process Hitch spawns (git, gh, draft providers, the
/// daemon detach shim) is non-interactive with redirected/null stdio. Without
/// `CREATE_NO_WINDOW`, a child spawned from a console-less parent (the
/// hidden-console daemon, a GUI process) — or from a console-attached parent (the
/// CLI, tests, a stale shim) — materializes a visible console window that flashes
/// on screen. Routing every plain console spawn through [`configure_windowless`]
/// keeps that flag opt-out-free so new spawn sites cannot silently re-acquire the
/// flash bug.
///
/// [`ProcessTree::spawn`] keeps its own `CREATE_SUSPENDED | CREATE_NO_WINDOW`
/// because it additionally needs the suspended start to assign a Job Object before
/// the child runs; it references the same windows-sys constant, so there is still
/// one source of truth for the value.
mod windowless {
    /// Configure `cmd` so a spawned console child runs with an invisible console
    /// instead of materializing a window. No-op off Windows. See the module docs
    /// for why every plain console spawn should go through this.
    #[cfg(windows)]
    pub fn configure_windowless(cmd: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    /// No-op on non-Windows platforms: there is no console window to suppress.
    #[cfg(not(windows))]
    pub fn configure_windowless(_cmd: &mut std::process::Command) {}
}

/// Bounded, detachable byte-collecting reader for a cancellable child's pipe.
///
/// Both the daemon's draft-provider runner and `hitch-git::run_command` capture a
/// [`ProcessTree`] child's stdout/stderr on dedicated threads and must be able to
/// *give up* on a reader that never reaches EOF without losing the bytes it
/// already saw. This is the one place that chunk-loop / channel / grace logic
/// lives; the two call sites wrap it with their own payload type (`Vec<u8>` vs a
/// lossy `String`) and error surface.
///
/// ## Why a *bounded* drain, and the recycled-pgid tie-in
///
/// On the normal-exit path the child exited on its own, so its pipes are at EOF
/// and the reader finishes immediately — the fast path returns the complete
/// output exactly as a plain join would. But a same-group descendant can inherit
/// the captured write end and outlive the child, and once the leader is reaped we
/// cannot group-kill that descendant on Unix to force EOF: its pgid may already
/// have been recycled to an unrelated group (the hazard
/// [`ProcessTree::terminate_after_leader_reaped`] and [`ProcessTreeRegistration`]
/// guard against). So the reader appends to a shared buffer chunk-by-chunk — never
/// `read_to_end` — and signals completion over a oneshot-style channel. The
/// `done` receiver lets a [`PipeReader::drain_bounded`] wait for true EOF with a
/// deadline and detach the still-parked thread, recovering whatever bytes the
/// command already wrote.
mod pipe_reader {
    use std::io::{self, Read};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    /// A child stdout/stderr reader thread plus the channels needed to drain it
    /// without blocking forever. See the module docs for the rationale.
    pub struct PipeReader {
        handle: thread::JoinHandle<io::Result<()>>,
        buffer: Arc<Mutex<Vec<u8>>>,
        done: mpsc::Receiver<()>,
    }

    /// Outcome of a bounded reader drain on the normal-exit path.
    ///
    /// The distinction matters when the collected bytes *are* the result (a
    /// successful command's stdout): a `Drained` value is the complete output and
    /// safe to consume, while a `TimedOut` value is only the bytes collected
    /// before the grace elapsed — possibly truncated — so the caller must decide
    /// whether partial output is acceptable rather than treating it as complete.
    ///
    /// The payload type defaults to the raw `Vec<u8>` [`drain_bounded`] produces;
    /// [`DrainOutcome::map`] lets a caller decode it (e.g. to a lossy `String`)
    /// while preserving the load-bearing `Drained`/`TimedOut` distinction.
    ///
    /// [`drain_bounded`]: PipeReader::drain_bounded
    pub enum DrainOutcome<T = Vec<u8>> {
        /// The reader reached EOF (or a read error) within the grace period; the
        /// bytes are the complete output.
        Drained(T),
        /// The grace elapsed with the reader still parked (a descendant held the
        /// captured write end open). The bytes are whatever arrived so far and may
        /// be truncated.
        TimedOut(T),
    }

    impl<T> DrainOutcome<T> {
        /// Transform the collected payload while preserving the variant, so a
        /// caller can decode the raw bytes (e.g. `String::from_utf8_lossy`)
        /// without losing the `Drained`/`TimedOut` distinction.
        pub fn map<U>(self, f: impl FnOnce(T) -> U) -> DrainOutcome<U> {
            match self {
                DrainOutcome::Drained(value) => DrainOutcome::Drained(f(value)),
                DrainOutcome::TimedOut(value) => DrainOutcome::TimedOut(f(value)),
            }
        }

        /// The collected payload regardless of whether the drain completed. Use
        /// only where partial output is acceptable (e.g. stderr context on the
        /// failure path).
        pub fn into_inner(self) -> T {
            match self {
                DrainOutcome::Drained(value) | DrainOutcome::TimedOut(value) => value,
            }
        }
    }

    impl PipeReader {
        /// Spawn a reader thread that drains `pipe` chunk-by-chunk into a shared
        /// buffer until EOF or a read error.
        pub fn spawn<R: Read + Send + 'static>(mut pipe: R) -> Self {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let (done_tx, done) = mpsc::channel();
            let thread_buffer = Arc::clone(&buffer);
            let handle = thread::spawn(move || {
                // Read in chunks (rather than `read_to_end`) so the shared buffer
                // holds the bytes seen so far even if the pipe never reaches EOF
                // and this thread ends up detached.
                let mut chunk = [0u8; 8 * 1024];
                let result = loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break Ok(()),
                        Ok(n) => thread_buffer.lock().unwrap().extend_from_slice(&chunk[..n]),
                        Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => break Err(err),
                    }
                };
                // Signal EOF/error to the bounded drain. A send failure just means
                // the caller already stopped waiting, which is fine.
                let _ = done_tx.send(());
                result
            });
            Self {
                handle,
                buffer,
                done,
            }
        }

        /// Whether the reader thread has finished (reached EOF or errored).
        pub fn is_finished(&self) -> bool {
            self.handle.is_finished()
        }

        /// Snapshot the bytes read so far. Used when a stuck reader is detached so
        /// partial command output still survives.
        pub fn collected(&self) -> Vec<u8> {
            self.buffer.lock().unwrap().clone()
        }

        /// Join a reader whose thread is known to be (or about to be) finished —
        /// e.g. a cancel path killed the tree while the leader was alive, so the
        /// reader gets EOF and this returns the complete output. Returns the read
        /// thread's `io::Error` if the read loop failed, or an "output reader
        /// panicked" error if the thread panicked.
        pub fn join(self) -> io::Result<Vec<u8>> {
            let Self { handle, buffer, .. } = self;
            match handle.join() {
                Ok(result) => result?,
                Err(_) => return Err(io::Error::other("command output reader panicked")),
            }
            Ok(Arc::try_unwrap(buffer)
                .map(|mutex| mutex.into_inner().unwrap())
                .unwrap_or_else(|shared| shared.lock().unwrap().clone()))
        }

        /// Drain a reader on the NORMAL-EXIT path with a bounded wait.
        ///
        /// The command exited on its own, so its pipes *should* be at EOF and the
        /// reader already finished — that fast path joins the thread and returns
        /// the complete output as [`DrainOutcome::Drained`]. But a same-group
        /// descendant can inherit the captured write end and outlive the command,
        /// and once the leader is reaped we cannot group-kill that descendant on
        /// Unix to force EOF (the recycled-pgid hazard — see the module docs and
        /// [`crate::ProcessTree::terminate_after_leader_reaped`]). Rather than
        /// block forever, wait for EOF with `grace`; if it never comes, detach the
        /// parked thread and return the bytes collected so far as
        /// [`DrainOutcome::TimedOut`].
        pub fn drain_bounded(self, grace: Duration) -> io::Result<DrainOutcome> {
            match self.done.recv_timeout(grace) {
                // EOF (or read error) reached: the thread is done, so join it to
                // surface any error and return the complete output.
                Ok(()) => self.join().map(DrainOutcome::Drained),
                // No EOF within the grace period: a descendant still holds the
                // write end. Return whatever the command already wrote, tagged as
                // possibly truncated.
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    Ok(DrainOutcome::TimedOut(self.collected()))
                }
                // The sender dropped without signalling (thread panicked before the
                // send); fall back to the collected bytes rather than blocking on a
                // join. The panic means the read loop aborted, so treat this as a
                // completed (if possibly short) read rather than a still-parked
                // reader.
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Ok(DrainOutcome::Drained(self.collected()))
                }
            }
        }
    }
}

#[cfg(test)]
mod pipe_reader_tests {
    use std::io::{self, Read};
    use std::time::Duration;

    use crate::{DrainOutcome, PipeReader};

    /// A reader over an in-memory cursor reaches EOF, so a bounded drain returns
    /// the complete bytes as `Drained`.
    #[test]
    fn drain_bounded_returns_drained_on_eof() {
        let reader = PipeReader::spawn(io::Cursor::new(b"hello world".to_vec()));
        match reader.drain_bounded(Duration::from_secs(5)).unwrap() {
            DrainOutcome::Drained(bytes) => assert_eq!(bytes, b"hello world"),
            DrainOutcome::TimedOut(_) => panic!("expected Drained on EOF"),
        }
    }

    /// Joining a finished reader returns the complete output.
    #[test]
    fn join_returns_complete_output() {
        let reader = PipeReader::spawn(io::Cursor::new(b"payload".to_vec()));
        assert_eq!(reader.join().unwrap(), b"payload");
    }

    /// A reader whose pipe never reaches EOF times out, and the bounded drain
    /// returns whatever bytes arrived so far as `TimedOut` rather than blocking.
    #[test]
    fn drain_bounded_times_out_on_stuck_pipe() {
        // A pipe that yields a chunk then blocks forever (never EOF) models a
        // descendant that inherited the write end and never closed it.
        struct StuckPipe {
            sent: bool,
        }
        impl Read for StuckPipe {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.sent {
                    self.sent = true;
                    let data = b"partial";
                    buf[..data.len()].copy_from_slice(data);
                    return Ok(data.len());
                }
                // Block "forever" relative to the test grace; long enough that the
                // drain times out first, short enough not to hang the suite.
                std::thread::sleep(Duration::from_secs(30));
                Ok(0)
            }
        }

        let reader = PipeReader::spawn(StuckPipe { sent: false });
        // Give the reader a moment to read the first chunk before draining.
        std::thread::sleep(Duration::from_millis(100));
        match reader.drain_bounded(Duration::from_millis(200)).unwrap() {
            DrainOutcome::TimedOut(bytes) => assert_eq!(bytes, b"partial"),
            DrainOutcome::Drained(_) => panic!("expected TimedOut on a stuck pipe"),
        }
    }

    /// `is_finished` flips true once the reader reaches EOF.
    #[test]
    fn is_finished_reflects_eof() {
        let reader = PipeReader::spawn(io::Cursor::new(b"x".to_vec()));
        // Poll briefly for the thread to finish reading the tiny buffer.
        for _ in 0..50 {
            if reader.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(reader.is_finished());
        assert_eq!(reader.collected(), b"x");
    }
}
