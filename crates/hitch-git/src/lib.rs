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

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{BranchType, DiffFormat, DiffOptions, Oid, Repository, Status, StatusOptions};
use hitch_core::{ProjectId, Worktree};

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

    /// Best-effort default branch detection: `origin/HEAD`, then current branch.
    pub fn default_branch(&self) -> Result<String> {
        default_branch(&self.root)
    }

    /// Return the branch currently checked out in this worktree, including unborn branches.
    pub fn current_branch(&self) -> Result<String> {
        current_branch(&self.root)
    }
}

/// Paths and executables used for write-side commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitClient {
    git: PathBuf,
    gh: PathBuf,
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

    /// Commit staged changes using the system git executable.
    pub fn commit(&self, repo_path: impl AsRef<Path>, message: &str) -> Result<CommandOutput> {
        self.run_git(
            repo_path.as_ref(),
            vec![os("commit"), os("-m"), os(message)],
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

    /// Fetch from a remote using the system git executable.
    pub fn fetch(&self, repo_path: impl AsRef<Path>, remote: &str) -> Result<CommandOutput> {
        self.run_git(repo_path.as_ref(), vec![os("fetch"), os(remote)])
    }

    /// Create a Hitch-managed worktree under `managed_root/<project>/<branch>`.
    pub fn create_worktree(
        &self,
        repo_path: impl AsRef<Path>,
        request: &CreateWorktreeRequest,
    ) -> Result<Worktree> {
        create_worktree_with_client(self, repo_path.as_ref(), request)
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
        create_pr_with_client(self, repo_path.as_ref(), request)
    }

    fn run_git(&self, cwd: &Path, args: Vec<OsString>) -> Result<CommandOutput> {
        run_command(&self.git, cwd, args)
    }

    fn run_gh(&self, cwd: &Path, args: Vec<OsString>) -> Result<CommandOutput> {
        run_command(&self.gh, cwd, args)
    }
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
    Ok(String::from_utf8(out)?)
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

    client.run_git(repo_path, args)?;
    Ok(Worktree::new(
        request.project_id,
        target,
        request.branch.clone(),
        false,
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
    let output = client.run_git(repo_path, args)?;

    if let Some(branch) = &request.delete_branch {
        client.run_git(repo_path, vec![os("branch"), os("-d"), os(branch)])?;
    }

    Ok(output)
}

fn create_pr_with_client(
    client: &GitClient,
    repo_path: &Path,
    request: &CreatePrRequest,
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
        client.push(repo_path, remote, &branch, true)?;
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

    let output = client.run_gh(repo_path, args)?;
    Ok(output.stdout.trim().to_string())
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

fn run_command(program: &Path, cwd: &Path, args: Vec<OsString>) -> Result<CommandOutput> {
    let output = Command::new(program)
        .current_dir(cwd)
        .args(&args)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    if output.status.success() {
        Ok(CommandOutput { stdout, stderr })
    } else {
        Err(GitError::CommandFailed {
            program: program.to_string_lossy().into_owned(),
            args: args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            cwd: cwd.to_path_buf(),
            code: output.status.code(),
            stdout: stdout.into_boxed_str(),
            stderr: stderr.into_boxed_str(),
        })
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
        client.commit(fixture.path(), "update tracked").unwrap();
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
        client.commit(fixture.path(), "hooked commit").unwrap();

        assert_eq!(fs::read_to_string(marker).unwrap(), "hook\n");
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

    #[allow(dead_code)]
    fn unique_name(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }
}
