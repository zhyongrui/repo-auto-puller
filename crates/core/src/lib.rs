use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub branch: String,
    pub upstream: String,
    pub remote: String,
    pub remote_branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoContext {
    pub branch: String,
    pub upstream: String,
    pub remote: String,
    pub remote_branch: String,
    pub dirty: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncDecision {
    UpToDate,
    Diverged,
    DirtyBehind,
    AheadOnly,
    PullFastForward,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncReport {
    pub decision: SyncDecision,
    pub level: &'static str,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct RepoSyncer {
    repo: PathBuf,
}

impl RepoSyncer {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let repo = resolve_repo(path.as_ref())?;
        Ok(Self { repo })
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo
    }

    pub fn read_context(&self) -> Result<RepoContext> {
        let branch = self.run_git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        if branch == "HEAD" {
            bail!("detached HEAD is not supported");
        }

        let upstream = self.run_git(&[
            "for-each-ref",
            "--format=%(upstream:short)",
            &format!("refs/heads/{branch}"),
        ])?;
        if upstream.is_empty() {
            bail!("branch {branch:?} has no upstream configured");
        }

        let (remote, remote_branch) = upstream
            .split_once('/')
            .map(|(remote, branch)| (remote.to_owned(), branch.to_owned()))
            .ok_or_else(|| anyhow!("unexpected upstream name: {upstream}"))?;

        let dirty = !self.run_git(&["status", "--porcelain"])?.is_empty();

        Ok(RepoContext {
            branch,
            upstream,
            remote,
            remote_branch,
            dirty,
        })
    }

    pub fn read_snapshot(&self) -> Result<Snapshot> {
        let context = self.read_context()?;
        let counts = self.run_git(&[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{}", context.upstream),
        ])?;
        let mut parts = counts.split_whitespace();
        let ahead = parts
            .next()
            .ok_or_else(|| anyhow!("missing ahead count"))?
            .parse::<u32>()
            .context("failed to parse ahead count")?;
        let behind = parts
            .next()
            .ok_or_else(|| anyhow!("missing behind count"))?
            .parse::<u32>()
            .context("failed to parse behind count")?;

        Ok(Snapshot {
            branch: context.branch,
            upstream: context.upstream,
            remote: context.remote,
            remote_branch: context.remote_branch,
            ahead,
            behind,
            dirty: context.dirty,
        })
    }

    pub fn fetch_ref(&self, remote: &str, remote_branch: &str) -> Result<()> {
        self.run_git(&["fetch", "--quiet", remote, remote_branch])?;
        Ok(())
    }

    pub fn fetch(&self, snapshot: &Snapshot) -> Result<()> {
        self.fetch_ref(&snapshot.remote, &snapshot.remote_branch)
    }

    pub fn pull_fast_forward(&self, snapshot: &Snapshot) -> Result<()> {
        self.run_git(&[
            "pull",
            "--ff-only",
            "--no-rebase",
            &snapshot.remote,
            &snapshot.remote_branch,
        ])?;
        Ok(())
    }

    pub fn head_summary(&self) -> Result<String> {
        self.run_git(&["log", "-1", "--oneline", "--decorate=short", "HEAD"])
    }

    pub fn sync_decision(snapshot: &Snapshot) -> SyncDecision {
        if snapshot.behind == 0 && snapshot.ahead == 0 {
            return SyncDecision::UpToDate;
        }
        if snapshot.behind > 0 && snapshot.ahead > 0 {
            return SyncDecision::Diverged;
        }
        if snapshot.dirty && snapshot.behind > 0 {
            return SyncDecision::DirtyBehind;
        }
        if snapshot.ahead > 0 {
            return SyncDecision::AheadOnly;
        }
        SyncDecision::PullFastForward
    }

    pub fn report(snapshot: &Snapshot) -> SyncReport {
        let decision = Self::sync_decision(snapshot);
        let (level, message) = match decision {
            SyncDecision::UpToDate => (
                "IDLE",
                format!(
                    "{} is up to date with {}",
                    snapshot.branch, snapshot.upstream
                ),
            ),
            SyncDecision::Diverged => (
                "WARN",
                format!(
                    "{} diverged from {} (ahead {}, behind {}); skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.ahead, snapshot.behind
                ),
            ),
            SyncDecision::DirtyBehind => (
                "WARN",
                format!(
                    "{} is behind {} by {} commit(s), but the working tree is dirty; skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.behind
                ),
            ),
            SyncDecision::AheadOnly => (
                "WARN",
                format!(
                    "{} is ahead of {} by {} commit(s); skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.ahead
                ),
            ),
            SyncDecision::PullFastForward => (
                "INFO",
                format!(
                    "{} is behind {} by {} commit(s)",
                    snapshot.branch, snapshot.upstream, snapshot.behind
                ),
            ),
        };

        SyncReport {
            decision,
            level,
            message,
        }
    }

    pub fn describe(snapshot: &Snapshot) -> (&'static str, String) {
        let report = Self::report(snapshot);
        (report.level, report.message)
    }

    fn run_git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .with_context(|| format!("failed to execute git {}", args.join(" ")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            bail!("git {} failed: {}", args.join(" "), stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

impl SyncDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncDecision::UpToDate => "up-to-date",
            SyncDecision::Diverged => "diverged",
            SyncDecision::DirtyBehind => "dirty-behind",
            SyncDecision::AheadOnly => "ahead-only",
            SyncDecision::PullFastForward => "pull-fast-forward",
        }
    }
}

pub fn resolve_repo(path: &Path) -> Result<PathBuf> {
    let repo = path
        .canonicalize()
        .with_context(|| format!("failed to resolve repo path {}", path.display()))?;
    if !repo.join(".git").exists() {
        bail!("{} is not a Git repository", repo.display());
    }
    Ok(repo)
}

#[cfg(test)]
mod tests {
    use super::{RepoSyncer, Snapshot, SyncDecision};

    #[test]
    fn chooses_up_to_date() {
        let snapshot = Snapshot {
            branch: "main".into(),
            upstream: "origin/main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            ahead: 0,
            behind: 0,
            dirty: false,
        };
        assert_eq!(RepoSyncer::sync_decision(&snapshot), SyncDecision::UpToDate);
    }

    #[test]
    fn chooses_dirty_behind_before_pull() {
        let snapshot = Snapshot {
            branch: "main".into(),
            upstream: "origin/main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            ahead: 0,
            behind: 2,
            dirty: true,
        };
        assert_eq!(
            RepoSyncer::sync_decision(&snapshot),
            SyncDecision::DirtyBehind
        );
    }

    #[test]
    fn chooses_diverged() {
        let snapshot = Snapshot {
            branch: "main".into(),
            upstream: "origin/main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            ahead: 1,
            behind: 2,
            dirty: false,
        };
        assert_eq!(RepoSyncer::sync_decision(&snapshot), SyncDecision::Diverged);
    }
}
