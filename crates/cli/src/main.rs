use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::Parser;
use repo_auto_puller_core::{RepoSyncer, SyncDecision};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(
    name = "repo-auto-puller",
    version,
    about = "Watch one or more Git repositories and fast-forward pull when upstream branches get new commits."
)]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,

    #[arg(long)]
    once: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    repo: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AppConfig {
    #[serde(default)]
    defaults: DefaultsConfig,
    #[serde(default)]
    repositories: Vec<RepositoryConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct DefaultsConfig {
    log_file: Option<PathBuf>,
    #[serde(default)]
    verbose: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct RepositoryConfig {
    name: String,
    path: PathBuf,
    #[serde(default = "default_interval_seconds")]
    interval_seconds: f64,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    dry_run: bool,
}

struct Logger {
    writer: Box<dyn Write + Send>,
}

struct ManagedRepo {
    config: RepositoryConfig,
    syncer: RepoSyncer,
    next_run_at: Instant,
    last_message: Option<String>,
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

    fn log(&mut self, level: &str, scope: &str, message: impl AsRef<str>) -> Result<()> {
        let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
        writeln!(
            self.writer,
            "[{timestamp}] [{level}] [{scope}] {}",
            message.as_ref()
        )
        .context("failed to write log line")?;
        self.writer.flush().context("failed to flush log writer")?;
        Ok(())
    }
}

fn default_interval_seconds() -> f64 {
    60.0
}

fn default_enabled() -> bool {
    true
}

fn load_config(path: &Path) -> Result<AppConfig> {
    let path = expand_tilde(path);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config: AppConfig =
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
    if config.repositories.is_empty() {
        bail!("config has no repositories");
    }
    Ok(config)
}

fn build_logger(config: &AppConfig) -> Result<Logger> {
    match config.defaults.log_file.as_deref() {
        Some(path) => Logger::file(&expand_tilde(path)),
        None => Ok(Logger::stdout()),
    }
}

fn build_managed_repos(config: AppConfig, cli: &Cli) -> Result<(Logger, bool, Vec<ManagedRepo>)> {
    let logger = build_logger(&config)?;
    let verbose = cli.verbose || config.defaults.verbose;
    let selected = &cli.repo;
    let mut repos = Vec::new();

    for repo in config.repositories {
        if !repo.enabled {
            continue;
        }
        if !selected.is_empty() && !selected.iter().any(|name| name == &repo.name) {
            continue;
        }
        if repo.interval_seconds <= 0.0 {
            bail!("repository {} has invalid interval_seconds", repo.name);
        }
        let syncer = RepoSyncer::new(expand_tilde(&repo.path))
            .with_context(|| format!("failed to initialize repository {}", repo.name))?;
        repos.push(ManagedRepo {
            config: repo,
            syncer,
            next_run_at: Instant::now(),
            last_message: None,
        });
    }

    if repos.is_empty() {
        bail!("no repositories matched the current config and CLI filters");
    }

    Ok((logger, verbose, repos))
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    path.to_path_buf()
}

fn sync_repo(
    logger: &mut Logger,
    managed: &mut ManagedRepo,
    global_dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let snapshot_before_fetch = managed.syncer.read_snapshot()?;
    managed.syncer.fetch(&snapshot_before_fetch)?;
    let snapshot = managed.syncer.read_snapshot()?;
    let (level, message) = RepoSyncer::describe(&snapshot);

    if verbose || level != "IDLE" || managed.last_message.as_deref() != Some(message.as_str()) {
        logger.log(level, &managed.config.name, &message)?;
        managed.last_message = Some(message);
    }

    if RepoSyncer::sync_decision(&snapshot) != SyncDecision::PullFastForward {
        return Ok(());
    }

    if global_dry_run || managed.config.dry_run {
        logger.log(
            "INFO",
            &managed.config.name,
            format!(
                "dry-run: would fast-forward pull {}/{} into {}",
                snapshot.remote, snapshot.remote_branch, snapshot.branch
            ),
        )?;
        return Ok(());
    }

    logger.log(
        "INFO",
        &managed.config.name,
        format!(
            "pulling {}/{} into {}",
            snapshot.remote, snapshot.remote_branch, snapshot.branch
        ),
    )?;
    managed.syncer.pull_fast_forward(&snapshot)?;
    logger.log(
        "INFO",
        &managed.config.name,
        format!("auto-pull complete: {}", managed.syncer.head_summary()?),
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config(&cli.config)?;
    let (mut logger, verbose, mut repos) = build_managed_repos(config, &cli)?;

    let keep_running = Arc::new(AtomicBool::new(true));
    let signal_flag = Arc::clone(&keep_running);
    ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    })
    .context("failed to install signal handlers")?;

    if cli.once {
        for managed in &mut repos {
            if let Err(err) = sync_repo(&mut logger, managed, cli.dry_run, verbose) {
                logger.log("ERROR", &managed.config.name, err.to_string())?;
            }
        }
        return Ok(());
    }

    for managed in &repos {
        logger.log(
            "INFO",
            &managed.config.name,
            format!(
                "watching {} every {}s",
                managed.syncer.repo_path().display(),
                managed.config.interval_seconds
            ),
        )?;
    }

    while keep_running.load(Ordering::SeqCst) {
        let now = Instant::now();
        let mut earliest_next: Option<Instant> = None;

        for managed in &mut repos {
            if now < managed.next_run_at {
                earliest_next = Some(match earliest_next {
                    Some(existing) => existing.min(managed.next_run_at),
                    None => managed.next_run_at,
                });
                continue;
            }

            if let Err(err) = sync_repo(&mut logger, managed, cli.dry_run, verbose) {
                logger.log("ERROR", &managed.config.name, err.to_string())?;
            }

            managed.next_run_at = Instant::now() + Duration::from_secs_f64(managed.config.interval_seconds);
            earliest_next = Some(match earliest_next {
                Some(existing) => existing.min(managed.next_run_at),
                None => managed.next_run_at,
            });
        }

        if !keep_running.load(Ordering::SeqCst) {
            break;
        }

        let sleep_for = earliest_next
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|| Duration::from_secs(1))
            .min(Duration::from_secs(1));
        thread::sleep(sleep_for);
    }

    logger.log("INFO", "app", "repo-auto-puller stopped")?;
    Ok(())
}
