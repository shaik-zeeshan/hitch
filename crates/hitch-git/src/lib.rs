//! `hitch-git` — git for Hitch (ADR 0004).
//!
//! This crate is Hitch's focused git layer. Reads use libgit2 via [`git2`]
//! (status, diffs, branches, log, dirty checks); writes and network operations
//! use the user's `git` executable so hooks, signing, credential helpers, and
//! config behave exactly like a terminal. Pull-request creation shells out to
//! `gh` and deliberately stays GitHub-only for the first cut.
//!
//! A feature crate: depends only on `hitch-core`, never on another Hitch feature
//! crate.

use git2::{BranchType, DiffFormat, DiffOptions, Oid, Repository, Status, StatusOptions};
use hitch_core::{ProjectId, Worktree};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::NamedTempFile;

/// Convenient result alias for git operations.
pub type Result<T> = std::result::Result<T, GitError>;

/// A discovered non-bare git worktree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepository {
    root: PathBuf,
}

impl GitRepository {
    /// Discover a git repository from any path inside it and return its workdir root.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::discover(path.as_ref())?;
        let root = repo
            .workdir()
            .ok_or(GitError::BareRepository)?
            .to_path_buf();
        Ok(Self { root })
    }

    /// The repository worktree root on disk.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read status using libgit2.
    pub fn status(&self) -> Result<StatusSummary> {
        status(&self.root)
    }

    /// True when the repo has staged, unstaged, conflicted, or untracked changes.
    pub fn is_dirty(&self) -> Result<bool> {
        is_dirty(&self.root)
    }

    /// Read a file-level patch for either the staged or unstaged/worktree side.
    pub fn diff_file(&self, path: impl AsRef<Path>, target: DiffTarget) -> Result<String> {
        diff_file(&self.root, path, target)
    }

    /// List local and remote branches.
    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        branches(&self.root)
    }

    /// Return recent commits from `HEAD`, newest first.
    pub fn log(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        log(&self.root, limit)
    }

    /// Return commits reachable from `HEAD` but not from `base`, newest first.
    pub fn commits_since(&self, base: &str, limit: usize) -> Result<Vec<CommitInfo>> {
        commits_since(&self.root, base, limit)
    }

    /// Return how many commits the current branch is ahead/behind its upstream.
    /// Returns `(0, 0)` when no upstream is configured.
    pub fn ahead_behind(&self) -> Result<(u32, u32)> {
        ahead_behind(&self.root)
    }

    /// Return changed file paths for `HEAD` relative to `base`.
    pub fn changed_paths_since(&self, base: &str) -> Result<Vec<PathBuf>> {
        changed_paths_since(&self.root, base)
    }

    /// Return branch diff text for `HEAD` relative to `base`.
    pub fn diff_since(&self, base: &str) -> Result<String> {
        diff_since(&self.root, base)
    }

    /// Compare `HEAD` to `base` in a single pass: the commit list, the changed
    /// paths, and the patch text are all derived from one repo open and one
    /// `git2::Diff`. Use this when a caller needs more than one of those (e.g.
    /// PR-draft generation) to avoid re-discovering the repo and rebuilding the
    /// branch diff several times.
    pub fn branch_comparison(&self, base: &str, limit: usize) -> Result<BranchComparison> {
        branch_comparison(&self.root, base, limit)
    }

    /// Best-effort default branch detection: `origin/HEAD`, then current branch.
    pub fn default_branch(&self) -> Result<String> {
        default_branch(&self.root)
    }

    /// Return the branch currently checked out in this worktree, including unborn branches.
    pub fn current_branch(&self) -> Result<String> {
        current_branch(&self.root)
    }

    /// List every working tree git knows about for this repository: the main
    /// worktree plus each linked worktree on disk. Used to import worktrees a
    /// project already has (created outside Hitch) so they auto-appear.
    pub fn worktrees(&self) -> Result<Vec<DiscoveredWorktree>> {
        discover_worktrees(&self.root)
    }
}

/// A working tree git reports for a repository — the main checkout or a linked
/// worktree — with the branch it currently has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorktree {
    /// Absolute path to the working tree on disk.
    pub path: PathBuf,
    /// The branch checked out there (falls back to the worktree name if detached).
    pub branch: String,
    /// True for the repository's main worktree (the original checkout).
    pub is_main: bool,
}

/// Paths and executables used for write-side commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitClient {
    git: PathBuf,
    gh: PathBuf,
}
/// Hooks a long-running `git`/`gh` child into a higher-level cancellation path.
///
/// The daemon's Job registry implements this so `ShutdownDaemon`/`CancelJob`
/// can kill the subprocess group and wait for the worker to drain before exit.
pub trait CommandControl {
    fn is_cancelled(&self) -> bool;
    fn set_child_pgid(&self, pgid: Option<i32>);
}

impl Default for GitClient {
    fn default() -> Self {
        Self {
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
        }
    }
}

impl GitClient {
    /// Use custom `git`/`gh` program paths. This is primarily for daemon config
    /// and tests that stub command invocations.
    pub fn with_programs(git: impl Into<PathBuf>, gh: impl Into<PathBuf>) -> Self {
        Self {
            git: git.into(),
            gh: gh.into(),
        }
    }

    /// Stage whole files (`git add -- <paths...>`).
    pub fn stage_files(
        &self,
        repo_path: impl AsRef<Path>,
        paths: &[PathBuf],
    ) -> Result<CommandOutput> {
        if paths.is_empty() {
            return Ok(CommandOutput::default());
        }
        let mut args = vec![os("add"), os("--")];
        args.extend(paths.iter().map(path_os));
        self.run_git(repo_path.as_ref(), args)
    }

    /// Unstage whole files (`git reset -- <paths...>`).
    ///
    /// `git reset` is used rather than `git restore --staged` because the latter
    /// resolves `HEAD` and so fails on a repository with no commits yet (the
    /// initial-commit case, where every file shows as staged). `git reset --`
    /// unstages correctly whether or not `HEAD` exists, and keeps any
    /// working-tree edits intact.
    pub fn unstage_files(
        &self,
        repo_path: impl AsRef<Path>,
        paths: &[PathBuf],
    ) -> Result<CommandOutput> {
        if paths.is_empty() {
            return Ok(CommandOutput::default());
        }
        let mut args = vec![os("reset"), os("--")];
        args.extend(paths.iter().map(path_os));
        self.run_git(repo_path.as_ref(), args)
    }

    /// Discard whole-file changes, including staged edits and untracked files.
    ///
    /// Hitch's Changes list is file-oriented, so discard intentionally resets
    /// both the index and working tree for each selected path. Untracked entries
    /// are removed with `git clean`; newly staged files are removed with
    /// `git rm`, which also works before the first commit.
    pub fn discard_files(
        &self,
        repo_path: impl AsRef<Path>,
        paths: &[PathBuf],
    ) -> Result<CommandOutput> {
        if paths.is_empty() {
            return Ok(CommandOutput::default());
        }

        let repo_path = repo_path.as_ref();
        let summary = status(repo_path)?;
        let mut output = CommandOutput::default();

        for path in paths {
            let Some(entry) = summary.entry_for(path) else {
                continue;
            };

            if entry.index == FileState::New {
                output = self.run_git(
                    repo_path,
                    vec![os("rm"), os("-f"), os("-r"), os("--"), path_os(path)],
                )?;
                continue;
            }

            if entry.index != FileState::Unmodified {
                let mut args = vec![os("restore"), os("--staged"), os("--worktree"), os("--")];
                if let Some(old_path) = &entry.old_path {
                    args.push(path_os(old_path));
                }
                args.push(path_os(path));
                output = self.run_git(repo_path, args)?;
            } else if !matches!(entry.working_tree, FileState::Unmodified | FileState::New) {
                let restore_path = entry.old_path.as_ref().unwrap_or(path);
                output = self.run_git(
                    repo_path,
                    vec![
                        os("restore"),
                        os("--worktree"),
                        os("--"),
                        path_os(restore_path),
                    ],
                )?;
            }

            if matches!(entry.working_tree, FileState::New | FileState::Renamed) {
                output = self.run_git(
                    repo_path,
                    vec![os("clean"), os("-fd"), os("--"), path_os(path)],
                )?;
            }
        }

        Ok(output)
    }

    /// Commit staged changes using the system git executable and a message file.
    pub fn commit(
        &self,
        repo_path: impl AsRef<Path>,
        subject: &str,
        body: Option<&str>,
    ) -> Result<CommandOutput> {
        let repo_path = repo_path.as_ref();
        let message_path = write_temp_commit_message(subject, body)?;
        let result = self.run_git(
            repo_path,
            vec![os("commit"), os("-F"), path_os(&message_path)],
        );
        let _ = fs::remove_file(&message_path);
        result
    }

    /// Clone a repository using the system git executable.
    pub fn clone_repo(
        &self,
        remote_url: &str,
        destination: impl AsRef<Path>,
    ) -> Result<CommandOutput> {
        self.run_git(
            Path::new("."),
            vec![os("clone"), os(remote_url), path_os(destination.as_ref())],
        )
    }

    /// Clone a repository as a cancellable child process.
    pub fn clone_repo_with_control(
        &self,
        remote_url: &str,
        destination: impl AsRef<Path>,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        self.run_git_with_control(
            Path::new("."),
            vec![os("clone"), os(remote_url), path_os(destination.as_ref())],
            control,
        )
    }

    /// Push a branch using the system git executable.
    pub fn push(
        &self,
        repo_path: impl AsRef<Path>,
        remote: &str,
        branch: &str,
        set_upstream: bool,
    ) -> Result<CommandOutput> {
        let mut args = vec![os("push")];
        if set_upstream {
            args.push(os("-u"));
        }
        args.extend([os(remote), os(branch)]);
        self.run_git(repo_path.as_ref(), args)
    }

    /// Push a branch as a cancellable child process.
    pub fn push_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        remote: &str,
        branch: &str,
        set_upstream: bool,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        let mut args = vec![os("push")];
        if set_upstream {
            args.push(os("-u"));
        }
        args.extend([os(remote), os(branch)]);
        self.run_git_with_control(repo_path.as_ref(), args, control)
    }

    /// Fetch from a remote using the system git executable.
    pub fn fetch(&self, repo_path: impl AsRef<Path>, remote: &str) -> Result<CommandOutput> {
        self.run_git(repo_path.as_ref(), vec![os("fetch"), os(remote)])
    }

    /// Pull a branch using the system git executable (relies on user's pull
    /// config — ff/merge/rebase).
    pub fn pull(
        &self,
        repo_path: impl AsRef<Path>,
        remote: &str,
        branch: &str,
    ) -> Result<CommandOutput> {
        self.run_git(repo_path.as_ref(), vec![os("pull"), os(remote), os(branch)])
    }

    /// Pull a branch as a cancellable child process.
    pub fn pull_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        remote: &str,
        branch: &str,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        self.run_git_with_control(
            repo_path.as_ref(),
            vec![os("pull"), os(remote), os(branch)],
            control,
        )
    }

    /// Create a Hitch-managed worktree under `managed_root/<project>/<branch>`.
    pub fn create_worktree(
        &self,
        repo_path: impl AsRef<Path>,
        request: &CreateWorktreeRequest,
    ) -> Result<Worktree> {
        create_worktree_with_client(self, repo_path.as_ref(), request, None)
    }

    /// Create a Hitch-managed worktree as a cancellable child process.
    pub fn create_worktree_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        request: &CreateWorktreeRequest,
        control: &dyn CommandControl,
    ) -> Result<Worktree> {
        create_worktree_with_client(self, repo_path.as_ref(), request, Some(control))
    }

    /// Remove a worktree with git's own safety checks, keeping the branch by default.
    pub fn remove_worktree(
        &self,
        repo_path: impl AsRef<Path>,
        request: &RemoveWorktreeRequest,
    ) -> Result<CommandOutput> {
        remove_worktree_with_client(self, repo_path.as_ref(), request)
    }

    /// Push if necessary, then create a GitHub PR using `gh pr create`.
    pub fn create_pr(
        &self,
        repo_path: impl AsRef<Path>,
        request: &CreatePrRequest,
    ) -> Result<String> {
        create_pr_with_client(self, repo_path.as_ref(), request, None)
    }

    /// Create a GitHub PR as a cancellable child process.
    pub fn create_pr_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        request: &CreatePrRequest,
        control: &dyn CommandControl,
    ) -> Result<String> {
        create_pr_with_client(self, repo_path.as_ref(), request, Some(control))
    }

    /// Look up the PR (if any) GitHub associates with the worktree's current
    /// branch via `gh pr view`. Returns `Ok(None)` when there is no PR for the
    /// branch (or when `gh` can't determine one — unauthenticated, no remote,
    /// offline): callers treat "unknown" the same as "none" and keep offering
    /// Create-PR, so a transient failure never blocks the flow. Only the JSON
    /// shape is trusted; malformed output also degrades to `None`.
    pub fn pr_status(&self, repo_path: impl AsRef<Path>) -> Result<Option<PrInfo>> {
        pr_status_with_client(self, repo_path.as_ref(), None)
    }

    /// Look up PR status as a cancellable `gh pr view` child process.
    pub fn pr_status_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        control: &dyn CommandControl,
    ) -> Result<Option<PrInfo>> {
        pr_status_with_client(self, repo_path.as_ref(), Some(control))
    }

    /// Look up the current user's PRs for a specific set of head branches in one
    /// cancellable `gh pr list`. Each returned PR is paired with its head branch
    /// so the caller can map them back to worktrees; a branch may appear more than
    /// once (e.g. an old merged PR and a newer open one), so the caller picks which
    /// wins. See [`pr_list_for_branches_with_client`] for why the search is scoped.
    pub fn pr_list_for_branches_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        branches: &[String],
        control: &dyn CommandControl,
    ) -> Result<Vec<(String, PrInfo)>> {
        pr_list_for_branches_with_client(self, repo_path.as_ref(), branches, Some(control))
    }

    fn run_git(&self, cwd: &Path, args: Vec<OsString>) -> Result<CommandOutput> {
        run_command(&self.git, cwd, args, None)
    }

    fn run_git_with_control(
        &self,
        cwd: &Path,
        args: Vec<OsString>,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        run_command(&self.git, cwd, args, Some(control))
    }

    fn run_gh(&self, cwd: &Path, args: Vec<OsString>) -> Result<CommandOutput> {
        run_command(&self.gh, cwd, args, None)
    }

    fn run_gh_with_control(
        &self,
        cwd: &Path,
        args: Vec<OsString>,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        run_command(&self.gh, cwd, args, Some(control))
    }
}

fn pr_status_with_client(
    client: &GitClient,
    repo_path: &Path,
    control: Option<&dyn CommandControl>,
) -> Result<Option<PrInfo>> {
    let args = vec![
        os("pr"),
        os("view"),
        os("--json"),
        os("number,url,state,isDraft"),
    ];
    let output = match control {
        Some(control) => client.run_gh_with_control(repo_path, args, control),
        None => client.run_gh(repo_path, args),
    };
    let output = match output {
        Ok(output) => output,
        Err(err @ GitError::CommandFailed { .. }) => {
            if control.is_some_and(|control| control.is_cancelled()) {
                return Err(err);
            }
            // gh exits non-zero when the branch has no PR (and on auth/network
            // errors). Either way we have no PR to show.
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&output.stdout) else {
        return Ok(None);
    };
    let (Some(number), Some(url)) = (value["number"].as_u64(), value["url"].as_str()) else {
        return Ok(None);
    };
    Ok(Some(PrInfo {
        number,
        url: url.to_string(),
        state: value["state"].as_str().unwrap_or_default().to_string(),
        draft: value["isDraft"].as_bool().unwrap_or(false),
    }))
}

/// Build the `gh pr list --search` query that scopes a batched PR lookup to PRs
/// whose head is one of `branches`. GitHub treats spaces as `AND`, so multiple
/// branch heads must be explicitly grouped with `OR`: `(head:b1 OR head:b2)`.
/// Do not add `author:@me` here: `gh pr view` for the selected worktree returns
/// the PR for the current branch regardless of author, and the batched path must
/// preserve teammate- or bot-authored PRs for the same branch. Blank branch names
/// (e.g. a detached worktree) contribute no `head:` term.
fn pr_branch_search(branches: &[String]) -> String {
    let head_count = branches.iter().filter(|branch| !branch.is_empty()).count();
    if head_count == 0 {
        return String::new();
    }

    let mut search = String::new();
    if head_count > 1 {
        search.push('(');
    }
    let mut first = true;
    for branch in branches {
        if branch.is_empty() {
            continue;
        }
        if first {
            first = false;
        } else {
            search.push_str(" OR ");
        }
        search.push_str("head:");
        search.push_str(branch);
    }
    if head_count > 1 {
        search.push(')');
    }
    search
}

fn pr_list_for_branches_with_client(
    client: &GitClient,
    repo_path: &Path,
    branches: &[String],
    control: Option<&dyn CommandControl>,
) -> Result<Vec<(String, PrInfo)>> {
    let search = pr_branch_search(branches);
    // No real branches to look up — an empty query would match every PR, which
    // we don't want, so bail before spending a `gh` call.
    if search.is_empty() {
        return Ok(Vec::new());
    }
    // `head:` matches by *prefix* in GitHub search, so a short worktree branch
    // (e.g. `fix`) pulls in every PR whose head merely starts with it (`fix/…`).
    // Without headroom that spillover could evict the exact PR we need under the
    // limit, dropping the chip. Keep it generous; the exact-match
    // filter below discards the spillover once it's fetched.
    let limit = branches
        .len()
        .saturating_mul(8)
        .clamp(100, 1000)
        .to_string();
    // `--state all` so a worktree whose PR has already merged still shows a chip;
    // `headRefName` is the branch we map back to each worktree.
    let args = vec![
        os("pr"),
        os("list"),
        os("--state"),
        os("all"),
        os("--search"),
        os(&search),
        os("--limit"),
        os(&limit),
        os("--json"),
        os("number,url,state,isDraft,headRefName"),
    ];
    let output = match control {
        Some(control) => client.run_gh_with_control(repo_path, args, control),
        None => client.run_gh(repo_path, args),
    };
    let output = match output {
        Ok(output) => output,
        Err(err @ GitError::CommandFailed { .. }) => {
            if control.is_some_and(|control| control.is_cancelled()) {
                return Err(err);
            }
            // No repo / no auth / no network — nothing to show, like pr_status.
            return Ok(Vec::new());
        }
        Err(err) => return Err(err),
    };
    let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(&output.stdout) else {
        return Ok(Vec::new());
    };
    let mut prs = Vec::with_capacity(values.len());
    for value in values {
        let (Some(number), Some(url), Some(head)) = (
            value["number"].as_u64(),
            value["url"].as_str(),
            value["headRefName"].as_str(),
        ) else {
            continue;
        };
        // `head:` is a prefix match, so the OR'd query can return PRs on branches
        // that only start with a requested one. Keep exact matches only, so a
        // prefix hit is never mapped onto the wrong worktree.
        if !branches.iter().any(|branch| branch.as_str() == head) {
            continue;
        }
        prs.push((
            head.to_string(),
            PrInfo {
                number,
                url: url.to_string(),
                state: value["state"].as_str().unwrap_or_default().to_string(),
                draft: value["isDraft"].as_bool().unwrap_or(false),
            },
        ));
    }
    Ok(prs)
}

/// A successful CLI command result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Whole-file status as seen on either the index or working-tree side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Unmodified,
    New,
    Modified,
    Deleted,
    Renamed,
    Typechange,
    Conflicted,
}

/// One changed path from libgit2 status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub index: FileState,
    pub working_tree: FileState,
}

/// Complete status snapshot for a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSummary {
    pub entries: Vec<StatusEntry>,
    pub dirty: bool,
    pub additions: usize,
    pub deletions: usize,
}

impl StatusSummary {
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn entry_for(&self, path: impl AsRef<Path>) -> Option<&StatusEntry> {
        let path = path.as_ref();
        self.entries.iter().find(|entry| entry.path == path)
    }
}

/// Which side of a file diff to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTarget {
    /// `HEAD` ↔ index (what is staged).
    Staged,
    /// Index ↔ working tree (what is unstaged/untracked).
    Worktree,
}

/// Branch metadata for local and remote refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
}

/// A single commit for log views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub summary: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub time_seconds: i64,
}

/// `HEAD`-vs-`base` comparison computed in a single pass: commits, changed
/// paths, and patch text all derived from one repo open and one diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchComparison {
    /// Commits reachable from `HEAD` but not from `base`, newest first.
    pub commits: Vec<CommitInfo>,
    /// Changed file paths for `HEAD` relative to `base`, sorted and deduped.
    pub changed_paths: Vec<PathBuf>,
    /// Unified branch diff text for `HEAD` relative to `base`.
    pub diff: String,
}

/// Whether worktree creation should create a branch or check out an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeCheckout {
    /// `git worktree add -b <branch> <path> <base>`.
    NewBranch,
    /// `git worktree add <path> <branch>`.
    ExistingBranch,
}

/// Request to create a Hitch-managed git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorktreeRequest {
    pub project_id: ProjectId,
    pub project_name: String,
    pub managed_root: PathBuf,
    pub branch: String,
    pub checkout: WorktreeCheckout,
    /// Base branch/ref for [`WorktreeCheckout::NewBranch`]. Defaults to the repo default branch.
    pub base: Option<String>,
}

impl CreateWorktreeRequest {
    pub fn target_path(&self) -> PathBuf {
        managed_worktree_path(&self.managed_root, &self.project_name, &self.branch)
    }
}

/// Request to remove a worktree. Branch deletion is opt-in and must be merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWorktreeRequest {
    pub path: PathBuf,
    pub force: bool,
    pub delete_branch: Option<String>,
}

/// Request to create a GitHub pull request through `gh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePrRequest {
    pub title: String,
    pub body: Option<String>,
    /// Defaults to [`default_branch`].
    pub base: Option<String>,
    /// Defaults to the current branch.
    pub head: Option<String>,
    /// Remote to push to when the branch has no upstream or is ahead. Defaults to `origin`.
    pub remote: Option<String>,
    pub draft: bool,
}

/// An existing GitHub pull request for the current branch, as reported by `gh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    /// `gh`'s PR state, e.g. `OPEN`, `CLOSED`, `MERGED`.
    pub state: String,
    pub draft: bool,
}

/// Errors from read-side libgit2, write-side command execution, and invariants.
#[derive(Debug)]
pub enum GitError {
    Git(git2::Error),
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    BareRepository,
    NoHead,
    BranchNotMerged {
        branch: String,
    },
    CommandFailed {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        code: Option<i32>,
        stdout: Box<str>,
        stderr: Box<str>,
    },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(err) => fmt::Display::fmt(err, f),
            Self::Io(err) => fmt::Display::fmt(err, f),
            Self::Utf8(err) => fmt::Display::fmt(err, f),
            Self::BareRepository => {
                write!(f, "bare repositories do not have a Hitch worktree root")
            }
            Self::NoHead => write!(f, "repository has no HEAD"),
            Self::BranchNotMerged { branch } => {
                write!(f, "branch {branch:?} is not merged into HEAD")
            }
            Self::CommandFailed {
                program,
                args,
                cwd,
                code,
                stderr,
                ..
            } => write!(
                f,
                "command failed in {}: {} {} exited {:?}: {}",
                cwd.display(),
                program,
                args.join(" "),
                code,
                stderr.trim()
            ),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Utf8(err) => Some(err),
            Self::BareRepository
            | Self::NoHead
            | Self::BranchNotMerged { .. }
            | Self::CommandFailed { .. } => None,
        }
    }
}

impl From<git2::Error> for GitError {
    fn from(err: git2::Error) -> Self {
        Self::Git(err)
    }
}

impl From<std::io::Error> for GitError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<std::string::FromUtf8Error> for GitError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Self::Utf8(err)
    }
}

/// Read status using libgit2.
pub fn status(repo_path: impl AsRef<Path>) -> Result<StatusSummary> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        // Keep the app's Changes panel fast in worktrees with generated output
        // or dependency folders that are not ignored yet. The UI can stage a
        // collapsed untracked directory with `git add dir/`; the next status
        // refresh expands only the tracked index entries that Git now knows.
        .recurse_untracked_dirs(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo.statuses(Some(&mut options))?;
    let mut entries = Vec::with_capacity(statuses.len());
    for entry in statuses.iter() {
        let status = entry.status();
        let Some((path, old_path)) = status_paths(entry) else {
            continue;
        };
        entries.push(StatusEntry {
            path,
            old_path,
            index: index_state(status),
            working_tree: worktree_state(status),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let (additions, deletions) = diff_line_stats(&repo)?;
    Ok(StatusSummary {
        dirty: !entries.is_empty(),
        entries,
        additions,
        deletions,
    })
}

fn diff_line_stats(repo: &Repository) -> Result<(usize, usize)> {
    let mut additions = 0;
    let mut deletions = 0;

    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let index = repo.index()?;
    let staged = repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), None)?;
    let staged_stats = staged.stats()?;
    additions += staged_stats.insertions();
    deletions += staged_stats.deletions();

    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false)
        .show_untracked_content(true);
    let index = repo.index()?;
    let worktree = repo.diff_index_to_workdir(Some(&index), Some(&mut options))?;
    let worktree_stats = worktree.stats()?;
    additions += worktree_stats.insertions();
    deletions += worktree_stats.deletions();

    Ok((additions, deletions))
}

/// Fast dirty check for badge/event refreshes that only need a boolean.
pub fn is_dirty(repo_path: impl AsRef<Path>) -> Result<bool> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false);
    let statuses = repo.statuses(Some(&mut options))?;
    Ok(!statuses.is_empty())
}

/// Read a file-level diff using libgit2.
pub fn diff_file(
    repo_path: impl AsRef<Path>,
    path: impl AsRef<Path>,
    target: DiffTarget,
) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let mut options = DiffOptions::new();
    options
        .pathspec(path.as_ref())
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);

    let diff = match target {
        DiffTarget::Staged => {
            let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
            let index = repo.index()?;
            repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut options))?
        }
        DiffTarget::Worktree => {
            let index = repo.index()?;
            repo.diff_index_to_workdir(Some(&index), Some(&mut options))?
        }
    };

    diff_to_string(&diff)
}

/// Read the complete staged diff using libgit2.
pub fn staged_diff(repo_path: impl AsRef<Path>) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let mut index = repo.index()?;
    let diff = repo.diff_tree_to_index(head_tree.as_ref(), Some(&mut index), None)?;
    diff_to_string(&diff)
}

/// List local and remote branches using libgit2.
pub fn branches(repo_path: impl AsRef<Path>) -> Result<Vec<BranchInfo>> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let mut out = Vec::new();

    for branch_type in [BranchType::Local, BranchType::Remote] {
        for branch in repo.branches(Some(branch_type))? {
            let (branch, _) = branch?;
            let Some(name) = branch.name()?.map(str::to_owned) else {
                continue;
            };
            let upstream = if branch_type == BranchType::Local {
                branch
                    .upstream()
                    .ok()
                    .and_then(|upstream| upstream.name().ok().flatten().map(str::to_owned))
            } else {
                None
            };
            out.push(BranchInfo {
                name,
                is_head: branch.is_head(),
                is_remote: branch_type == BranchType::Remote,
                upstream,
            });
        }
    }

    out.sort_by(|a, b| (a.is_remote, &a.name).cmp(&(b.is_remote, &b.name)));
    Ok(out)
}

/// Return recent commits from `HEAD`, newest first.
pub fn log(repo_path: impl AsRef<Path>, limit: usize) -> Result<Vec<CommitInfo>> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;

    commits_from_revwalk(&repo, revwalk, limit)
}

/// Return commits reachable from `HEAD` but not from `base`, newest first.
pub fn commits_since(
    repo_path: impl AsRef<Path>,
    base: &str,
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    let repo = Repository::discover(repo_path.as_ref())?;
    // An unborn HEAD (no commits yet) has nothing reachable, so there are no
    // commits since the base. Degrade gracefully instead of erroring, matching
    // how staged_diff tolerates a missing HEAD.
    let Some(head) = head_commit_oid(&repo) else {
        return Ok(Vec::new());
    };
    let base = resolve_base_oid(&repo, base)?;
    let merge_base = repo.merge_base(head, base).unwrap_or(base);
    let mut revwalk = repo.revwalk()?;
    revwalk.push(head)?;
    revwalk.hide(merge_base)?;
    commits_from_revwalk(&repo, revwalk, limit)
}

/// Return changed file paths for `HEAD` relative to `base`.
pub fn changed_paths_since(repo_path: impl AsRef<Path>, base: &str) -> Result<Vec<PathBuf>> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let diff = diff_since_base(&repo, base)?;
    Ok(diff_changed_paths(&diff))
}

/// Return branch diff text for `HEAD` relative to `base`.
pub fn diff_since(repo_path: impl AsRef<Path>, base: &str) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let diff = diff_since_base(&repo, base)?;
    diff_to_string(&diff)
}

/// Compare `HEAD` to `base` in a single pass, returning the commit list,
/// changed paths, and patch text. Opens the repo once and builds the branch
/// `git2::Diff` once, where the per-function variants would re-discover the
/// repo and rebuild the diff for each piece.
pub fn branch_comparison(
    repo_path: impl AsRef<Path>,
    base: &str,
    limit: usize,
) -> Result<BranchComparison> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let base_oid = resolve_base_oid(&repo, base)?;
    let head = head_commit_oid(&repo);
    let merge_base = head
        .map(|head| repo.merge_base(head, base_oid).unwrap_or(base_oid))
        .unwrap_or(base_oid);

    // Commits reachable from HEAD but not from the merge base. An unborn HEAD
    // contributes no commits.
    let commits = if let Some(head) = head {
        let mut revwalk = repo.revwalk()?;
        revwalk.push(head)?;
        revwalk.hide(merge_base)?;
        commits_from_revwalk(&repo, revwalk, limit)?
    } else {
        Vec::new()
    };

    // One diff drives both the changed-path list and the patch text.
    let base_tree = repo.find_commit(merge_base)?.tree()?;
    let head_tree = match head {
        Some(head) => repo.find_commit(head)?.tree()?,
        None => base_tree.clone(),
    };
    let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;

    let changed_paths = diff_changed_paths(&diff);
    let diff_text = diff_to_string(&diff)?;

    Ok(BranchComparison {
        commits,
        changed_paths,
        diff: diff_text,
    })
}

/// Best-effort default branch detection: `origin/HEAD`, then current branch.
pub fn default_branch(repo_path: impl AsRef<Path>) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;

    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target() {
            if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
                return Ok(name.to_string());
            }
        }
    }

    current_branch_from_repo(&repo)
}

/// Return the branch currently checked out in a worktree, including unborn branches.
pub fn current_branch(repo_path: impl AsRef<Path>) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;
    current_branch_from_repo(&repo)
}

/// List the main worktree plus every linked worktree git tracks for a repo.
///
/// Linked worktrees whose directory no longer exists on disk (stale/prunable
/// entries) are skipped so callers only see worktrees they can actually open.
pub fn discover_worktrees(repo_path: impl AsRef<Path>) -> Result<Vec<DiscoveredWorktree>> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let main_root = repo
        .workdir()
        .ok_or(GitError::BareRepository)?
        .to_path_buf();

    let mut out = vec![DiscoveredWorktree {
        branch: current_branch_from_repo(&repo).unwrap_or_else(|_| "HEAD".into()),
        path: main_root,
        is_main: true,
    }];

    for name in repo.worktrees()?.iter().flatten() {
        let Ok(worktree) = repo.find_worktree(name) else {
            continue;
        };
        let path = worktree.path().to_path_buf();
        // Skip linked worktrees whose directory is gone (prunable but not pruned).
        if !path.is_dir() {
            continue;
        }
        let branch = Repository::open(&path)
            .ok()
            .and_then(|wt_repo| current_branch_from_repo(&wt_repo).ok())
            .unwrap_or_else(|| name.to_string());
        out.push(DiscoveredWorktree {
            path,
            branch,
            is_main: false,
        });
    }

    Ok(out)
}

/// Build the managed worktree path Hitch owns for a project branch.
pub fn managed_worktree_path(
    managed_root: impl AsRef<Path>,
    project_name: &str,
    branch: &str,
) -> PathBuf {
    managed_root
        .as_ref()
        .join(safe_path_component(project_name))
        .join(safe_path_component(branch))
}

fn create_worktree_with_client(
    client: &GitClient,
    repo_path: &Path,
    request: &CreateWorktreeRequest,
    control: Option<&dyn CommandControl>,
) -> Result<Worktree> {
    let target = request.target_path();
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut args = vec![os("worktree"), os("add")];
    match request.checkout {
        WorktreeCheckout::NewBranch => {
            let base = match &request.base {
                Some(base) => base.clone(),
                None => default_branch(repo_path)?,
            };
            args.extend([os("-b"), os(&request.branch), path_os(&target), os(base)]);
        }
        WorktreeCheckout::ExistingBranch => {
            args.extend([path_os(&target), os(&request.branch)]);
        }
    }

    match control {
        Some(control) => client.run_git_with_control(repo_path, args, control)?,
        None => client.run_git(repo_path, args)?,
    };
    Ok(Worktree::new(
        request.project_id,
        target,
        request.branch.clone(),
        false,
        true,
    ))
}

fn remove_worktree_with_client(
    client: &GitClient,
    repo_path: &Path,
    request: &RemoveWorktreeRequest,
) -> Result<CommandOutput> {
    if let Some(branch) = &request.delete_branch {
        if !is_branch_merged(repo_path, branch)? {
            return Err(GitError::BranchNotMerged {
                branch: branch.clone(),
            });
        }
    }

    let mut args = vec![os("worktree"), os("remove")];
    if request.force {
        args.push(os("--force"));
    }
    args.push(path_os(&request.path));
    let output = match client.run_git(repo_path, args) {
        Ok(output) => output,
        Err(err) if already_removed_worktree(&err, &request.path) => CommandOutput::default(),
        Err(err) => return Err(err),
    };

    if let Some(branch) = &request.delete_branch {
        client.run_git(repo_path, vec![os("branch"), os("-d"), os(branch)])?;
    }

    Ok(output)
}

fn already_removed_worktree(err: &GitError, path: &Path) -> bool {
    !path.exists()
        && matches!(
            err,
            GitError::CommandFailed { stderr, .. } if stderr.contains("is not a working tree")
        )
}

fn create_pr_with_client(
    client: &GitClient,
    repo_path: &Path,
    request: &CreatePrRequest,
    control: Option<&dyn CommandControl>,
) -> Result<String> {
    let branch = match &request.head {
        Some(head) => head.clone(),
        None => current_branch(repo_path)?,
    };
    let base = match &request.base {
        Some(base) => base.clone(),
        None => default_branch(repo_path)?,
    };
    let remote = request.remote.as_deref().unwrap_or("origin");

    if branch_needs_push(repo_path, &branch)? {
        match control {
            Some(control) => client.push_with_control(repo_path, remote, &branch, true, control)?,
            None => client.push(repo_path, remote, &branch, true)?,
        };
    }

    let mut args = vec![
        os("pr"),
        os("create"),
        os("--title"),
        os(&request.title),
        os("--base"),
        os(base),
        os("--head"),
        os(&branch),
    ];
    if let Some(body) = &request.body {
        args.extend([os("--body"), os(body)]);
    }
    if request.draft {
        args.push(os("--draft"));
    }

    let output = match control {
        Some(control) => client.run_gh_with_control(repo_path, args, control)?,
        None => client.run_gh(repo_path, args)?,
    };
    Ok(output.stdout.trim().to_string())
}

fn commits_from_revwalk(
    repo: &Repository,
    revwalk: git2::Revwalk<'_>,
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    let mut commits = Vec::new();
    for oid in revwalk.take(limit) {
        let commit = repo.find_commit(oid?)?;
        let author = commit.author();
        commits.push(CommitInfo {
            id: commit.id().to_string(),
            summary: commit.summary().map(str::to_owned),
            author_name: author.name().map(str::to_owned),
            author_email: author.email().map(str::to_owned),
            time_seconds: commit.time().seconds(),
        });
    }
    Ok(commits)
}

fn diff_since_base<'repo>(repo: &'repo Repository, base: &str) -> Result<git2::Diff<'repo>> {
    let base_oid = resolve_base_oid(repo, base)?;
    // On an unborn HEAD the branch carries no commits, so there is nothing to
    // diff against the base. Return an empty diff (base tree against itself)
    // rather than erroring, consistent with staged_diff's missing-HEAD path.
    let Some(head) = head_commit_oid(repo) else {
        let base_tree = repo.find_commit(base_oid)?.tree()?;
        return Ok(repo.diff_tree_to_tree(Some(&base_tree), Some(&base_tree), None)?);
    };
    let merge_base = repo.merge_base(head, base_oid).unwrap_or(base_oid);
    let base_tree = repo.find_commit(merge_base)?.tree()?;
    let head_tree = repo.find_commit(head)?.tree()?;
    Ok(repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?)
}

/// Resolve the OID of the current `HEAD` commit, or `None` on an unborn HEAD.
fn head_commit_oid(repo: &Repository) -> Option<Oid> {
    repo.head().ok().and_then(|head| head.target())
}

/// Collect the changed file paths from a diff, sorted and deduped.
fn diff_changed_paths(diff: &git2::Diff<'_>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(Path::to_path_buf);
        if let Some(path) = path {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn resolve_base_oid(repo: &Repository, base: &str) -> Result<Oid> {
    let candidates = [
        base.to_string(),
        format!("origin/{base}"),
        format!("refs/heads/{base}"),
    ];
    for candidate in candidates {
        if let Ok(object) = repo.revparse_single(&candidate) {
            return Ok(object.peel_to_commit()?.id());
        }
    }
    Err(GitError::Git(git2::Error::from_str(&format!(
        "base branch not found: {base}"
    ))))
}

fn diff_to_string(diff: &git2::Diff<'_>) -> Result<String> {
    let mut out = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        // libgit2 reports the line origin (+/-/space) separately from the line
        // content; re-prepend it for context/added/removed lines so the result
        // is a valid unified diff. File and hunk headers ('F'/'H') already
        // carry their own prefix in `content`, so they're left untouched.
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin() as u8);
        }
        out.extend_from_slice(line.content());
        true
    })?;
    // A non-UTF-8 hunk (e.g. a binary-ish change) should degrade to replacement
    // characters rather than fail the whole best-effort textual diff.
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn write_temp_commit_message(subject: &str, body: Option<&str>) -> Result<PathBuf> {
    // A commit subject must be a single line. LLM-drafted subjects sometimes
    // contain embedded newlines; collapse any internal whitespace runs (which
    // include newlines) into single spaces so they don't leak into the body.
    let mut message = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) {
        message.push_str("\n\n");
        message.push_str(body);
        message.push('\n');
    } else {
        message.push('\n');
    }

    // Create a temp file with mode 0600 (owner read/write only) to avoid exposing
    // commit messages containing credentials or private details to other users.
    let mut temp_file = NamedTempFile::new_in(std::env::temp_dir())?;
    temp_file.write_all(message.as_bytes())?;
    let (_, path) = temp_file.keep().map_err(|err| GitError::Io(err.error))?;
    Ok(path)
}

fn current_branch_from_repo(repo: &Repository) -> Result<String> {
    if let Ok(head) = repo.head() {
        if let Some(branch) = branch_name_from_head(&head) {
            return Ok(branch);
        }
    }

    if let Ok(head) = repo.find_reference("HEAD") {
        if let Some(branch) = branch_name_from_head(&head) {
            return Ok(branch);
        }
    }

    Err(GitError::NoHead)
}

fn branch_name_from_head(reference: &git2::Reference<'_>) -> Option<String> {
    if let Some(target) = reference.symbolic_target() {
        return target.strip_prefix("refs/heads/").map(str::to_owned);
    }

    match reference.shorthand()? {
        "HEAD" => None,
        branch => Some(branch.to_owned()),
    }
}

fn ahead_behind(repo_path: &Path) -> Result<(u32, u32)> {
    let repo = Repository::discover(repo_path)?;
    let head = repo.head()?;
    let local_oid = head.target().ok_or(GitError::NoHead)?;
    let local_branch = match head.shorthand() {
        Some(name) => name.to_owned(),
        None => return Ok((0, 0)),
    };
    let local = match repo.find_branch(&local_branch, BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok((0, 0)),
    };
    let upstream = match local.upstream() {
        Ok(u) => u,
        Err(_) => return Ok((0, 0)),
    };
    let upstream_oid = branch_target_oid(&upstream)?;
    let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;
    Ok((
        ahead.min(u32::MAX as usize) as u32,
        behind.min(u32::MAX as usize) as u32,
    ))
}

fn branch_needs_push(repo_path: &Path, branch: &str) -> Result<bool> {
    let repo = Repository::discover(repo_path)?;
    let local = repo.find_branch(branch, BranchType::Local)?;
    let local_oid = branch_target_oid(&local)?;
    let upstream = match local.upstream() {
        Ok(upstream) => upstream,
        Err(_) => return Ok(true),
    };
    let upstream_oid = branch_target_oid(&upstream)?;
    let (ahead, _) = repo.graph_ahead_behind(local_oid, upstream_oid)?;
    Ok(ahead > 0)
}

fn is_branch_merged(repo_path: &Path, branch: &str) -> Result<bool> {
    let repo = Repository::discover(repo_path)?;
    let head_oid = repo.head()?.target().ok_or(GitError::NoHead)?;
    let branch = repo.find_branch(branch, BranchType::Local)?;
    let branch_oid = branch_target_oid(&branch)?;
    Ok(head_oid == branch_oid || repo.graph_descendant_of(head_oid, branch_oid)?)
}

fn branch_target_oid(branch: &git2::Branch<'_>) -> Result<Oid> {
    branch
        .get()
        .target()
        .ok_or_else(|| GitError::Git(git2::Error::from_str("branch has no direct target")))
}

fn run_command(
    program: &Path,
    cwd: &Path,
    args: Vec<OsString>,
    control: Option<&dyn CommandControl>,
) -> Result<CommandOutput> {
    let Some(control) = control else {
        let output = Command::new(program)
            .current_dir(cwd)
            .args(&args)
            .output()?;
        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8(output.stderr)?;
        if output.status.success() {
            return Ok(CommandOutput { stdout, stderr });
        }
        return Err(command_failed(
            program,
            cwd,
            &args,
            output.status.code(),
            stdout,
            stderr,
        ));
    };

    if control.is_cancelled() {
        return Err(command_failed(
            program,
            cwd,
            &args,
            None,
            String::new(),
            "command cancelled".to_string(),
        ));
    }

    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn()?;
    control.set_child_pgid(Some(child.id() as i32));
    let stdout_reader = spawn_pipe_reader(
        child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture command stdout"))?,
    );
    let stderr_reader = spawn_pipe_reader(
        child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture command stderr"))?,
    );

    let mut cancelled = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if control.is_cancelled() => {
                cancelled = true;
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                #[cfg(not(unix))]
                {
                    let _ = child.kill();
                }
                break child.wait()?;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    };
    control.set_child_pgid(None);

    let stdout = String::from_utf8(join_pipe_reader(stdout_reader)?)?;
    let stderr = String::from_utf8(join_pipe_reader(stderr_reader)?)?;
    if status.success() && !cancelled {
        Ok(CommandOutput { stdout, stderr })
    } else {
        Err(command_failed(
            program,
            cwd,
            &args,
            status.code(),
            stdout,
            if cancelled && stderr.trim().is_empty() {
                "command cancelled".to_string()
            } else {
                stderr
            },
        ))
    }
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    mut pipe: R,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        pipe.read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_pipe_reader(
    handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::io::Result<Vec<u8>> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::other("command output reader panicked")),
    }
}

fn command_failed(
    program: &Path,
    cwd: &Path,
    args: &[OsString],
    code: Option<i32>,
    stdout: String,
    stderr: String,
) -> GitError {
    GitError::CommandFailed {
        program: program.to_string_lossy().into_owned(),
        args: args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: cwd.to_path_buf(),
        code,
        stdout: stdout.into_boxed_str(),
        stderr: stderr.into_boxed_str(),
    }
}

fn status_paths(entry: git2::StatusEntry<'_>) -> Option<(PathBuf, Option<PathBuf>)> {
    entry
        .head_to_index()
        .or_else(|| entry.index_to_workdir())
        .and_then(|delta| {
            let new = delta.new_file().path();
            let old = delta.old_file().path();
            let path = new.or(old)?;
            let old_path = match (old, new) {
                (Some(old), Some(new)) if old != new => Some(old.to_path_buf()),
                _ => None,
            };
            Some((path.to_path_buf(), old_path))
        })
        .or_else(|| entry.path().map(|path| (PathBuf::from(path), None)))
}

fn index_state(status: Status) -> FileState {
    if status.contains(Status::CONFLICTED) {
        FileState::Conflicted
    } else if status.contains(Status::INDEX_DELETED) {
        FileState::Deleted
    } else if status.contains(Status::INDEX_RENAMED) {
        FileState::Renamed
    } else if status.contains(Status::INDEX_TYPECHANGE) {
        FileState::Typechange
    } else if status.contains(Status::INDEX_MODIFIED) {
        FileState::Modified
    } else if status.contains(Status::INDEX_NEW) {
        FileState::New
    } else {
        FileState::Unmodified
    }
}

fn worktree_state(status: Status) -> FileState {
    if status.contains(Status::CONFLICTED) {
        FileState::Conflicted
    } else if status.contains(Status::WT_DELETED) {
        FileState::Deleted
    } else if status.contains(Status::WT_RENAMED) {
        FileState::Renamed
    } else if status.contains(Status::WT_TYPECHANGE) {
        FileState::Typechange
    } else if status.contains(Status::WT_MODIFIED) {
        FileState::Modified
    } else if status.contains(Status::WT_NEW) {
        FileState::New
    } else {
        FileState::Unmodified
    }
}

fn safe_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();

    match sanitized.trim_matches(['.', ' ']) {
        "" => "_".to_string(),
        trimmed => trimmed.to_string(),
    }
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

fn path_os(path: impl AsRef<Path>) -> OsString {
    path.as_ref().as_os_str().to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tempfile::TempDir;

    #[test]
    fn pr_branch_search_groups_ored_heads_and_skips_blanks() {
        // Parentheses keep the head terms grouped as one branch disjunction.
        assert_eq!(
            pr_branch_search(&["feature".into(), "fix/bug".into()]),
            "(head:feature OR head:fix/bug)"
        );
        // A detached worktree contributes no head term; an empty query is not a
        // branch lookup, so callers treat it as "nothing to ask".
        let only_blank = pr_branch_search(&[String::new()]);
        assert_eq!(only_blank, "");
        assert!(!only_blank.contains("head:"));
    }

    #[test]
    fn pr_list_for_branches_queries_ored_heads_without_author_filter() {
        let fixture = RepoFixture::new();
        let bin = TempDir::new().unwrap();
        let log = bin.path().join("calls.log");
        let gh = fake_program(
            bin.path().join("gh"),
            &log,
            r#"[{"number":41,"url":"https://github.test/pr/41","state":"OPEN","isDraft":false,"headRefName":"feature"},{"number":42,"url":"https://github.test/pr/42","state":"MERGED","isDraft":true,"headRefName":"fix/bug"}]"#,
        );
        let client = GitClient::with_programs("git", gh);
        let branches = vec!["feature".into(), "fix/bug".into()];

        let prs = pr_list_for_branches_with_client(&client, fixture.path(), &branches, None)
            .expect("PR list should parse fake gh output");

        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].0, "feature");
        assert_eq!(prs[0].1.number, 41);
        assert_eq!(prs[1].0, "fix/bug");
        assert_eq!(prs[1].1.number, 42);
        let calls = fs::read_to_string(log).unwrap();
        let gh_call = calls
            .lines()
            .find(|line| line.starts_with("gh pr list "))
            .expect("gh pr list should be invoked");
        assert!(
            gh_call.contains("--search (head:feature OR head:fix/bug)"),
            "unexpected gh call: {gh_call}"
        );
        assert!(
            !gh_call.contains("author:@me"),
            "batched worktree PR lookup must keep teammate-authored PRs: {gh_call}"
        );
    }

    #[test]
    fn status_and_diff_cover_clean_modified_staged_and_untracked() {
        let fixture = RepoFixture::new();
        fixture.write("tracked.txt", "changed\n");
        fixture.write("new.txt", "hello\n");

        let summary = status(fixture.path()).unwrap();
        assert!(summary.dirty);
        assert_eq!(
            summary.entry_for("tracked.txt").unwrap().working_tree,
            FileState::Modified
        );
        assert_eq!(
            summary.entry_for("new.txt").unwrap().working_tree,
            FileState::New
        );
        assert_eq!(summary.additions, 2);
        assert_eq!(summary.deletions, 1);

        let diff = diff_file(fixture.path(), "tracked.txt", DiffTarget::Worktree).unwrap();
        assert!(diff.contains("changed"), "diff was {diff:?}");

        GitClient::default()
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        let summary = status(fixture.path()).unwrap();
        assert_eq!(
            summary.entry_for("tracked.txt").unwrap().index,
            FileState::Modified
        );
        assert_eq!(
            summary.entry_for("tracked.txt").unwrap().working_tree,
            FileState::Unmodified
        );
        let staged = diff_file(fixture.path(), "tracked.txt", DiffTarget::Staged).unwrap();
        assert!(staged.contains("changed"), "staged diff was {staged:?}");
    }

    #[test]
    fn dirty_check_detects_changes_without_full_status_work() {
        let fixture = RepoFixture::new();
        assert!(!is_dirty(fixture.path()).unwrap());

        fixture.write("tracked.txt", "changed\n");
        assert!(is_dirty(fixture.path()).unwrap());
    }

    #[test]
    fn status_collapses_untracked_directories_for_fast_app_reads() {
        let fixture = RepoFixture::new();
        fs::create_dir(fixture.path().join("generated")).unwrap();
        fs::write(fixture.path().join("generated/one.txt"), "one\n").unwrap();
        fs::write(fixture.path().join("generated/two.txt"), "two\n").unwrap();

        let summary = status(fixture.path()).unwrap();

        assert_eq!(summary.entries.len(), 1, "entries were {summary:?}");
        let path = summary.entries[0].path.to_string_lossy();
        assert!(
            path == "generated" || path == "generated/",
            "path was {path:?}"
        );
        assert_eq!(summary.entries[0].working_tree, FileState::New);
    }

    #[test]
    fn git_cli_writes_and_read_path_agree_on_status() {
        let fixture = RepoFixture::new();
        let client = GitClient::default();
        fixture.write("tracked.txt", "write path\n");

        client
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        let staged = status(fixture.path()).unwrap();
        assert_eq!(
            staged.entry_for("tracked.txt").unwrap().index,
            FileState::Modified
        );

        client
            .unstage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        let unstaged = status(fixture.path()).unwrap();
        assert_eq!(
            unstaged.entry_for("tracked.txt").unwrap().working_tree,
            FileState::Modified
        );

        client
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        client
            .commit(fixture.path(), "update tracked", None)
            .unwrap();
        assert!(!status(fixture.path()).unwrap().dirty);
    }

    #[test]
    fn discard_files_resets_tracked_staged_and_untracked_paths() {
        let fixture = RepoFixture::new();
        let client = GitClient::default();

        fixture.write("tracked.txt", "discard me\n");
        fixture.write("staged.txt", "staged\n");
        fixture.git(["add", "staged.txt"]);
        fixture.write("new.txt", "new\n");

        client
            .discard_files(
                fixture.path(),
                &[
                    PathBuf::from("tracked.txt"),
                    PathBuf::from("staged.txt"),
                    PathBuf::from("new.txt"),
                ],
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "initial\n"
        );
        assert!(!fixture.path().join("staged.txt").exists());
        assert!(!fixture.path().join("new.txt").exists());
        assert!(!status(fixture.path()).unwrap().dirty);
    }

    #[test]
    fn discard_files_reverts_renamed_paths() {
        let fixture = RepoFixture::new();
        let client = GitClient::default();

        fixture.git(["mv", "tracked.txt", "moved.txt"]);
        client
            .discard_files(fixture.path(), &[PathBuf::from("moved.txt")])
            .unwrap();

        assert!(fixture.path().join("tracked.txt").exists());
        assert!(!fixture.path().join("moved.txt").exists());
        assert!(!status(fixture.path()).unwrap().dirty);
    }

    #[test]
    fn discard_files_reverts_worktree_renamed_paths() {
        let fixture = RepoFixture::new();
        let client = GitClient::default();

        fs::rename(
            fixture.path().join("tracked.txt"),
            fixture.path().join("moved.txt"),
        )
        .unwrap();
        client
            .discard_files(fixture.path(), &[PathBuf::from("moved.txt")])
            .unwrap();

        assert!(fixture.path().join("tracked.txt").exists());
        assert!(!fixture.path().join("moved.txt").exists());
        assert!(!status(fixture.path()).unwrap().dirty);
    }

    #[test]
    fn discard_staged_new_file_works_before_the_first_commit() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        run_real_git(&path, ["init", "--initial-branch=main"]);
        run_real_git(&path, ["config", "user.name", "Hitch Test"]);
        run_real_git(&path, ["config", "user.email", "hitch@example.test"]);
        fs::write(path.join("first.txt"), "hello\n").unwrap();

        let client = GitClient::default();
        client
            .stage_files(&path, &[PathBuf::from("first.txt")])
            .unwrap();
        client
            .discard_files(&path, &[PathBuf::from("first.txt")])
            .unwrap();

        assert!(!path.join("first.txt").exists());
        assert!(!status(&path).unwrap().dirty);
    }

    #[test]
    fn unstage_works_before_the_first_commit() {
        // A repo with no commits has no resolvable HEAD; every tracked file
        // shows as staged-new. `git restore --staged` errors here, so unstage
        // must fall back to `git reset --`, which untracks the file again.
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        run_real_git(&path, ["init", "--initial-branch=main"]);
        run_real_git(&path, ["config", "user.name", "Hitch Test"]);
        run_real_git(&path, ["config", "user.email", "hitch@example.test"]);
        fs::write(path.join("first.txt"), "hello\n").unwrap();

        let client = GitClient::default();
        client
            .stage_files(&path, &[PathBuf::from("first.txt")])
            .unwrap();
        assert_eq!(
            status(&path).unwrap().entry_for("first.txt").unwrap().index,
            FileState::New
        );

        client
            .unstage_files(&path, &[PathBuf::from("first.txt")])
            .unwrap();
        let entry = status(&path).unwrap();
        let entry = entry.entry_for("first.txt").unwrap();
        assert_eq!(entry.index, FileState::Unmodified);
        assert_eq!(entry.working_tree, FileState::New);
    }

    #[test]
    fn current_branch_reads_unborn_symbolic_head() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        run_real_git(&path, ["init", "--initial-branch=main"]);

        assert_eq!(current_branch(&path).unwrap(), "main");
        assert_eq!(default_branch(&path).unwrap(), "main");
    }

    #[test]
    fn commit_uses_system_git_and_fires_repo_hooks() {
        let fixture = RepoFixture::new();
        let marker = fixture.path().join("hook-ran");
        let hook = fixture.path().join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            format!("#!/bin/sh\necho hook > {}\n", marker.display()),
        )
        .unwrap();
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();

        fixture.write("tracked.txt", "hooked\n");
        let client = GitClient::default();
        client
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        client
            .commit(fixture.path(), "hooked commit", None)
            .unwrap();

        assert_eq!(fs::read_to_string(marker).unwrap(), "hook\n");
    }

    #[test]
    fn commit_preserves_multiline_subject_and_body() {
        let fixture = RepoFixture::new();
        fixture.write("tracked.txt", "body commit\n");
        let client = GitClient::default();
        client
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        client
            .commit(
                fixture.path(),
                "feat: update tracked",
                Some("First body line\n\nSecond body line"),
            )
            .unwrap();

        let output = Command::new("git")
            .current_dir(fixture.path())
            .args(["log", "-1", "--pretty=%B"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let message = String::from_utf8(output.stdout).unwrap();
        assert!(message.starts_with("feat: update tracked\n\nFirst body line\n\nSecond body line"));
    }

    #[test]
    fn lists_branches_and_log() {
        let fixture = RepoFixture::new();
        fixture.git(["checkout", "-b", "feature/list"]);
        fixture.write("feature.txt", "feature\n");
        fixture.git(["add", "feature.txt"]);
        fixture.git(["commit", "-m", "feature commit"]);

        let branches = branches(fixture.path()).unwrap();
        assert!(branches
            .iter()
            .any(|branch| branch.name == "feature/list" && branch.is_head));
        let log = log(fixture.path(), 2).unwrap();
        assert_eq!(log[0].summary.as_deref(), Some("feature commit"));

        let branch_commits = commits_since(fixture.path(), "main", 10).unwrap();
        assert_eq!(branch_commits[0].summary.as_deref(), Some("feature commit"));
        let changed = changed_paths_since(fixture.path(), "main").unwrap();
        assert_eq!(changed, vec![PathBuf::from("feature.txt")]);
        let branch_diff = diff_since(fixture.path(), "main").unwrap();
        assert!(branch_diff.contains("feature.txt"));
    }

    #[test]
    fn branch_comparison_matches_individual_queries_in_one_pass() {
        let fixture = RepoFixture::new();
        fixture.git(["checkout", "-b", "feature/combined"]);
        fixture.write("feature.txt", "feature\n");
        fixture.git(["add", "feature.txt"]);
        fixture.git(["commit", "-m", "feature commit"]);

        let comparison = branch_comparison(fixture.path(), "main", 10).unwrap();
        assert_eq!(
            comparison.commits[0].summary.as_deref(),
            Some("feature commit")
        );
        assert_eq!(comparison.changed_paths, vec![PathBuf::from("feature.txt")]);
        assert!(comparison.diff.contains("feature.txt"));

        // The combined helper agrees with the per-piece functions.
        assert_eq!(
            comparison.commits,
            commits_since(fixture.path(), "main", 10).unwrap()
        );
        assert_eq!(
            comparison.changed_paths,
            changed_paths_since(fixture.path(), "main").unwrap()
        );
        assert_eq!(comparison.diff, diff_since(fixture.path(), "main").unwrap());
    }

    #[test]
    fn branch_queries_degrade_gracefully_on_unborn_head() {
        // A repo with a committed `main` but a fresh unborn branch checked out:
        // commits/diff "since main" must not error on the missing HEAD.
        let fixture = RepoFixture::new();
        fixture.git(["checkout", "--orphan", "feature/unborn"]);
        fixture.git(["rm", "-rf", "--cached", "."]);

        assert!(commits_since(fixture.path(), "main", 10)
            .unwrap()
            .is_empty());
        assert!(changed_paths_since(fixture.path(), "main")
            .unwrap()
            .is_empty());
        assert!(diff_since(fixture.path(), "main").unwrap().is_empty());

        let comparison = branch_comparison(fixture.path(), "main", 10).unwrap();
        assert!(comparison.commits.is_empty());
        assert!(comparison.changed_paths.is_empty());
        assert!(comparison.diff.is_empty());
    }

    #[test]
    fn commit_subject_with_embedded_newline_stays_single_line() {
        let fixture = RepoFixture::new();
        fixture.write("tracked.txt", "newline subject\n");
        let client = GitClient::default();
        client
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        // An LLM draft can leak a newline into the subject; it must not bleed
        // into the body.
        client
            .commit(
                fixture.path(),
                "feat: do a thing\nthat leaked into a second line",
                Some("- Real body bullet"),
            )
            .unwrap();

        let output = Command::new("git")
            .current_dir(fixture.path())
            .args(["log", "-1", "--pretty=%B"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let message = String::from_utf8(output.stdout).unwrap();
        assert!(
            message.starts_with(
                "feat: do a thing that leaked into a second line\n\n- Real body bullet"
            ),
            "message was {message:?}"
        );
    }

    #[test]
    fn creates_existing_and_new_branch_worktrees_and_rejects_checked_out_branch() {
        let fixture = RepoFixture::new();
        let managed = TempDir::new().unwrap();
        let client = GitClient::default();
        let project_id = ProjectId::new();

        let request = CreateWorktreeRequest {
            project_id,
            project_name: "hitch".into(),
            managed_root: managed.path().into(),
            branch: "feature/new".into(),
            checkout: WorktreeCheckout::NewBranch,
            base: Some("main".into()),
        };
        let worktree = client.create_worktree(fixture.path(), &request).unwrap();
        assert!(worktree.path.exists());
        assert_eq!(worktree.branch, "feature/new");
        assert!(!worktree.is_main);

        let second = CreateWorktreeRequest {
            branch: "main".into(),
            checkout: WorktreeCheckout::ExistingBranch,
            ..request.clone()
        };
        let err = client.create_worktree(fixture.path(), &second).unwrap_err();
        assert!(matches!(err, GitError::CommandFailed { .. }));
    }

    #[test]
    fn discover_worktrees_reports_main_plus_created_linked_worktrees() {
        let fixture = RepoFixture::new();
        let managed = TempDir::new().unwrap();
        let client = GitClient::default();

        // Only the main worktree exists at first.
        let initial = discover_worktrees(fixture.path()).unwrap();
        assert_eq!(initial.len(), 1);
        assert!(initial[0].is_main);
        assert_eq!(
            canonical_or_self(&initial[0].path),
            canonical_or_self(fixture.path())
        );

        // Create a linked worktree on a fresh branch.
        let request = CreateWorktreeRequest {
            project_id: ProjectId::new(),
            project_name: "hitch".into(),
            managed_root: managed.path().into(),
            branch: "feature/discover".into(),
            checkout: WorktreeCheckout::NewBranch,
            base: Some("main".into()),
        };
        let created = client.create_worktree(fixture.path(), &request).unwrap();

        // Discovery now reports both, with the linked worktree's branch.
        let found = discover_worktrees(fixture.path()).unwrap();
        assert_eq!(found.len(), 2, "found {found:?}");
        let linked = found
            .iter()
            .find(|w| !w.is_main)
            .expect("linked worktree discovered");
        assert_eq!(linked.branch, "feature/discover");
        assert_eq!(
            canonical_or_self(&linked.path),
            canonical_or_self(&created.path)
        );
    }

    fn canonical_or_self(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    #[test]
    fn removes_worktree_keeps_branch_by_default_and_deletes_only_merged_branch() {
        let fixture = RepoFixture::new();
        let managed = TempDir::new().unwrap();
        let client = GitClient::default();
        let request = CreateWorktreeRequest {
            project_id: ProjectId::new(),
            project_name: "hitch".into(),
            managed_root: managed.path().into(),
            branch: "feature/remove".into(),
            checkout: WorktreeCheckout::NewBranch,
            base: Some("main".into()),
        };
        let worktree = client.create_worktree(fixture.path(), &request).unwrap();

        client
            .remove_worktree(
                fixture.path(),
                &RemoveWorktreeRequest {
                    path: worktree.path.clone(),
                    force: false,
                    delete_branch: None,
                },
            )
            .unwrap();
        assert!(!worktree.path.exists());
        assert!(branches(fixture.path())
            .unwrap()
            .iter()
            .any(|branch| branch.name == "feature/remove"));

        let existing_request = CreateWorktreeRequest {
            checkout: WorktreeCheckout::ExistingBranch,
            ..request
        };
        let worktree = client
            .create_worktree(fixture.path(), &existing_request)
            .unwrap();
        client
            .remove_worktree(
                fixture.path(),
                &RemoveWorktreeRequest {
                    path: worktree.path.clone(),
                    force: false,
                    delete_branch: Some("feature/remove".into()),
                },
            )
            .unwrap();
        assert!(!branches(fixture.path())
            .unwrap()
            .iter()
            .any(|branch| branch.name == "feature/remove"));
    }

    #[test]
    fn removing_worktree_already_removed_externally_is_idempotent() {
        let fixture = RepoFixture::new();
        let managed = TempDir::new().unwrap();
        let client = GitClient::default();
        let request = CreateWorktreeRequest {
            project_id: ProjectId::new(),
            project_name: "hitch".into(),
            managed_root: managed.path().into(),
            branch: "feature/external-remove".into(),
            checkout: WorktreeCheckout::NewBranch,
            base: Some("main".into()),
        };
        let worktree = client.create_worktree(fixture.path(), &request).unwrap();

        run_real_git(
            fixture.path(),
            [
                "worktree",
                "remove",
                "--force",
                worktree.path.to_str().unwrap(),
            ],
        );
        assert!(!worktree.path.exists());

        client
            .remove_worktree(
                fixture.path(),
                &RemoveWorktreeRequest {
                    path: worktree.path,
                    force: true,
                    delete_branch: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn refuses_to_delete_unmerged_branch_after_worktree_remove() {
        let fixture = RepoFixture::new();
        let managed = TempDir::new().unwrap();
        let client = GitClient::default();
        let request = CreateWorktreeRequest {
            project_id: ProjectId::new(),
            project_name: "hitch".into(),
            managed_root: managed.path().into(),
            branch: "feature/unmerged".into(),
            checkout: WorktreeCheckout::NewBranch,
            base: Some("main".into()),
        };
        let worktree = client.create_worktree(fixture.path(), &request).unwrap();
        fs::write(worktree.path.join("unmerged.txt"), "unmerged\n").unwrap();
        run_real_git(&worktree.path, ["add", "unmerged.txt"]);
        run_real_git(&worktree.path, ["commit", "-m", "unmerged commit"]);

        let worktree_path = worktree.path.clone();
        let err = client
            .remove_worktree(
                fixture.path(),
                &RemoveWorktreeRequest {
                    path: worktree.path,
                    force: false,
                    delete_branch: Some("feature/unmerged".into()),
                },
            )
            .unwrap_err();
        assert!(matches!(err, GitError::BranchNotMerged { .. }));
        assert!(worktree_path.exists());
    }

    #[test]
    fn create_pr_pushes_first_when_needed_and_invokes_gh_with_default_base() {
        let fixture = RepoFixture::new();
        let bin = TempDir::new().unwrap();
        let log = bin.path().join("calls.log");
        let git = fake_program(bin.path().join("git"), &log, "");
        let gh = fake_program(bin.path().join("gh"), &log, "https://github.test/pr/1\n");
        let client = GitClient::with_programs(git, gh);

        let url = client
            .create_pr(
                fixture.path(),
                &CreatePrRequest {
                    title: "Ship it".into(),
                    body: Some("Body".into()),
                    base: None,
                    head: None,
                    remote: None,
                    draft: true,
                },
            )
            .unwrap();

        assert_eq!(url, "https://github.test/pr/1");
        let calls = fs::read_to_string(log).unwrap();
        let mut lines = calls.lines();
        assert_eq!(lines.next().unwrap().trim_end(), "git push -u origin main");
        let gh_call = lines.next().unwrap();
        assert!(gh_call.contains("gh pr create"));
        assert!(gh_call.contains("--base main"));
        assert!(gh_call.contains("--head main"));
        assert!(gh_call.contains("--draft"));
    }

    #[test]
    fn managed_path_sanitizes_project_and_branch_components() {
        assert_eq!(
            managed_worktree_path("/tmp/root", "my/project", "feature/test")
                .strip_prefix("/tmp/root")
                .unwrap(),
            Path::new("my_project/feature_test")
        );
    }

    struct RepoFixture {
        _temp: TempDir,
        path: PathBuf,
    }

    impl RepoFixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("repo");
            fs::create_dir(&path).unwrap();
            run_real_git(&path, ["init", "--initial-branch=main"]);
            run_real_git(&path, ["config", "user.name", "Hitch Test"]);
            run_real_git(&path, ["config", "user.email", "hitch@example.test"]);
            fs::write(path.join("tracked.txt"), "initial\n").unwrap();
            run_real_git(&path, ["add", "tracked.txt"]);
            run_real_git(&path, ["commit", "-m", "initial commit"]);
            Self { _temp: temp, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn write(&self, relative: &str, contents: &str) {
            fs::write(self.path.join(relative), contents).unwrap();
        }

        fn git<const N: usize>(&self, args: [&str; N]) {
            run_real_git(&self.path, args);
        }
    }

    fn run_real_git<const N: usize>(cwd: &Path, args: [&str; N]) {
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

    fn fake_program(path: PathBuf, log: &Path, stdout: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\nprintf '%s ' \"$(basename $0)\" >> {log}\nfor arg in \"$@\"; do printf '%s ' \"$arg\" >> {log}; done\nprintf '\\n' >> {log}\nprintf '{stdout}'\n",
            log = shell_quote(log),
            stdout = stdout.replace('\\', "\\\\").replace('\'', "'\\''")
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    struct TestControl {
        cancelled: AtomicBool,
        pgids: Mutex<Vec<Option<i32>>>,
        cancel_on_clear: bool,
    }

    impl TestControl {
        fn new(cancel_on_clear: bool) -> Self {
            Self {
                cancelled: AtomicBool::new(false),
                pgids: Mutex::new(Vec::new()),
                cancel_on_clear,
            }
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    impl CommandControl for TestControl {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }

        fn set_child_pgid(&self, pgid: Option<i32>) {
            self.pgids.lock().unwrap().push(pgid);
            if self.cancel_on_clear && pgid.is_none() {
                self.cancel();
            }
        }
    }

    #[test]
    fn run_command_keeps_success_when_cancellation_arrives_after_exit() {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-success");
        fs::write(&script, "#!/bin/sh\nprintf 'done'\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let control = TestControl::new(true);
        let output = run_command(&script, temp.path(), Vec::new(), Some(&control)).unwrap();

        assert_eq!(output.stdout, "done");
        assert_eq!(output.stderr, "");
        assert!(
            control.is_cancelled(),
            "test precondition: cancel should flip after exit"
        );
        let pgids = control.pgids.lock().unwrap();
        assert!(pgids.first().is_some_and(|pgid| pgid.is_some()));
        assert_eq!(pgids.last().copied(), Some(None));
    }

    #[cfg(unix)]
    #[test]
    fn run_command_reports_mid_flight_cancellation() {
        use std::time::Duration;

        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-sleep");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let control = std::sync::Arc::new(TestControl::new(false));
        let cancel = std::sync::Arc::clone(&control);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });

        let err =
            run_command(&script, temp.path(), Vec::new(), Some(control.as_ref())).unwrap_err();
        trigger.join().unwrap();

        match err {
            GitError::CommandFailed { stderr, .. } => assert!(stderr.contains("command cancelled")),
            other => panic!("unexpected error: {other:?}"),
        }
        let pgids = control.pgids.lock().unwrap();
        assert!(pgids.first().is_some_and(|pgid| pgid.is_some()));
        assert_eq!(pgids.last().copied(), Some(None));
    }

    #[cfg(unix)]
    #[test]
    fn controlled_clone_kills_the_child_when_cancelled() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-sleep");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let client = GitClient::with_programs(&script, &script);
        let control = Arc::new(TestControl::new(false));
        let cancel = Arc::clone(&control);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let started = Instant::now();
        let err = client
            .clone_repo_with_control(
                "https://example.com/repo.git",
                temp.path().join("target"),
                control.as_ref(),
            )
            .unwrap_err();
        trigger.join().unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation should not wait for the full child runtime"
        );
        match err {
            GitError::CommandFailed { stderr, .. } => {
                assert!(stderr.contains("command cancelled"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let pgids = control.pgids.lock().unwrap();
        assert!(pgids.first().is_some_and(|pgid| pgid.is_some()));
        assert_eq!(pgids.last().copied(), Some(None));
    }

    #[allow(dead_code)]
    fn unique_name(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }
}
