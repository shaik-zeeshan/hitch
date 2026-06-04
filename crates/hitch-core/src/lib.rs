//! `hitch-core` — shared domain types for Hitch.
//!
//! The leaf of the workspace DAG (ADR 0005): this crate defines the vocabulary
//! every other crate speaks — [`Project`], [`Worktree`], [`Session`],
//! [`AgentState`], and their ids — and depends on nothing else in the workspace.
//! Every type is `serde`-serializable so it can cross the `hitch-proto` wire and
//! land in the `hitch-store` SQLite database unchanged.

mod agent;
mod ids;
mod project;
mod session;
mod worktree;

pub use agent::AgentState;
pub use ids::{JobId, ProjectId, SessionId, WorktreeId};
pub use process_tree::ProcessTree;
pub use project::{Project, ProjectKind};
pub use session::{Session, SessionParent};
pub use worktree::Worktree;

/// Environment variable Hitch sets in every PTY session so agent hooks launched
/// from that shell can report state against the correct Hitch session tab.
pub const SESSION_ID_ENV: &str = "HITCH_SESSION_ID";

mod process_tree {
    use std::process::{Child, Command};

    /// Platform process-tree handle for long-running cancellable child processes.
    ///
    /// Unix uses a fresh process group and kills that group. Windows uses a Job
    /// Object configured with `KILL_ON_JOB_CLOSE`, held alive by cloned handles.
    /// Other platforms retain the child pid only so callers can still use direct
    /// child termination while keeping this API portable.
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
        }
    }

    #[cfg(windows)]
    mod imp {
        use std::io;
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        use std::process::{Child, Command};
        use std::sync::Arc;

        use windows_sys::Win32::Foundation::{
            CloseHandle, RtlNtStatusToDosError, HANDLE, NTSTATUS,
        };
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

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

        #[derive(Debug)]
        struct JobHandle(HANDLE);

        unsafe impl Send for JobHandle {}
        unsafe impl Sync for JobHandle {}

        impl Drop for JobHandle {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }

        impl ProcessTree {
            pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
                let job = create_kill_on_close_job()?;
                command.creation_flags(CREATE_SUSPENDED);
                let mut child = command.spawn()?;
                let process = child.as_raw_handle() as HANDLE;
                // On Windows 8+ a process that inherited a parent job can still be
                // nested into this fresh job, so assignment normally succeeds. It
                // can legitimately fail when the parent job forbids nesting (e.g.
                // it was created with `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`) or on
                // pre-Win8 systems. Treat that as a soft failure: drop the job and
                // keep the child rather than killing every spawn under such a
                // parent. The job only existed to reach *descendants* on kill;
                // without it `terminate` falls back to the caller's direct child
                // kill, so the process is still cancellable.
                let assigned = unsafe { AssignProcessToJobObject(job.0, process) };
                let job = if assigned == 0 {
                    None
                } else {
                    Some(Arc::new(job))
                };
                if let Err(error) = resume_process(process) {
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
                let result = unsafe { TerminateJobObject(job.0, 1) };
                if result == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
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

        fn create_kill_on_close_job() -> io::Result<JobHandle> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let result = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if result == 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    CloseHandle(handle);
                }
                return Err(error);
            }

            Ok(JobHandle(handle))
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
        }
    }
}
