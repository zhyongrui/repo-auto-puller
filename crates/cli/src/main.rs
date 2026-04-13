use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::{Args, Parser, Subcommand};
use repo_auto_puller_core::{RepoSyncer, SyncDecision};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "repo-auto-puller",
    version,
    about = "Watch one or more Git repositories and fast-forward pull when upstream branches get new commits."
)]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,

    #[command(flatten)]
    run: RunArgs,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Init(InitArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long)]
    repo_path: PathBuf,

    #[arg(long)]
    name: Option<String>,

    #[arg(long, default_value_t = 60.0)]
    interval: f64,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    disabled: bool,
}

#[derive(Clone, Debug, Args)]
struct RunArgs {
    #[arg(long)]
    once: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    repo: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AppConfig {
    #[serde(default)]
    defaults: DefaultsConfig,
    #[serde(default)]
    repositories: Vec<RepositoryConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct DefaultsConfig {
    log_file: Option<PathBuf>,
    #[serde(default)]
    verbose: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create log directory {}", parent.display()))?;
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

fn load_config_if_exists(path: &Path) -> Result<Option<AppConfig>> {
    let path = expand_tilde(path);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(load_config(&path)?))
}

fn build_logger(config: &AppConfig) -> Result<Logger> {
    match config.defaults.log_file.as_deref() {
        Some(path) => Logger::file(&expand_tilde(path)),
        None => Ok(Logger::stdout()),
    }
}

fn build_managed_repos(
    config: AppConfig,
    run_args: &RunArgs,
) -> Result<(Logger, bool, Vec<ManagedRepo>)> {
    let logger = build_logger(&config)?;
    let verbose = run_args.verbose || config.defaults.verbose;
    let selected = &run_args.repo;
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
    if raw == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home);
    }
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    path.to_path_buf()
}

fn sanitize_repo_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    let mut last_was_dash = false;

    for ch in name.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            last_was_dash = false;
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch == ' ' {
            if last_was_dash {
                None
            } else {
                last_was_dash = true;
                Some('-')
            }
        } else {
            None
        };

        if let Some(ch) = normalized {
            sanitized.push(ch);
        }
    }

    sanitized.trim_matches('-').to_owned()
}

fn infer_repo_name(repo_path: &Path) -> Result<String> {
    let name = repo_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_repo_name)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to infer repository name from path {}",
                repo_path.display()
            )
        })?;
    Ok(name)
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    Ok(())
}

fn upsert_repository(config: &mut AppConfig, repo: RepositoryConfig) {
    if let Some(existing) = config
        .repositories
        .iter_mut()
        .find(|existing| existing.name == repo.name)
    {
        *existing = repo;
    } else {
        config.repositories.push(repo);
    }
}

fn write_config(path: &Path, config: &AppConfig) -> Result<()> {
    let path = expand_tilde(path);
    ensure_parent_dir(&path)?;
    let toml = toml::to_string_pretty(config).context("failed to serialize config")?;
    fs::write(&path, toml).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn init_config(config_path: &Path, args: &InitArgs) -> Result<()> {
    if args.interval <= 0.0 {
        bail!("--interval must be greater than 0");
    }

    let repo_path = repo_auto_puller_core::resolve_repo(&expand_tilde(&args.repo_path))?;
    let name = match &args.name {
        Some(name) => {
            let name = sanitize_repo_name(name);
            if name.is_empty() {
                bail!("repository name must contain at least one alphanumeric character");
            }
            name
        }
        None => infer_repo_name(&repo_path)?,
    };

    let mut config = load_config_if_exists(config_path)?.unwrap_or_else(|| AppConfig {
        defaults: DefaultsConfig {
            log_file: Some(PathBuf::from(
                "~/.local/state/repo-auto-puller/repo-auto-puller.log",
            )),
            verbose: false,
        },
        repositories: Vec::new(),
    });

    upsert_repository(
        &mut config,
        RepositoryConfig {
            name: name.clone(),
            path: repo_path.clone(),
            interval_seconds: args.interval,
            enabled: !args.disabled,
            dry_run: args.dry_run,
        },
    );
    write_config(config_path, &config)?;

    println!("Wrote repository config:");
    println!("  name: {name}");
    println!("  path: {}", repo_path.display());
    println!("  config: {}", expand_tilde(config_path).display());
    println!("  interval_seconds: {}", args.interval);
    println!("  enabled: {}", !args.disabled);
    println!("  dry_run: {}", args.dry_run);
    Ok(())
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

fn run(config_path: &Path, run_args: RunArgs) -> Result<()> {
    let config = load_config(config_path)?;
    let (mut logger, verbose, mut repos) = build_managed_repos(config, &run_args)?;

    let keep_running = Arc::new(AtomicBool::new(true));
    let signal_flag = Arc::clone(&keep_running);
    ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    })
    .context("failed to install signal handlers")?;

    if run_args.once {
        for managed in &mut repos {
            if let Err(err) = sync_repo(&mut logger, managed, run_args.dry_run, verbose) {
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

            if let Err(err) = sync_repo(&mut logger, managed, run_args.dry_run, verbose) {
                logger.log("ERROR", &managed.config.name, err.to_string())?;
            }

            managed.next_run_at =
                Instant::now() + Duration::from_secs_f64(managed.config.interval_seconds);
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let Cli {
        config,
        run: run_args,
        command,
    } = cli;

    match command {
        Some(Commands::Init(args)) => init_config(&config, &args),
        None => run(&config, run_args),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, DefaultsConfig, RepositoryConfig, infer_repo_name, sanitize_repo_name,
        upsert_repository,
    };
    use std::path::PathBuf;

    #[test]
    fn sanitizes_repository_names() {
        assert_eq!(sanitize_repo_name("OpenClaw Code"), "openclaw-code");
        assert_eq!(sanitize_repo_name("repo__name"), "repo-name");
        assert_eq!(sanitize_repo_name("###"), "");
    }

    #[test]
    fn infers_repository_name_from_path() {
        let path = PathBuf::from("/tmp/OpenClaw Code");
        assert_eq!(
            infer_repo_name(&path).expect("should infer repository name"),
            "openclaw-code"
        );
    }

    #[test]
    fn upserts_repository_by_name() {
        let mut config = AppConfig {
            defaults: DefaultsConfig::default(),
            repositories: vec![RepositoryConfig {
                name: "openclawcode".into(),
                path: PathBuf::from("/old"),
                interval_seconds: 60.0,
                enabled: true,
                dry_run: false,
            }],
        };

        upsert_repository(
            &mut config,
            RepositoryConfig {
                name: "openclawcode".into(),
                path: PathBuf::from("/new"),
                interval_seconds: 30.0,
                enabled: true,
                dry_run: true,
            },
        );

        assert_eq!(config.repositories.len(), 1);
        assert_eq!(config.repositories[0].path, PathBuf::from("/new"));
        assert_eq!(config.repositories[0].interval_seconds, 30.0);
        assert!(config.repositories[0].dry_run);
    }
}
