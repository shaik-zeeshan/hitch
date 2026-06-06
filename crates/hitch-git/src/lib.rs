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

use git2::{
    BranchType, DiffFindOptions, DiffFormat, DiffOptions, Oid, Repository, Status, StatusOptions,
};
use hitch_core::{ProjectId, Worktree};
use hitch_process::{DrainOutcome, PipeReader, ProcessTree, ProcessTreeRegistration};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::Write;
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
    pub fn diff_file(
        &self,
        path: impl AsRef<Path>,
        target: DiffTarget,
        options: DiffFileOptions,
    ) -> Result<String> {
        diff_file(&self.root, path, target, options)
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
/// The daemon's Job registry implements this so `ShutdownDaemon`/`CancelJob`
/// can kill the subprocess tree and wait for the worker to drain before exit.
pub trait CommandControl {
    fn is_cancelled(&self) -> bool;
    fn set_process_tree(&self, tree: Option<ProcessTree>);
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

    /// Fetch from a remote as a cancellable child process.
    pub fn fetch_with_control(
        &self,
        repo_path: impl AsRef<Path>,
        remote: &str,
        control: &dyn CommandControl,
    ) -> Result<CommandOutput> {
        self.run_git_with_control(repo_path.as_ref(), vec![os("fetch"), os(remote)], control)
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

/// Split branch heads into one or more `gh pr list --search` queries. GitHub
/// rejects a search with more than five boolean operators or longer than 256
/// characters
/// (https://docs.github.com/en/rest/search/search#limitations-on-query-length),
/// and `pr_list_for_branches_with_client` maps any `gh` failure to an empty
/// result — so a single `(head:b1 OR … OR b7)` query for a project with seven or
/// more worktree branches (six `OR`s) would silently drop *every* chip. Capping
/// each query at six heads / a conservative length keeps larger projects working
/// by spreading the lookup across several `gh` calls.
fn pr_branch_search_chunks(branches: &[String]) -> Vec<String> {
    // Five `OR`s join six heads — the documented operator ceiling. Stay well
    // under the 256-char query cap on the joined branch names too.
    const MAX_HEADS_PER_QUERY: usize = 6;
    const MAX_SEARCH_LEN: usize = 200;

    let mut chunks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_len = 0usize;
    for branch in branches.iter().filter(|branch| !branch.is_empty()) {
        // `head:` qualifier + the name + a ` OR ` separator as headroom.
        let cost = branch.len() + 9;
        if !current.is_empty()
            && (current.len() == MAX_HEADS_PER_QUERY || current_len + cost > MAX_SEARCH_LEN)
        {
            chunks.push(pr_branch_search(&current));
            current.clear();
            current_len = 0;
        }
        current.push(branch.clone());
        current_len += cost;
    }
    if !current.is_empty() {
        chunks.push(pr_branch_search(&current));
    }
    chunks
}

/// Owner (`login`) of the `origin` remote, parsed from its URL. Used to discard
/// fork PRs that merely share a head branch name with a local worktree (see
/// `pr_list_for_branches_with_client`). Best-effort: `None` when there is no
/// `origin`, no URL, or an unrecognised URL shape — callers then keep every
/// name-matched PR rather than risk dropping legitimate chips.
fn origin_owner(repo_path: &Path) -> Option<String> {
    let repo = Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    parse_remote_owner(remote.url()?)
}

/// Extract the owner segment from a git remote URL. Handles the common GitHub
/// shapes: `git@github.com:owner/repo(.git)`, `https://github.com/owner/repo(.git)`,
/// and `ssh://git@github.com[:port]/owner/repo(.git)`.
fn parse_remote_owner(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let path = if let Some((_, rest)) = trimmed.split_once("://") {
        // URL form: scheme://[user@]host[:port]/owner/repo — owner follows the
        // first '/' after the host.
        rest.split_once('/').map(|(_, path)| path)?
    } else if let Some((_, rest)) = trimmed.split_once(':') {
        // scp form: [user@]host:owner/repo.
        rest
    } else {
        return None;
    };
    let owner = path.trim_start_matches('/').split('/').next()?;
    (!owner.is_empty()).then(|| owner.to_string())
}

fn pr_list_for_branches_with_client(
    client: &GitClient,
    repo_path: &Path,
    branches: &[String],
    control: Option<&dyn CommandControl>,
) -> Result<Vec<(String, PrInfo)>> {
    let searches = pr_branch_search_chunks(branches);
    // No real branches to look up — an empty query would match every PR, which
    // we don't want, so bail before spending a `gh` call.
    if searches.is_empty() {
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
    // For repos that accept fork PRs, `head:branch` also matches a contributor's
    // fork PR whose head branch happens to share a local worktree's name. Anchor
    // on the project's own `origin` owner so a fork PR is never mapped onto (and
    // opened from) the wrong local branch. Best-effort: an unknown owner means we
    // keep every name match rather than drop legitimate chips.
    let expected_owner = origin_owner(repo_path);
    let mut prs = Vec::new();
    // Heads are spread across several queries to stay under GitHub's search
    // limits (see `pr_branch_search_chunks`); a failed chunk is skipped rather
    // than abandoning the rest, so one odd/over-long branch can't drop every chip.
    for search in &searches {
        // `--state all` so a worktree whose PR has already merged still shows a
        // chip; `headRefName` is the branch we map back to each worktree.
        let args = vec![
            os("pr"),
            os("list"),
            os("--state"),
            os("all"),
            os("--search"),
            os(search),
            os("--limit"),
            os(&limit),
            os("--json"),
            os("number,url,state,isDraft,headRefName,headRepositoryOwner"),
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
                // No repo / no auth / no network — nothing to show, like
                // pr_status. Skip this chunk rather than abandon the rest.
                continue;
            }
            Err(err) => return Err(err),
        };
        let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(&output.stdout) else {
            continue;
        };
        for value in values {
            let (Some(number), Some(url), Some(head)) = (
                value["number"].as_u64(),
                value["url"].as_str(),
                value["headRefName"].as_str(),
            ) else {
                continue;
            };
            // `head:` is a prefix match, so the OR'd query can return PRs on
            // branches that only start with a requested one. Keep exact matches
            // only, so a prefix hit is never mapped onto the wrong worktree.
            if !branches.iter().any(|branch| branch.as_str() == head) {
                continue;
            }
            // Drop fork PRs that share a head branch name with a local worktree:
            // keep only PRs whose head repo owner matches `origin`. When we can't
            // resolve our owner, or gh omits the head owner, fail open and keep it.
            if let (Some(expected), Some(head_owner)) = (
                expected_owner.as_deref(),
                value["headRepositoryOwner"]["login"].as_str(),
            ) {
                if !head_owner.eq_ignore_ascii_case(expected) {
                    continue;
                }
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
    /// Added/deleted line counts on the staged side (HEAD↔index) for this path.
    pub staged_additions: usize,
    pub staged_deletions: usize,
    /// Added/deleted line counts on the worktree side (index↔worktree).
    pub worktree_additions: usize,
    pub worktree_deletions: usize,
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

/// View knobs for a file-level diff. The [`Default`] keeps libgit2's defaults
/// (whitespace-sensitive, three context lines), matching the legacy behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffFileOptions {
    /// Collapse whitespace-only changes (`DiffOptions::ignore_whitespace`).
    pub ignore_whitespace: bool,
    /// Override the surrounding context size. `None` keeps git's default (3).
    pub context_lines: Option<u32>,
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
            staged_additions: 0,
            staged_deletions: 0,
            worktree_additions: 0,
            worktree_deletions: 0,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let line_stats = diff_line_stats(&repo)?;
    for entry in &mut entries {
        if let Some((additions, deletions)) = line_stats.staged.get(entry.path.as_path()) {
            entry.staged_additions = *additions;
            entry.staged_deletions = *deletions;
        }
        if let Some((additions, deletions)) = line_stats.worktree.get(entry.path.as_path()) {
            entry.worktree_additions = *additions;
            entry.worktree_deletions = *deletions;
        }
    }
    let additions = line_stats.additions;
    let deletions = line_stats.deletions;
    Ok(StatusSummary {
        dirty: !entries.is_empty(),
        entries,
        additions,
        deletions,
    })
}

/// Aggregate and per-path add/delete line counts from one status pass. The
/// per-path maps are keyed by the new-side path (the path libgit2 status
/// reports), so a rename's counts land on the entry the Changes panel shows.
/// Staged and worktree sides are kept apart so a partially-staged file's row
/// can show the counts for the side it represents.
struct DiffLineStats {
    additions: usize,
    deletions: usize,
    staged: HashMap<PathBuf, (usize, usize)>,
    worktree: HashMap<PathBuf, (usize, usize)>,
}

fn diff_line_stats(repo: &Repository) -> Result<DiffLineStats> {
    let mut stats = DiffLineStats {
        additions: 0,
        deletions: 0,
        staged: HashMap::new(),
        worktree: HashMap::new(),
    };

    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let index = repo.index()?;
    let mut staged = repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), None)?;
    detect_diff_renames(&mut staged)?;
    let mut staged_per_path = HashMap::new();
    let (sa, sd) = accumulate_diff_line_stats(&staged, &mut staged_per_path)?;
    stats.additions += sa;
    stats.deletions += sd;
    stats.staged = staged_per_path;

    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false)
        .show_untracked_content(true);
    let index = repo.index()?;
    let mut worktree = repo.diff_index_to_workdir(Some(&index), Some(&mut options))?;
    detect_diff_renames(&mut worktree)?;
    let mut worktree_per_path = HashMap::new();
    let (wa, wd) = accumulate_diff_line_stats(&worktree, &mut worktree_per_path)?;
    stats.additions += wa;
    stats.deletions += wd;
    stats.worktree = worktree_per_path;

    Ok(stats)
}

fn detect_diff_renames(diff: &mut git2::Diff<'_>) -> Result<()> {
    let mut options = DiffFindOptions::new();
    options.renames(true).for_untracked(true);
    diff.find_similar(Some(&mut options))?;
    Ok(())
}

/// Fold one libgit2 [`Diff`] into a per-path stats map and return its aggregate
/// (additions, deletions). Per-delta counts come from each [`git2::Patch`]'s
/// `line_stats` so the same diff the aggregate would sum also yields the
/// per-file numbers — no extra git work. Binary deltas have no patch and
/// contribute nothing (matching the frontend `parseDiff`, which shows binary
/// files with no +/− counts).
fn accumulate_diff_line_stats(
    diff: &git2::Diff<'_>,
    per_path: &mut HashMap<PathBuf, (usize, usize)>,
) -> Result<(usize, usize)> {
    let mut total_additions = 0;
    let mut total_deletions = 0;
    for (idx, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(Path::to_path_buf);
        let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
            continue;
        };
        // `line_stats` is (context, additions, deletions).
        let (_context, additions, deletions) = patch.line_stats()?;
        total_additions += additions;
        total_deletions += deletions;
        if let Some(path) = path {
            let entry = per_path.entry(path).or_insert((0, 0));
            entry.0 += additions;
            entry.1 += deletions;
        }
    }
    Ok((total_additions, total_deletions))
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

/// Read a file-level diff using libgit2. `view` carries the optional
/// whitespace/context knobs; its [`Default`] reproduces the legacy behavior.
pub fn diff_file(
    repo_path: impl AsRef<Path>,
    path: impl AsRef<Path>,
    target: DiffTarget,
    view: DiffFileOptions,
) -> Result<String> {
    let repo = Repository::discover(repo_path.as_ref())?;
    let pathspec = diff_pathspec(&repo, path.as_ref());
    let mut options = DiffOptions::new();
    options
        .pathspec(pathspec.as_ref())
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if view.ignore_whitespace {
        options.ignore_whitespace(true);
    }
    if let Some(context_lines) = view.context_lines {
        options.context_lines(context_lines);
    }
    if let Some(old_path) = diff_rename_old_path(&repo, pathspec.as_ref(), target)? {
        options.pathspec(old_path);
    }

    let mut diff = match target {
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
    detect_diff_renames(&mut diff)?;

    diff_to_string(&diff)
}

fn diff_rename_old_path(
    repo: &Repository,
    path: &Path,
    target: DiffTarget,
) -> Result<Option<PathBuf>> {
    // libgit2 only pairs a rename when both the new file's add and the old
    // file's delete survive the status walk, so a pathspec scoped to just the
    // new path hides the delete and the rename is never detected. Most renames
    // keep the file in the same directory, so scope the (otherwise full-repo)
    // walk to the new path plus its parent directory — the old side rides along
    // and the walk stays bounded. Cross-directory renames fall through to an
    // unbounded walk below, preserving the previous behaviour exactly.
    match diff_rename_old_path_scoped(repo, path, target, true)? {
        RenameLookup::Found(old) => Ok(old),
        // The file shows up modified/typechanged (its old and new side share a
        // path), so it cannot be the new side of a rename — no fallback needed.
        RenameLookup::PresentNonRename => Ok(None),
        // The scoped walk either didn't see the file, or saw it as a bare add
        // whose matching delete may live outside the parent directory (a
        // cross-directory rename). Redo the walk over the whole repo, exactly
        // as before this optimisation, to settle it.
        RenameLookup::AddedOrAbsent => match diff_rename_old_path_scoped(repo, path, target, false)?
        {
            RenameLookup::Found(old) => Ok(old),
            _ => Ok(None),
        },
    }
}

/// Outcome of a single (possibly path-scoped) status walk while resolving a
/// file's rename source.
enum RenameLookup {
    /// The file is the new side of a detected rename; carries the old path.
    Found(Option<PathBuf>),
    /// The file appeared with a single (modified/typechange/delete) status
    /// whose old and new paths match — it cannot be a rename target, so a
    /// scoped walk that finds this needs no full-repo fallback.
    PresentNonRename,
    /// The file is absent from this walk, or present only as a bare add whose
    /// rename partner (the old path's delete) might lie outside the scope.
    AddedOrAbsent,
}

fn diff_rename_old_path_scoped(
    repo: &Repository,
    path: &Path,
    target: DiffTarget,
    scope_to_parent: bool,
) -> Result<RenameLookup> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    if scope_to_parent {
        options.pathspec(path.to_string_lossy().into_owned());
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                options.pathspec(format!("{}/*", parent.to_string_lossy()));
            }
            // File at the repo root: a bare `*` keeps the walk to root-level
            // entries without recursing into subdirectories.
            _ => {
                options.pathspec("*");
            }
        }
    }
    let statuses = repo.statuses(Some(&mut options))?;
    let mut result = RenameLookup::AddedOrAbsent;
    for entry in statuses.iter() {
        let status = entry.status();
        let Some((new_path, old_path)) = status_paths(entry) else {
            continue;
        };
        if new_path != path {
            continue;
        }
        let renamed = match target {
            DiffTarget::Staged => status.contains(Status::INDEX_RENAMED),
            DiffTarget::Worktree => status.contains(Status::WT_RENAMED),
        };
        if renamed {
            return Ok(RenameLookup::Found(old_path));
        }
        // Present, not a rename. A bare add (new file, no old side) could still
        // be the surviving half of a cross-dir rename whose delete the scope
        // dropped, so only treat clearly non-add states as terminal.
        let added = match target {
            DiffTarget::Staged => status.contains(Status::INDEX_NEW),
            DiffTarget::Worktree => status.contains(Status::WT_NEW),
        };
        if !added {
            result = RenameLookup::PresentNonRename;
        }
    }
    Ok(result)
}

fn diff_pathspec<'a>(repo: &Repository, path: &'a Path) -> Cow<'a, Path> {
    if !path.is_absolute() {
        return Cow::Borrowed(path);
    }

    let Some(workdir) = repo.workdir() else {
        return Cow::Borrowed(path);
    };
    if let Ok(relative) = path.strip_prefix(workdir) {
        return Cow::Owned(relative.to_path_buf());
    }

    // On macOS, temp dirs often cross the /var → /private/var symlink boundary:
    // callers hand us /var/..., while libgit2 reports the workdir as /private/var/....
    // Fall back to physical paths for existing files so absolute pathspecs still
    // become repository-relative.
    if let (Ok(real_path), Ok(real_workdir)) = (fs::canonicalize(path), fs::canonicalize(workdir)) {
        if let Ok(relative) = real_path.strip_prefix(real_workdir) {
            return Cow::Owned(relative.to_path_buf());
        }
    }

    Cow::Borrowed(path)
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
        // Skip linked worktrees whose directory is gone (prunable but not
        // pruned). Probe via the extended-length form so a deep managed-worktree
        // path isn't wrongly treated as missing under MAX_PATH on Windows. The
        // normal `path` is what we store and hand back to callers/GUI.
        if !path_is_dir_fs(&path) {
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
        // Filesystem access goes through the extended-length (`\\?\`) form on
        // Windows so a deep managed-worktree root can't trip MAX_PATH; the
        // normal `target` is still what's handed to the git CLI and returned to
        // callers below (ADR 0012).
        fs::create_dir_all(fs_path(parent)?)?;
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
    // Probe existence through the extended-length form on Windows: a managed
    // worktree path long enough to trip MAX_PATH must not be misread as "already
    // gone" and silently swallow a real removal failure.
    !path_exists_fs(path)
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

/// Grace period for draining a command's stdout/stderr reader on the normal-exit
/// path before detaching a reader still parked on an inherited pipe. The command
/// already exited, so its own pipes are at EOF and the readers finish well within
/// this window; the bound only caps the wait when a same-group descendant kept a
/// captured write end open (see `drain_pipe_reader_bounded`). Kept short and on
/// the same order as the `try_wait` poll interval so a completed run returns
/// promptly.
const READER_DRAIN_GRACE: Duration = Duration::from_millis(500);

fn run_command(
    program: &Path,
    cwd: &Path,
    args: Vec<OsString>,
    control: Option<&dyn CommandControl>,
) -> Result<CommandOutput> {
    let Some(control) = control else {
        let mut command = Command::new(program);
        command.current_dir(cwd).args(&args);
        // Match the cancellable path's windowless behaviour: the control-less
        // branch never reaches `ProcessTree::spawn`, so without this a
        // console-attached caller (CLI, tests, a stale shim) would flash a console
        // window for each git invocation. See `hitch_process::configure_windowless`.
        hitch_process::configure_windowless(&mut command);
        let output = command.output()?;
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
    let (mut child, tree) = ProcessTree::spawn(&mut command)?;
    let registration = ProcessTreeRegistration::new(
        || control.set_process_tree(Some(tree.clone())),
        || control.set_process_tree(None),
    );
    let stdout_reader = PipeReader::spawn(
        child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture command stdout"))?,
    );
    let stderr_reader = PipeReader::spawn(
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
                let _ = tree.terminate();
                let _ = child.kill();
                break child.wait()?;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    };
    // Disarm the external canceller before draining the pipe readers. Once the
    // child (the process-group leader) is reaped, the registered tree's
    // `terminate()` is no longer safe to call from a concurrent cancel: on Unix
    // it is `kill(-pgid)`, and the leader's pgid can be recycled to an unrelated
    // group as soon as the group empties. The cancel-before-exit branch above
    // already terminated the tree itself while the group was alive, so clearing
    // the registration here loses no intended kill — it only closes the window
    // where a late cancel could signal a recycled process group.
    drop(registration);

    let (stdout_result, stderr_result) = if cancelled {
        // The cancel branch group-killed the tree while the leader was still
        // alive, so the captured pipes get EOF and the readers finish on their
        // own. Join them unconditionally to collect the partial output.
        (stdout_reader.join(), stderr_reader.join())
    } else {
        // NORMAL-EXIT path: the child exited on its own. If a same-group
        // descendant inherited a captured write end and outlives it, the readers
        // never see EOF, and once the leader is reaped we cannot group-kill that
        // descendant on Unix to force it (the recycled-pgid hazard —
        // `terminate_after_leader_reaped` is a deliberate no-op there; on Windows
        // the owned Job Object handle still tears down the descendant and closes
        // the pipe). Use the post-reap-safe teardown, then drain with a bounded
        // wait so a slightly-late EOF still lands while a truly stuck reader is
        // detached with whatever the command already wrote.
        if !stdout_reader.is_finished() || !stderr_reader.is_finished() {
            let _ = tree.terminate_after_leader_reaped();
            let _ = child.kill();
        }
        (
            drain_pipe_reader_bounded(stdout_reader, READER_DRAIN_GRACE),
            drain_pipe_reader_bounded(stderr_reader, READER_DRAIN_GRACE),
        )
    };

    let stdout = String::from_utf8(stdout_result?)?;
    let stderr = String::from_utf8(stderr_result?)?;
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

/// Drain a command stdout/stderr reader on the NORMAL-EXIT path with a bounded
/// wait, collapsing both the drained and timed-out outcomes to the bytes
/// collected. `run_command` treats a slightly-late or stuck reader identically —
/// it uses whatever the command already wrote — so the distinction
/// [`DrainOutcome`] preserves for the daemon's success path is not meaningful
/// here; see [`PipeReader::drain_bounded`] for the shared chunk-loop/grace logic.
fn drain_pipe_reader_bounded(reader: PipeReader, grace: Duration) -> std::io::Result<Vec<u8>> {
    reader.drain_bounded(grace).map(DrainOutcome::into_inner)
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

const MAX_SAFE_PATH_COMPONENT_PREFIX_CHARS: usize = 96;

fn safe_path_component(value: &str) -> String {
    let mut sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();

    let trimmed = sanitized.trim_matches(['.', ' ']);
    if trimmed.is_empty() {
        sanitized.clear();
        sanitized.push('_');
    } else if trimmed.len() != sanitized.len() {
        sanitized = trimmed.to_string();
    }

    if sanitized.chars().count() > MAX_SAFE_PATH_COMPONENT_PREFIX_CHARS {
        sanitized = sanitized
            .chars()
            .take(MAX_SAFE_PATH_COMPONENT_PREFIX_CHARS)
            .collect();
        let trimmed = sanitized.trim_matches(['.', ' ']);
        if trimmed.is_empty() {
            sanitized.clear();
            sanitized.push('_');
        } else if trimmed.len() != sanitized.len() {
            sanitized = trimmed.to_string();
        }
    }

    if is_windows_reserved_device_name(&sanitized) {
        sanitized.insert(0, '_');
    }

    format!("{sanitized}-{:016x}", stable_path_hash(value))
}

fn is_windows_reserved_device_name(value: &str) -> bool {
    let stem = value
        .split_once('.')
        .map_or(value, |(stem, _)| stem)
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Stable, collision-resistant suffix for a sanitized worktree directory name.
///
/// Hashes the *original* (pre-sanitization) value so two branches that sanitize
/// to the same on-disk component still land in distinct directories. Uses the
/// shared leaf-crate FNV-1a so the suffix stays byte-for-byte stable across
/// builds — existing managed worktrees must keep resolving to the same path.
fn stable_path_hash(value: &str) -> u64 {
    hitch_core::fnv1a_64(value.as_bytes())
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

fn path_os(path: impl AsRef<Path>) -> OsString {
    path.as_ref().as_os_str().to_os_string()
}

/// Return the path to hand to std::fs / libgit2 for managed-worktree lifecycle
/// filesystem access.
///
/// On Windows the std `CreateFileW`-backed APIs are still subject to the legacy
/// MAX_PATH (260) limit unless the path is given in extended-length (`\\?\`)
/// form. Managed worktrees live deep under `%LOCALAPPDATA%\Hitch\worktrees\…`
/// (ADR 0012), so a long project/branch can push a checkout past 260 even though
/// each component is already capped (ADR 0001). Prefixing the *filesystem* path
/// with `\\?\` opts that single call out of MAX_PATH regardless of the OS
/// `LongPathsEnabled` policy. The normal form is kept everywhere else — git CLI
/// arguments, display strings, and paths stored / sent to the GUI — so only the
/// raw filesystem syscall sees the prefix.
///
/// Returns a clear error (rather than a silently truncated path) when the
/// absolute path still can't be represented in extended-length form.
#[cfg(windows)]
fn fs_path(path: &Path) -> Result<Cow<'_, Path>> {
    extended_length_path(path)
}

/// On non-Windows targets there is no MAX_PATH and no `\\?\` form; the path is
/// used verbatim.
#[cfg(not(windows))]
fn fs_path(path: &Path) -> Result<Cow<'_, Path>> {
    Ok(Cow::Borrowed(path))
}

/// `Path::exists` but routed through [`fs_path`] so a managed-worktree path past
/// MAX_PATH is probed correctly on Windows. Falls back to the normal form if the
/// extended-length conversion isn't possible (e.g. a relative path), so the
/// answer is never worse than `Path::exists`.
fn path_exists_fs(path: &Path) -> bool {
    match fs_path(path) {
        Ok(probe) => probe.exists(),
        Err(_) => path.exists(),
    }
}

/// `Path::is_dir` routed through [`fs_path`] (see [`path_exists_fs`]).
fn path_is_dir_fs(path: &Path) -> bool {
    match fs_path(path) {
        Ok(probe) => probe.is_dir(),
        Err(_) => path.is_dir(),
    }
}

/// Convert an absolute Windows path to its extended-length (`\\?\`) form,
/// mapping UNC paths (`\\server\share\…`) to `\\?\UNC\server\share\…`.
///
/// Paths already in extended-length form (e.g. anything that came back from
/// `std::fs::canonicalize`, which returns `\\?\` paths on Windows) are returned
/// unchanged. A relative path can't be prefixed safely (`\\?\` disables the
/// `.`/`..` and drive-relative resolution that would be needed), so callers must
/// pass an absolute path; a non-absolute or otherwise non-representable path is
/// reported as a clear error instead of being truncated.
#[cfg(windows)]
fn extended_length_path(path: &Path) -> Result<Cow<'_, Path>> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    match components.next() {
        // Already `\\?\…` (verbatim) — including what canonicalize() returns.
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::VerbatimDisk(_) | Prefix::Verbatim(_) | Prefix::VerbatimUNC(_, _)
            ) =>
        {
            Ok(Cow::Borrowed(path))
        }
        // `C:\…` → `\\?\C:\…`. The `\\?\` namespace does *not* accept forward
        // slashes (it skips path normalization), and libgit2 hands back
        // forward-slash paths on Windows, so the separators are normalized to
        // backslashes first.
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {
            let raw = path.as_os_str().to_string_lossy().replace('/', "\\");
            Ok(Cow::Owned(PathBuf::from(format!(r"\\?\{raw}"))))
        }
        // `\\server\share\…` → `\\?\UNC\server\share\…` (separators normalized to
        // backslashes, as the `\\?\` namespace requires).
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(_, _)) => {
            let raw = path.as_os_str().to_string_lossy().replace('/', "\\");
            let rest = raw.strip_prefix(r"\\").ok_or_else(|| {
                GitError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("cannot form extended-length path for {}", path.display()),
                ))
            })?;
            Ok(Cow::Owned(PathBuf::from(format!(r"\\?\UNC\{rest}"))))
        }
        // No drive/UNC prefix — a relative or drive-relative path can't be made
        // extended-length without resolution. Surface a clear error (ADR 0012)
        // rather than hand a path that MAX_PATH might truncate.
        _ => Err(GitError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "managed worktree path must be absolute to use the extended-length form: {}",
                path.display()
            ),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
    #[cfg(unix)]
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
    fn pr_branch_search_chunks_caps_heads_per_query() {
        // Seven branches => six `OR`s in one query, over GitHub's five-operator
        // limit; chunking must split into 6 + 1.
        let branches: Vec<String> = (0..7).map(|n| format!("branch-{n}")).collect();
        let chunks = pr_branch_search_chunks(&branches);
        assert_eq!(chunks.len(), 2, "seven heads must split across two queries");
        assert_eq!(chunks[0].matches(" OR ").count(), 5);
        assert!(!chunks[1].contains(" OR "), "trailing chunk holds one head");
        // Blank branches (detached worktrees) still contribute no query.
        assert!(pr_branch_search_chunks(&[String::new()]).is_empty());
    }

    #[test]
    fn parse_remote_owner_handles_common_url_shapes() {
        assert_eq!(
            parse_remote_owner("git@github.com:acme/widgets.git").as_deref(),
            Some("acme")
        );
        assert_eq!(
            parse_remote_owner("https://github.com/acme/widgets.git").as_deref(),
            Some("acme")
        );
        assert_eq!(
            parse_remote_owner("https://github.com/acme/widgets").as_deref(),
            Some("acme")
        );
        assert_eq!(
            parse_remote_owner("ssh://git@github.com:22/acme/widgets.git").as_deref(),
            Some("acme")
        );
        assert_eq!(parse_remote_owner("not-a-url").as_deref(), None);
    }

    #[test]
    #[cfg(unix)]
    fn pr_list_for_branches_drops_fork_prs_sharing_a_local_branch_name() {
        let fixture = RepoFixture::new();
        // Anchor the project on a known owner; a same-named fork PR from another
        // owner must not be mapped onto this local worktree branch.
        fixture.git([
            "remote",
            "add",
            "origin",
            "https://github.com/myowner/repo.git",
        ]);
        let bin = TempDir::new().unwrap();
        let log = bin.path().join("calls.log");
        let gh = fake_program(
            bin.path().join("gh"),
            &log,
            r#"[{"number":10,"url":"https://github.test/pr/10","state":"OPEN","isDraft":false,"headRefName":"patch-1","headRepositoryOwner":{"login":"myowner"}},{"number":99,"url":"https://github.test/pr/99","state":"OPEN","isDraft":false,"headRefName":"patch-1","headRepositoryOwner":{"login":"stranger"}}]"#,
        );
        let client = GitClient::with_programs("git", gh);
        let branches = vec!["patch-1".into()];

        let prs = pr_list_for_branches_with_client(&client, fixture.path(), &branches, None)
            .expect("PR list should parse fake gh output");

        assert_eq!(
            prs.len(),
            1,
            "fork PR sharing the branch name must be dropped"
        );
        assert_eq!(prs[0].0, "patch-1");
        assert_eq!(prs[0].1.number, 10);
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

        let diff = diff_file(fixture.path(), "tracked.txt", DiffTarget::Worktree, DiffFileOptions::default()).unwrap();
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
        let staged = diff_file(fixture.path(), "tracked.txt", DiffTarget::Staged, DiffFileOptions::default()).unwrap();
        assert!(staged.contains("changed"), "staged diff was {staged:?}");
    }

    #[test]
    fn diff_file_ignore_whitespace_drops_whitespace_only_changes() {
        let fixture = RepoFixture::new();
        // Commit a known baseline, then re-indent the same line — a pure
        // whitespace change.
        fixture.write("tracked.txt", "value\n");
        fixture.git(["add", "tracked.txt"]);
        fixture.git(["commit", "-m", "set tracked value"]);
        fixture.write("tracked.txt", "    value\n");

        let default = diff_file(
            fixture.path(),
            "tracked.txt",
            DiffTarget::Worktree,
            DiffFileOptions::default(),
        )
        .unwrap();
        assert!(
            default.contains("+    value"),
            "whitespace-sensitive diff was {default:?}"
        );

        let ignored = diff_file(
            fixture.path(),
            "tracked.txt",
            DiffTarget::Worktree,
            DiffFileOptions {
                ignore_whitespace: true,
                ..DiffFileOptions::default()
            },
        )
        .unwrap();
        assert!(
            ignored.is_empty(),
            "ignore_whitespace diff should be empty, was {ignored:?}"
        );
    }

    #[test]
    fn diff_file_context_lines_controls_hunk_context_size() {
        let fixture = RepoFixture::new();
        // Ten committed lines; flip the middle one so the hunk has equal context
        // available on both sides.
        let base: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        fixture.write("tracked.txt", &base);
        fixture.git(["add", "tracked.txt"]);
        fixture.git(["commit", "-m", "ten lines"]);
        let changed = base.replace("line5\n", "line5-changed\n");
        fixture.write("tracked.txt", &changed);

        let count_context = |context: u32| -> usize {
            let diff = diff_file(
                fixture.path(),
                "tracked.txt",
                DiffTarget::Worktree,
                DiffFileOptions {
                    context_lines: Some(context),
                    ..DiffFileOptions::default()
                },
            )
            .unwrap();
            // Context lines render with a leading space (not +/-/@/diff headers).
            diff.lines().filter(|line| line.starts_with(' ')).count()
        };

        // Zero context shows only the changed line; ten shows every other line.
        assert_eq!(count_context(0), 0);
        assert_eq!(count_context(10), 9);
    }

    #[test]
    fn status_reports_per_file_line_counts_on_the_relevant_side() {
        let fixture = RepoFixture::new();
        // tracked.txt: "initial\n" -> "changed\n" is 1 add + 1 del on the
        // worktree side. new.txt is a fresh file: 1 add, 0 del.
        fixture.write("tracked.txt", "changed\n");
        fixture.write("new.txt", "hello\n");

        let summary = status(fixture.path()).unwrap();
        let tracked = summary.entry_for("tracked.txt").unwrap();
        assert_eq!(
            (tracked.worktree_additions, tracked.worktree_deletions),
            (1, 1)
        );
        assert_eq!((tracked.staged_additions, tracked.staged_deletions), (0, 0));
        let new = summary.entry_for("new.txt").unwrap();
        assert_eq!((new.worktree_additions, new.worktree_deletions), (1, 0));

        // Staging tracked.txt moves its counts to the staged (HEAD↔index) side.
        GitClient::default()
            .stage_files(fixture.path(), &[PathBuf::from("tracked.txt")])
            .unwrap();
        let summary = status(fixture.path()).unwrap();
        let tracked = summary.entry_for("tracked.txt").unwrap();
        assert_eq!((tracked.staged_additions, tracked.staged_deletions), (1, 1));
        assert_eq!(
            (tracked.worktree_additions, tracked.worktree_deletions),
            (0, 0)
        );
    }

    #[test]
    fn status_attaches_staged_renamed_and_edited_line_counts_to_new_path() {
        let fixture = RepoFixture::new();
        fixture.write("tracked.txt", "one\ntwo\nthree\nfour\n");
        fixture.git(["add", "tracked.txt"]);
        fixture.git(["commit", "-m", "expand tracked"]);

        fs::rename(
            fixture.path().join("tracked.txt"),
            fixture.path().join("moved.txt"),
        )
        .unwrap();
        fixture.write("moved.txt", "one\nTWO\nthree\nfour\n");
        fixture.git(["add", "-A"]);

        let summary = status(fixture.path()).unwrap();
        assert_eq!(summary.entries.len(), 1, "entries were {summary:?}");
        assert!(summary.entry_for("tracked.txt").is_none());
        let moved = summary.entry_for("moved.txt").unwrap();
        assert_eq!(moved.old_path.as_deref(), Some(Path::new("tracked.txt")));
        assert_eq!(moved.index, FileState::Renamed);
        assert_eq!((moved.staged_additions, moved.staged_deletions), (1, 1));
        assert_eq!((moved.worktree_additions, moved.worktree_deletions), (0, 0));
        assert_eq!((summary.additions, summary.deletions), (1, 1));

        let diff = diff_file(fixture.path(), "moved.txt", DiffTarget::Staged, DiffFileOptions::default()).unwrap();
        assert!(
            diff.contains("rename from tracked.txt"),
            "diff was {diff:?}"
        );
        assert!(diff.contains("rename to moved.txt"), "diff was {diff:?}");
        assert!(diff.contains("-two"), "diff was {diff:?}");
        assert!(diff.contains("+TWO"), "diff was {diff:?}");
        assert!(!diff.contains("+one\n"), "diff was {diff:?}");
    }

    #[test]
    fn status_attaches_worktree_renamed_and_edited_line_counts_to_new_path() {
        let fixture = RepoFixture::new();
        fixture.write("tracked.txt", "one\ntwo\nthree\nfour\n");
        fixture.git(["add", "tracked.txt"]);
        fixture.git(["commit", "-m", "expand tracked"]);

        fs::rename(
            fixture.path().join("tracked.txt"),
            fixture.path().join("moved.txt"),
        )
        .unwrap();
        fixture.write("moved.txt", "one\nTWO\nthree\nfour\n");

        let summary = status(fixture.path()).unwrap();
        assert_eq!(summary.entries.len(), 1, "entries were {summary:?}");
        assert!(summary.entry_for("tracked.txt").is_none());
        let moved = summary.entry_for("moved.txt").unwrap();
        assert_eq!(moved.old_path.as_deref(), Some(Path::new("tracked.txt")));
        assert_eq!(moved.working_tree, FileState::Renamed);
        assert_eq!((moved.worktree_additions, moved.worktree_deletions), (1, 1));
        assert_eq!((moved.staged_additions, moved.staged_deletions), (0, 0));
        assert_eq!((summary.additions, summary.deletions), (1, 1));

        let diff = diff_file(fixture.path(), "moved.txt", DiffTarget::Worktree, DiffFileOptions::default()).unwrap();
        assert!(
            diff.contains("rename from tracked.txt"),
            "diff was {diff:?}"
        );
        assert!(diff.contains("rename to moved.txt"), "diff was {diff:?}");
        assert!(diff.contains("-two"), "diff was {diff:?}");
        assert!(diff.contains("+TWO"), "diff was {diff:?}");
        assert!(!diff.contains("+one\n"), "diff was {diff:?}");
    }

    // A rename across directories puts the old (deleted) side outside the new
    // path's parent directory, so the scoped status walk can't see the pairing.
    // diff_file must fall back to the full-repo walk and still report the rename
    // and its old path, for both the staged and worktree views. `dst/` already
    // holds a committed file so the new side never lands in a fresh untracked
    // directory (which `recurse_untracked_dirs(false)` would collapse out of
    // view — a limitation that predates this change and is orthogonal to it).
    #[test]
    fn diff_file_reports_cross_directory_rename() {
        for target in [DiffTarget::Staged, DiffTarget::Worktree] {
            let fixture = RepoFixture::new();
            fs::create_dir_all(fixture.path().join("src")).unwrap();
            fs::create_dir_all(fixture.path().join("dst")).unwrap();
            fixture.write("src/tracked.txt", "one\ntwo\nthree\nfour\n");
            fixture.write("dst/keep.txt", "keep\n");
            fixture.git(["add", "-A"]);
            fixture.git(["commit", "-m", "seed"]);

            fs::rename(
                fixture.path().join("src/tracked.txt"),
                fixture.path().join("dst/moved.txt"),
            )
            .unwrap();
            fixture.write("dst/moved.txt", "one\nTWO\nthree\nfour\n");
            if matches!(target, DiffTarget::Staged) {
                fixture.git(["add", "-A"]);
            }

            let diff =
                diff_file(fixture.path(), "dst/moved.txt", target, DiffFileOptions::default())
                    .unwrap();
            assert!(
                diff.contains("rename from src/tracked.txt"),
                "{target:?} diff was {diff:?}"
            );
            assert!(
                diff.contains("rename to dst/moved.txt"),
                "{target:?} diff was {diff:?}"
            );
            assert!(diff.contains("-two"), "{target:?} diff was {diff:?}");
            assert!(diff.contains("+TWO"), "{target:?} diff was {diff:?}");
            assert!(!diff.contains("+one\n"), "{target:?} diff was {diff:?}");
        }
    }

    #[test]
    fn status_and_diff_accept_repo_and_file_paths_with_spaces() {
        let fixture = RepoFixture::new_in_dir("repo with spaces");
        let file = PathBuf::from("dir with spaces/file with spaces.txt");
        fs::create_dir(fixture.path().join("dir with spaces")).unwrap();
        fs::write(fixture.path().join(&file), "initial\n").unwrap();
        run_real_git(
            fixture.path(),
            ["add", "dir with spaces/file with spaces.txt"],
        );
        run_real_git(fixture.path(), ["commit", "-m", "add spaced path"]);

        fs::write(fixture.path().join(&file), "changed\n").unwrap();

        let summary = status(fixture.path()).unwrap();
        let entry = summary.entry_for(&file).unwrap();
        assert_eq!(entry.working_tree, FileState::Modified);

        let relative = diff_file(fixture.path(), &file, DiffTarget::Worktree, DiffFileOptions::default()).unwrap();
        assert!(
            relative.contains("changed"),
            "relative diff was {relative:?}"
        );

        let absolute = diff_file(
            fixture.path(),
            fixture.path().join(&file),
            DiffTarget::Worktree,
            DiffFileOptions::default(),
        )
        .unwrap();
        assert!(
            absolute.contains("changed"),
            "absolute diff was {absolute:?}"
        );
    }

    #[test]
    fn staged_diff_accepts_absolute_file_path_with_spaces() {
        let fixture = RepoFixture::new_in_dir("repo with spaces");
        let file = PathBuf::from("file with spaces.txt");
        fs::write(fixture.path().join(&file), "initial\n").unwrap();
        run_real_git(fixture.path(), ["add", "file with spaces.txt"]);
        run_real_git(fixture.path(), ["commit", "-m", "add spaced path"]);

        fs::write(fixture.path().join(&file), "staged\n").unwrap();
        GitClient::default()
            .stage_files(fixture.path(), std::slice::from_ref(&file))
            .unwrap();

        let diff = diff_file(
            fixture.path(),
            fixture.path().join(&file),
            DiffTarget::Staged,
            DiffFileOptions::default(),
        )
        .unwrap();
        assert!(diff.contains("staged"), "staged diff was {diff:?}");
    }

    #[test]
    fn dirty_check_detects_changes_without_full_status_work() {
        let fixture = RepoFixture::new();
        assert!(!is_dirty(fixture.path()).unwrap());

        fixture.write("tracked.txt", "changed\n");
        assert!(is_dirty(fixture.path()).unwrap());
    }

    #[test]
    fn status_collapses_untracked_directories_without_recursing_for_stats() {
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
        let entry = &summary.entries[0];
        assert_eq!(entry.working_tree, FileState::New);
        assert_eq!((entry.worktree_additions, entry.worktree_deletions), (0, 0));
        assert_eq!((entry.staged_additions, entry.staged_deletions), (0, 0));
        assert_eq!((summary.additions, summary.deletions), (0, 0));
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
            fs::read_to_string(fixture.path().join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
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
    #[cfg(unix)]
    fn commit_uses_system_git_and_fires_repo_hooks() {
        let fixture = RepoFixture::new();
        let marker = fixture.path().join("hook-ran");
        let hook = fixture.path().join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            format!("#!/bin/sh\necho hook > {}\n", marker.display()),
        )
        .unwrap();
        make_executable(&hook);

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

    #[cfg(unix)]
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
        let path = managed_worktree_path("/tmp/root", "my/project", "feature/test");
        let components = path
            .strip_prefix("/tmp/root")
            .unwrap()
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            components,
            [
                "my_project-ab25a1fcafb8fb99",
                "feature_test-05b3fea2f0a81a1e"
            ]
        );
    }

    #[test]
    fn managed_path_components_are_windows_safe_and_collision_resistant() {
        let branch_a = safe_path_component(r#"feature/a:bad*name? " <x>|"#);
        assert_eq!(branch_a, "feature_a_bad_name_ _ _x__-2d0df6d4ab28ba66");

        let branch_b = safe_path_component("feature\\a:bad*name? \" <x>|");
        assert_eq!(branch_b, "feature_a_bad_name_ _ _x__-c2fc782d233557a7");
        assert_ne!(branch_a, branch_b);

        for value in [
            "CON",
            "prn",
            "AUX.",
            "nul ",
            "COM1",
            "LPT9.txt",
            "...",
            "   ",
            "\u{0000}\u{001f}",
        ] {
            let component = safe_path_component(value);
            assert_windows_safe_component(&component);
        }

        let long = safe_path_component(&format!("{}.", "a".repeat(300)));
        assert_windows_safe_component(&long);
        assert_eq!(
            long.chars().count(),
            MAX_SAFE_PATH_COMPONENT_PREFIX_CHARS + 17
        );
    }

    fn assert_windows_safe_component(component: &str) {
        assert!(!component.is_empty());
        assert!(!component.ends_with(['.', ' ']));
        assert!(!component.chars().any(|ch| matches!(
            ch,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        ) || ch.is_control()));

        let stem = component
            .split_once('.')
            .map_or(component, |(stem, _)| stem)
            .to_ascii_uppercase();
        assert!(!matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn extended_length_path_prefixes_only_real_drive_and_unc_paths() {
        // Drive path gains the verbatim prefix.
        let drive = extended_length_path(Path::new(r"C:\Users\me\.hitch\worktrees\p\b")).unwrap();
        assert_eq!(drive.as_os_str(), r"\\?\C:\Users\me\.hitch\worktrees\p\b");

        // UNC path maps to the \\?\UNC\ form.
        let unc = extended_length_path(Path::new(r"\\server\share\worktrees\p")).unwrap();
        assert_eq!(unc.as_os_str(), r"\\?\UNC\server\share\worktrees\p");

        // An already-extended path (what canonicalize returns) is untouched.
        let already = Path::new(r"\\?\C:\already\verbatim");
        assert_eq!(
            extended_length_path(already).unwrap().as_os_str(),
            already.as_os_str()
        );
        let already_unc = Path::new(r"\\?\UNC\server\share\x");
        assert_eq!(
            extended_length_path(already_unc).unwrap().as_os_str(),
            already_unc.as_os_str()
        );

        // A relative path can't be made extended-length: clear error, not truncation.
        assert!(extended_length_path(Path::new(r"worktrees\p\b")).is_err());
    }

    struct RepoFixture {
        _temp: TempDir,
        path: PathBuf,
    }

    impl RepoFixture {
        fn new() -> Self {
            Self::new_in_dir("repo")
        }

        fn new_in_dir(name: &str) -> Self {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join(name);
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

    #[cfg(unix)]
    fn fake_program(path: PathBuf, log: &Path, stdout: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\nprintf '%s ' \"$(basename $0)\" >> {log}\nfor arg in \"$@\"; do printf '%s ' \"$arg\" >> {log}; done\nprintf '\\n' >> {log}\nprintf '{stdout}'\n",
            log = shell_quote(log),
            stdout = stdout.replace('\\', "\\\\").replace('\'', "'\\''")
        );
        fs::write(&path, script).unwrap();
        make_executable(&path);
        path
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
    #[cfg(any(unix, windows))]
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    #[cfg(any(unix, windows))]
    struct TestControl {
        cancelled: AtomicBool,
        registrations: Mutex<Vec<bool>>,
        current: Mutex<Option<ProcessTree>>,
        cancel_on_clear: bool,
    }

    #[cfg(any(unix, windows))]
    impl TestControl {
        fn new(cancel_on_clear: bool) -> Self {
            Self {
                cancelled: AtomicBool::new(false),
                registrations: Mutex::new(Vec::new()),
                current: Mutex::new(None),
                cancel_on_clear,
            }
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(any(unix, windows))]
    impl CommandControl for TestControl {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }

        fn set_process_tree(&self, tree: Option<ProcessTree>) {
            let registered = tree.is_some();
            *self.current.lock().unwrap() = tree;
            self.registrations.lock().unwrap().push(registered);
            if self.cancel_on_clear && !registered {
                self.cancel();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_command_keeps_success_when_cancellation_arrives_after_exit() {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-success");
        fs::write(&script, "#!/bin/sh\nprintf 'done'\n").unwrap();
        make_executable(&script);

        let control = TestControl::new(true);
        let output = run_command(&script, temp.path(), Vec::new(), Some(&control)).unwrap();

        assert_eq!(output.stdout, "done");
        assert_eq!(output.stderr, "");
        assert!(
            control.is_cancelled(),
            "test precondition: cancel should flip after exit"
        );
        let registrations = control.registrations.lock().unwrap();
        assert_eq!(registrations.first().copied(), Some(true));
        assert_eq!(registrations.last().copied(), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn run_command_does_not_wait_for_grandchild_holding_stdout() {
        use std::time::Instant;

        // Regression: a command that exits 0 on its own (the NORMAL-EXIT path)
        // after backgrounding a child that inherited stdout must not keep the
        // reader join blocked until that child exits. The post-reap drain cannot
        // group-kill on Unix (recycled-pgid hazard — `terminate_after_leader_reaped`
        // is a deliberate no-op there), so the reader gets no EOF; the bounded
        // post-reap wait must detach the stuck reader and return the output the
        // command already printed.
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-backgrounds-child");
        fs::write(
            &script,
            "#!/bin/sh\n# Background child inherits stdout (fd 1) and holds it open past our wait.\nsleep 30 &\nprintf 'done'\n",
        )
        .unwrap();
        make_executable(&script);

        // A live control routes run_command through the spawn/pipe-reader path
        // (the `None` path uses `Command::output`, which never has this hazard).
        let control = TestControl::new(false);
        let started = Instant::now();
        let output = run_command(&script, temp.path(), Vec::new(), Some(&control)).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(output.stdout, "done");
        assert!(
            elapsed < Duration::from_secs(10),
            "run_command blocked on inherited stdout pipe for {elapsed:?}; \
             the post-reap reader drain should be bounded"
        );
    }

    /// Regression: the external cancellation handle must be disarmed
    /// (`set_process_tree(None)`) *before* the stdout/stderr pipe readers are
    /// joined, not after. Otherwise a concurrent cancel arriving during the
    /// drain calls `tree.terminate()` on a child that has already been reaped;
    /// on Unix that is `kill(-pgid)` against a process group whose pgid the OS
    /// may have recycled.
    ///
    /// The recycled-pgid race itself depends on OS pid reuse timing and cannot
    /// be triggered deterministically, so this test instead pins the ordering
    /// the fix guarantees. A signalling control creates a sentinel file the
    /// moment the tree is disarmed; the child writes one line, then blocks until
    /// that sentinel exists before exiting (keeping its stdout pipe open). If the
    /// disarm runs before the join (correct), the sentinel appears, the child
    /// exits, the pipe closes, and the join completes. If the disarm ran only
    /// after the join (the pre-fix ordering), the join would wait on the child
    /// while the child waits on the sentinel — a deadlock the watchdog catches.
    #[cfg(unix)]
    #[test]
    fn run_command_disarms_cancel_handle_before_joining_pipe_readers() {
        use std::sync::Arc;
        use std::time::Duration;

        struct SignalOnDisarm {
            sentinel: PathBuf,
        }
        impl CommandControl for SignalOnDisarm {
            fn is_cancelled(&self) -> bool {
                false
            }
            fn set_process_tree(&self, tree: Option<ProcessTree>) {
                if tree.is_none() {
                    // Disarm: release the child blocking on this sentinel.
                    let _ = fs::write(&self.sentinel, b"go");
                }
            }
        }

        let temp = TempDir::new().unwrap();
        let sentinel = temp.path().join("disarm-sentinel");
        let script = temp.path().join("git-blocks-until-disarm");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'done'\nwhile [ ! -e {sentinel} ]; do sleep 0.01; done\n",
                sentinel = shell_quote(&sentinel),
            ),
        )
        .unwrap();
        make_executable(&script);

        let control = Arc::new(SignalOnDisarm {
            sentinel: sentinel.clone(),
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let runner = {
            let control = Arc::clone(&control);
            let script = script.clone();
            let cwd = temp.path().to_path_buf();
            thread::spawn(move || {
                let result = run_command(&script, &cwd, Vec::new(), Some(control.as_ref()));
                let _ = tx.send(());
                result
            })
        };

        // Watchdog: a disarm-after-join ordering deadlocks here.
        rx.recv_timeout(Duration::from_secs(10))
            .expect("run_command deadlocked: cancel handle was not disarmed before pipe join");

        let output = runner.join().unwrap().unwrap();
        assert_eq!(output.stdout, "done");
        assert!(
            sentinel.exists(),
            "disarm should have created the sentinel that releases the child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_command_reports_mid_flight_cancellation() {
        use std::time::Duration;

        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-sleep");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        make_executable(&script);

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
        let registrations = control.registrations.lock().unwrap();
        assert_eq!(registrations.first().copied(), Some(true));
        assert_eq!(registrations.last().copied(), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn controlled_clone_kills_the_child_when_cancelled() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-sleep");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        make_executable(&script);

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

        let registrations = control.registrations.lock().unwrap();
        assert_eq!(registrations.first().copied(), Some(true));
        assert_eq!(registrations.last().copied(), Some(false));
    }

    #[cfg(windows)]
    #[test]
    fn controlled_clone_cancellation_kills_windows_job_tree_with_grandchild() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let temp = TempDir::new().unwrap();
        let script = temp.path().join("git-sleep.cmd");
        fs::write(
            &script,
            "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -Command \"Start-Process powershell -ArgumentList '-NoProfile','-Command','while ($true) { Write-Output grandchild; Write-Error grandchild; Start-Sleep -Seconds 1 }' -NoNewWindow; while ($true) { Start-Sleep -Seconds 1 }\"\r\n",
        )
        .unwrap();

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
            "cancellation should not wait for a stdout/stderr-inheriting grandchild"
        );
        match err {
            GitError::CommandFailed { stderr, .. } => {
                assert!(stderr.contains("command cancelled"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let registrations = control.registrations.lock().unwrap();
        assert_eq!(registrations.first().copied(), Some(true));
        assert_eq!(registrations.last().copied(), Some(false));
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
