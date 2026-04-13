use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "repo-auto-puller",
    version,
    about = "Watch a Git repository and fast-forward pull when the upstream branch gets new commits."
)]
struct Cli {
    #[arg(long, default_value = "/home/lyz/pros/openclawcode")]
    repo: PathBuf,

    #[arg(long, default_value_t = 60.0)]
    interval: f64,

    #[arg(long)]
    once: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    log_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    branch: String,
    upstream: String,
    remote: String,
    remote_branch: String,
    ahead: u32,
    behind: u32,
    dirty: bool,
}

struct Logger {
    writer: Box<dyn Write + Send>,
}

impl Logger {
    fn stdout() -> Self {
        Self {
            writer: Box::new(io::stdout()),
        }
    }

    fn file(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create log directory {}", parent.display())
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open log file {}", path.display()))?;
        Ok(Self {
            writer: Box::new(file),
        })
    }

    fn log(&mut self, level: &str, message: impl AsRef<str>) -> Result<()> {
        let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
        writeln!(self.writer, "[{timestamp}] [{level}] {}", message.as_ref())
            .context("failed to write log line")?;
        self.writer.flush().context("failed to flush log writer")?;
        Ok(())
    }
}

struct RepoAutoPuller {
    repo: PathBuf,
    interval: Duration,
    logger: Logger,
    verbose: bool,
    dry_run: bool,
    keep_running: Arc<AtomicBool>,
    last_snapshot: Option<Snapshot>,
}

impl RepoAutoPuller {
    fn new(cli: Cli) -> Result<Self> {
        if cli.interval <= 0.0 {
            bail!("--interval must be greater than 0");
        }

        let repo = resolve_repo(&cli.repo)?;
        let logger = match cli.log_file.as_deref() {
            Some(path) => Logger::file(path)?,
            None => Logger::stdout(),
        };

        Ok(Self {
            repo,
            interval: Duration::from_secs_f64(cli.interval),
            logger,
            verbose: cli.verbose,
            dry_run: cli.dry_run,
            keep_running: Arc::new(AtomicBool::new(true)),
            last_snapshot: None,
        })
    }

    fn install_signal_handlers(&self) -> Result<()> {
        let keep_running = Arc::clone(&self.keep_running);
        ctrlc::set_handler(move || {
            keep_running.store(false, Ordering::SeqCst);
        })
        .context("failed to install signal handlers")
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

    fn read_snapshot(&self) -> Result<Snapshot> {
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
            .ok_or_else(|| anyhow!("unexpected upstream name: {upstream}"))?;

        let dirty = !self.run_git(&["status", "--porcelain"])?.is_empty();
        let counts = self.run_git(&["rev-list", "--left-right", "--count", &format!("HEAD...{upstream}")])?;
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
            branch,
            upstream,
            remote: remote.to_owned(),
            remote_branch: remote_branch.to_owned(),
            ahead,
            behind,
            dirty,
        })
    }

    fn fetch(&self, snapshot: &Snapshot) -> Result<()> {
        self.run_git(&["fetch", "--quiet", &snapshot.remote, &snapshot.remote_branch])?;
        Ok(())
    }

    fn pull(&self, snapshot: &Snapshot) -> Result<()> {
        self.run_git(&[
            "pull",
            "--ff-only",
            "--no-rebase",
            &snapshot.remote,
            &snapshot.remote_branch,
        ])?;
        Ok(())
    }

    fn head_summary(&self) -> Result<String> {
        self.run_git(&["log", "-1", "--oneline", "--decorate=short", "HEAD"])
    }

    fn describe(snapshot: &Snapshot) -> (&'static str, String) {
        if snapshot.behind == 0 && snapshot.ahead == 0 {
            return (
                "IDLE",
                format!("{} is up to date with {}", snapshot.branch, snapshot.upstream),
            );
        }
        if snapshot.behind > 0 && snapshot.ahead > 0 {
            return (
                "WARN",
                format!(
                    "{} diverged from {} (ahead {}, behind {}); skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.ahead, snapshot.behind
                ),
            );
        }
        if snapshot.dirty && snapshot.behind > 0 {
            return (
                "WARN",
                format!(
                    "{} is behind {} by {} commit(s), but the working tree is dirty; skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.behind
                ),
            );
        }
        if snapshot.ahead > 0 {
            return (
                "WARN",
                format!(
                    "{} is ahead of {} by {} commit(s); skipping auto-pull",
                    snapshot.branch, snapshot.upstream, snapshot.ahead
                ),
            );
        }
        (
            "INFO",
            format!(
                "{} is behind {} by {} commit(s)",
                snapshot.branch, snapshot.upstream, snapshot.behind
            ),
        )
    }

    fn maybe_log_snapshot(&mut self, snapshot: &Snapshot) -> Result<()> {
        if self.last_snapshot.as_ref() == Some(snapshot) && !self.verbose {
            return Ok(());
        }

        let (level, message) = Self::describe(snapshot);
        if level != "IDLE" || self.verbose || self.last_snapshot.as_ref() != Some(snapshot) {
            self.logger.log(level, message)?;
        }
        self.last_snapshot = Some(snapshot.clone());
        Ok(())
    }

    fn sync_once(&mut self) -> Result<()> {
        let snapshot_before_fetch = self.read_snapshot()?;
        self.fetch(&snapshot_before_fetch)?;
        let snapshot = self.read_snapshot()?;
        self.maybe_log_snapshot(&snapshot)?;

        if snapshot.behind == 0 {
            return Ok(());
        }
        if snapshot.ahead > 0 || snapshot.dirty {
            return Ok(());
        }
        if self.dry_run {
            self.logger.log(
                "INFO",
                format!(
                    "dry-run: {} would fast-forward pull {}/{}",
                    snapshot.branch, snapshot.remote, snapshot.remote_branch
                ),
            )?;
            return Ok(());
        }

        self.logger.log(
            "INFO",
            format!(
                "pulling {}/{} into {}",
                snapshot.remote, snapshot.remote_branch, snapshot.branch
            ),
        )?;
        self.pull(&snapshot)?;

        let final_snapshot = self.read_snapshot()?;
        self.last_snapshot = Some(final_snapshot.clone());
        self.logger
            .log("INFO", format!("auto-pull complete: {}", self.head_summary()?))?;

        if final_snapshot.behind != 0 || final_snapshot.ahead != 0 {
            self.logger.log(
                "WARN",
                format!(
                    "post-pull status is still ahead {}, behind {}",
                    final_snapshot.ahead, final_snapshot.behind
                ),
            )?;
        }

        Ok(())
    }

    fn run(&mut self, once: bool) -> Result<()> {
        self.install_signal_handlers()?;
        if once {
            return self.sync_once();
        }

        self.logger.log(
            "INFO",
            format!(
                "watching {} every {}s",
                self.repo.display(),
                self.interval.as_secs_f64()
            ),
        )?;

        while self.keep_running.load(Ordering::SeqCst) {
            if let Err(err) = self.sync_once() {
                self.logger.log("ERROR", err.to_string())?;
            }

            if !self.keep_running.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(self.interval);
        }

        self.logger.log("INFO", "repo-auto-puller stopped")?;
        Ok(())
    }
}

fn resolve_repo(path: &Path) -> Result<PathBuf> {
    let repo = path
        .canonicalize()
        .with_context(|| format!("failed to resolve repo path {}", path.display()))?;
    if !repo.join(".git").exists() {
        bail!("{} is not a Git repository", repo.display());
    }
    Ok(repo)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let once = cli.once;
    let mut puller = RepoAutoPuller::new(cli)?;
    puller.run(once)
}
