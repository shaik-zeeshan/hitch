//! `hitch-process` — kill-the-whole-tree process primitives (ADR 0012).
//!
//! A *leaf* crate (ADR 0005): no in-workspace dependencies, so feature crates
//! (`hitch-pty`, `hitch-git`) and the daemon can all share one implementation
//! of "terminate this child *and every descendant*" without depending on each
//! other. Unix uses process groups; Windows uses Job Objects.

#[cfg(windows)]
pub use job_object::JobHandle;
pub use process_tree::ProcessTree;
pub use registration::ProcessTreeRegistration;

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
