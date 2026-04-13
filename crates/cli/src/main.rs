use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveTime};
use clap::{Args, Parser, Subcommand};
use repo_auto_puller_core::{RepoSyncer, Snapshot, SyncDecision, SyncReport};
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
    Status(StatusArgs),
    Dashboard(DashboardArgs),
    Pause(RepoToggleArgs),
    Resume(RepoToggleArgs),
    MigrateConfig,
    CheckConfig,
    Doctor(DoctorArgs),
    InstallService(InstallServiceArgs),
    UninstallService(UninstallServiceArgs),
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

#[derive(Debug, Args)]
struct StatusArgs {
    #[arg(long)]
    repo: Vec<String>,

    #[arg(long)]
    no_fetch: bool,

    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DashboardArgs {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: String,

    #[arg(long)]
    repo: Vec<String>,

    #[arg(long, default_value_t = 15)]
    refresh_seconds: u64,
}

#[derive(Debug, Args)]
struct RepoToggleArgs {
    #[arg(long)]
    repo: String,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long)]
    repo: Vec<String>,

    #[arg(long)]
    no_fetch: bool,

    #[arg(long, default_value = "repo-auto-puller")]
    service_name: String,
}

#[derive(Debug, Args)]
struct InstallServiceArgs {
    #[arg(long, default_value = "repo-auto-puller")]
    service_name: String,

    #[arg(long)]
    binary: Option<PathBuf>,

    #[arg(long)]
    repo: Vec<String>,

    #[arg(long)]
    enable: bool,

    #[arg(long)]
    start: bool,
}

#[derive(Debug, Args)]
struct UninstallServiceArgs {
    #[arg(long, default_value = "repo-auto-puller")]
    service_name: String,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AppConfig {
    #[serde(default = "default_config_version")]
    config_version: u32,
    #[serde(default)]
    defaults: DefaultsConfig,
    #[serde(default)]
    repositories: Vec<RepositoryConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DefaultsConfig {
    log_file: Option<PathBuf>,
    state_file: Option<PathBuf>,
    history_file: Option<PathBuf>,
    #[serde(default)]
    verbose: bool,
    desktop_notifications: Option<DesktopNotificationsConfig>,
    before_pull_command: Option<String>,
    after_pull_command: Option<String>,
    on_failure_command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DesktopNotificationsConfig {
    on_pull: Option<bool>,
    on_failure: Option<bool>,
    command: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QuietHoursConfig {
    start: String,
    end: String,
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
    paused: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    allowed_branches: Vec<String>,
    quiet_hours: Option<QuietHoursConfig>,
    desktop_notifications: Option<DesktopNotificationsConfig>,
    before_pull_command: Option<String>,
    after_pull_command: Option<String>,
    on_failure_command: Option<String>,
}

struct Logger {
    writer: Box<dyn Write + Send>,
}

struct ManagedRepo {
    config: RepositoryConfig,
    syncer: RepoSyncer,
    next_run_at: Instant,
    last_message: Option<String>,
    last_error: Option<String>,
    before_pull_command: Option<String>,
    after_pull_command: Option<String>,
    on_failure_command: Option<String>,
    notification_settings: NotificationSettings,
}

#[derive(Clone)]
struct SelectedRepo {
    config: RepositoryConfig,
    before_pull_command: Option<String>,
    after_pull_command: Option<String>,
    on_failure_command: Option<String>,
    notification_settings: NotificationSettings,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    repositories: Vec<StatusEntry>,
}

#[derive(Debug, Serialize)]
struct StatusEntry {
    name: String,
    path: String,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
    dirty: Option<bool>,
    decision: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct PersistedState {
    #[serde(default)]
    repositories: BTreeMap<String, PersistedRepoState>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedRepoState {
    name: String,
    path: String,
    updated_at: String,
    level: String,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
    dirty: Option<bool>,
    decision: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct HistoryRecord {
    recorded_at: String,
    name: String,
    path: String,
    level: String,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
    dirty: Option<bool>,
    decision: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

struct CommandProbe {
    ok: bool,
    detail: String,
}

struct EffectiveReport {
    decision: String,
    message: String,
    level: &'static str,
    blocks_auto_pull: bool,
}

#[derive(Clone, Debug)]
struct NotificationSettings {
    on_pull: bool,
    on_failure: bool,
    command: Option<String>,
}

struct StateStore {
    path: PathBuf,
    state: PersistedState,
}

struct HistoryStore {
    path: PathBuf,
}

const CURRENT_CONFIG_VERSION: u32 = 1;

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

impl StateStore {
    fn load(path: &Path) -> Result<Self> {
        let path = expand_tilde(path);
        let state = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read state file {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse state file {}", path.display()))?
        } else {
            PersistedState::default()
        };
        Ok(Self { path, state })
    }

    fn write(&self) -> Result<()> {
        ensure_parent_dir(&self.path)?;
        let json =
            serde_json::to_string_pretty(&self.state).context("failed to serialize state file")?;
        fs::write(&self.path, json)
            .with_context(|| format!("failed to write state file {}", self.path.display()))?;
        Ok(())
    }

    fn update_success(
        &mut self,
        managed: &ManagedRepo,
        snapshot: &Snapshot,
        effective: &EffectiveReport,
    ) -> Result<()> {
        self.state.repositories.insert(
            managed.config.name.clone(),
            PersistedRepoState {
                name: managed.config.name.clone(),
                path: managed.syncer.repo_path().display().to_string(),
                updated_at: current_timestamp(),
                level: effective.level.to_owned(),
                branch: Some(snapshot.branch.clone()),
                upstream: Some(snapshot.upstream.clone()),
                ahead: Some(snapshot.ahead),
                behind: Some(snapshot.behind),
                dirty: Some(snapshot.dirty),
                decision: Some(effective.decision.clone()),
                message: Some(effective.message.clone()),
                error: None,
            },
        );
        self.write()
    }

    fn update_error(&mut self, managed: &ManagedRepo, error: &str) -> Result<()> {
        self.state.repositories.insert(
            managed.config.name.clone(),
            PersistedRepoState {
                name: managed.config.name.clone(),
                path: managed.syncer.repo_path().display().to_string(),
                updated_at: current_timestamp(),
                level: "ERROR".to_owned(),
                branch: None,
                upstream: None,
                ahead: None,
                behind: None,
                dirty: None,
                decision: None,
                message: None,
                error: Some(error.to_owned()),
            },
        );
        self.write()
    }
}

impl HistoryStore {
    fn new(path: &Path) -> Self {
        Self {
            path: expand_tilde(path),
        }
    }

    fn append(&self, record: &HistoryRecord) -> Result<()> {
        ensure_parent_dir(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open history file {}", self.path.display()))?;
        serde_json::to_writer(&mut file, record).context("failed to serialize history record")?;
        writeln!(file).context("failed to append history newline")?;
        file.flush().context("failed to flush history file")?;
        Ok(())
    }

    fn append_success(
        &self,
        managed: &ManagedRepo,
        snapshot: &Snapshot,
        effective: &EffectiveReport,
    ) -> Result<()> {
        self.append(&HistoryRecord {
            recorded_at: current_timestamp(),
            name: managed.config.name.clone(),
            path: managed.syncer.repo_path().display().to_string(),
            level: effective.level.to_owned(),
            branch: Some(snapshot.branch.clone()),
            upstream: Some(snapshot.upstream.clone()),
            ahead: Some(snapshot.ahead),
            behind: Some(snapshot.behind),
            dirty: Some(snapshot.dirty),
            decision: Some(effective.decision.clone()),
            message: Some(effective.message.clone()),
            error: None,
        })
    }

    fn append_error(&self, managed: &ManagedRepo, error: &str) -> Result<()> {
        self.append(&HistoryRecord {
            recorded_at: current_timestamp(),
            name: managed.config.name.clone(),
            path: managed.syncer.repo_path().display().to_string(),
            level: "ERROR".to_owned(),
            branch: None,
            upstream: None,
            ahead: None,
            behind: None,
            dirty: None,
            decision: None,
            message: None,
            error: Some(error.to_owned()),
        })
    }
}

fn default_interval_seconds() -> f64 {
    60.0
}

fn default_config_version() -> u32 {
    CURRENT_CONFIG_VERSION
}

fn default_enabled() -> bool {
    true
}

fn current_timestamp() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

fn home_dir_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn default_log_file_path() -> PathBuf {
    match std::env::consts::OS {
        "windows" => home_dir_path()
            .map(|home| home.join("AppData/Local/repo-auto-puller/repo-auto-puller.log"))
            .unwrap_or_else(|| {
                PathBuf::from("~/AppData/Local/repo-auto-puller/repo-auto-puller.log")
            }),
        _ => PathBuf::from("~/.local/state/repo-auto-puller/repo-auto-puller.log"),
    }
}

fn default_state_file_path() -> PathBuf {
    match std::env::consts::OS {
        "windows" => home_dir_path()
            .map(|home| home.join("AppData/Local/repo-auto-puller/status.json"))
            .unwrap_or_else(|| PathBuf::from("~/AppData/Local/repo-auto-puller/status.json")),
        _ => PathBuf::from("~/.local/state/repo-auto-puller/status.json"),
    }
}

fn default_history_file_path() -> PathBuf {
    match std::env::consts::OS {
        "windows" => home_dir_path()
            .map(|home| home.join("AppData/Local/repo-auto-puller/history.jsonl"))
            .unwrap_or_else(|| PathBuf::from("~/AppData/Local/repo-auto-puller/history.jsonl")),
        _ => PathBuf::from("~/.local/state/repo-auto-puller/history.jsonl"),
    }
}

fn default_binary_path() -> PathBuf {
    match std::env::consts::OS {
        "windows" => home_dir_path()
            .map(|home| home.join("AppData/Local/repo-auto-puller/repo-auto-puller.exe"))
            .unwrap_or_else(|| {
                PathBuf::from("~/AppData/Local/repo-auto-puller/repo-auto-puller.exe")
            }),
        _ => PathBuf::from("~/.local/bin/repo-auto-puller"),
    }
}

fn load_config(path: &Path) -> Result<AppConfig> {
    let path = expand_tilde(path);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config: AppConfig =
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
    validate_config_schema(&config)?;
    Ok(config)
}

fn load_config_if_exists(path: &Path) -> Result<Option<AppConfig>> {
    let path = expand_tilde(path);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(load_config(&path)?))
}

fn validate_config_schema(config: &AppConfig) -> Result<()> {
    if config.config_version > CURRENT_CONFIG_VERSION {
        bail!(
            "config_version {} is newer than this binary supports ({CURRENT_CONFIG_VERSION})",
            config.config_version
        );
    }

    if config.repositories.is_empty() {
        bail!("config has no repositories");
    }

    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for repo in &config.repositories {
        if repo.name.trim().is_empty() {
            bail!("repository names must not be empty");
        }
        if !names.insert(repo.name.clone()) {
            bail!("duplicate repository name: {}", repo.name);
        }
        if repo.interval_seconds <= 0.0 {
            bail!("repository {} has invalid interval_seconds", repo.name);
        }
        let mut branches = BTreeSet::new();
        for branch in &repo.allowed_branches {
            if branch.trim().is_empty() {
                bail!(
                    "repository {} has an empty allowed_branches entry",
                    repo.name
                );
            }
            if !branches.insert(branch.clone()) {
                bail!(
                    "repository {} has duplicate allowed_branches entry: {}",
                    repo.name,
                    branch
                );
            }
        }
        if let Some(quiet_hours) = &repo.quiet_hours {
            let start = parse_quiet_time(&quiet_hours.start).with_context(|| {
                format!(
                    "repository {} has invalid quiet_hours.start {}",
                    repo.name, quiet_hours.start
                )
            })?;
            let end = parse_quiet_time(&quiet_hours.end).with_context(|| {
                format!(
                    "repository {} has invalid quiet_hours.end {}",
                    repo.name, quiet_hours.end
                )
            })?;
            if start == end {
                bail!(
                    "repository {} has quiet_hours with identical start and end",
                    repo.name
                );
            }
        }
        let raw_path = repo.path.to_string_lossy().to_string();
        if !paths.insert(raw_path.clone()) {
            bail!("duplicate repository path: {raw_path}");
        }
    }

    Ok(())
}

fn build_logger(config: &AppConfig) -> Result<Logger> {
    match config.defaults.log_file.as_deref() {
        Some(path) => Logger::file(&expand_tilde(path)),
        None => Ok(Logger::stdout()),
    }
}

fn build_state_store(config: &AppConfig) -> Result<StateStore> {
    let path = config
        .defaults
        .state_file
        .clone()
        .unwrap_or_else(default_state_file_path);
    StateStore::load(&path)
}

fn build_history_store(config: &AppConfig) -> HistoryStore {
    let path = config
        .defaults
        .history_file
        .clone()
        .unwrap_or_else(default_history_file_path);
    HistoryStore::new(&path)
}

fn merge_notification_settings(
    defaults: Option<&DesktopNotificationsConfig>,
    repo: Option<&DesktopNotificationsConfig>,
) -> NotificationSettings {
    NotificationSettings {
        on_pull: repo
            .and_then(|config| config.on_pull)
            .or_else(|| defaults.and_then(|config| config.on_pull))
            .unwrap_or(false),
        on_failure: repo
            .and_then(|config| config.on_failure)
            .or_else(|| defaults.and_then(|config| config.on_failure))
            .unwrap_or(false),
        command: repo
            .and_then(|config| config.command.clone())
            .or_else(|| defaults.and_then(|config| config.command.clone())),
    }
}

fn selected_repositories(config: &AppConfig, selected: &[String]) -> Result<Vec<SelectedRepo>> {
    let mut repos = Vec::new();

    for repo in &config.repositories {
        if !repo.enabled {
            continue;
        }
        if !selected.is_empty() && !selected.iter().any(|name| name == &repo.name) {
            continue;
        }

        repos.push(SelectedRepo {
            config: repo.clone(),
            before_pull_command: repo
                .before_pull_command
                .clone()
                .or_else(|| config.defaults.before_pull_command.clone()),
            after_pull_command: repo
                .after_pull_command
                .clone()
                .or_else(|| config.defaults.after_pull_command.clone()),
            on_failure_command: repo
                .on_failure_command
                .clone()
                .or_else(|| config.defaults.on_failure_command.clone()),
            notification_settings: merge_notification_settings(
                config.defaults.desktop_notifications.as_ref(),
                repo.desktop_notifications.as_ref(),
            ),
        });
    }

    if repos.is_empty() {
        if selected.is_empty() {
            bail!("no enabled repositories found in config");
        }
        bail!(
            "no repositories matched the requested names: {}",
            selected.join(", ")
        );
    }

    Ok(repos)
}

fn build_managed_repos(
    config: AppConfig,
    run_args: &RunArgs,
) -> Result<(Logger, StateStore, HistoryStore, bool, Vec<ManagedRepo>)> {
    let logger = build_logger(&config)?;
    let state_store = build_state_store(&config)?;
    let history_store = build_history_store(&config);
    let verbose = run_args.verbose || config.defaults.verbose;
    let repos = selected_repositories(&config, &run_args.repo)?;
    let mut managed = Vec::with_capacity(repos.len());

    for repo in repos {
        let syncer = RepoSyncer::new(expand_tilde(&repo.config.path))
            .with_context(|| format!("failed to initialize repository {}", repo.config.name))?;
        managed.push(ManagedRepo {
            config: repo.config,
            syncer,
            next_run_at: Instant::now(),
            last_message: None,
            last_error: None,
            before_pull_command: repo.before_pull_command,
            after_pull_command: repo.after_pull_command,
            on_failure_command: repo.on_failure_command,
            notification_settings: repo.notification_settings,
        });
    }

    Ok((logger, state_store, history_store, verbose, managed))
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let home = home_dir_path();
    if raw == "~"
        && let Some(home) = home.clone()
    {
        return home;
    }
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(stripped);
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

fn set_repository_paused(config_path: &Path, repo_name: &str, paused: bool) -> Result<()> {
    let mut config = load_config(config_path)?;
    let repo = config
        .repositories
        .iter_mut()
        .find(|repo| repo.name == repo_name)
        .with_context(|| format!("repository not found in config: {repo_name}"))?;
    repo.paused = paused;
    write_config(config_path, &config)?;
    println!(
        "{} repository {}",
        if paused { "Paused" } else { "Resumed" },
        repo_name
    );
    Ok(())
}

fn migrate_config(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;
    config.config_version = CURRENT_CONFIG_VERSION;
    write_config(config_path, &config)?;
    println!(
        "Migrated config to version {} in {}",
        CURRENT_CONFIG_VERSION,
        expand_tilde(config_path).display()
    );
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
        config_version: CURRENT_CONFIG_VERSION,
        defaults: DefaultsConfig {
            log_file: Some(default_log_file_path()),
            state_file: Some(default_state_file_path()),
            history_file: Some(default_history_file_path()),
            verbose: false,
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
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
            paused: false,
            dry_run: args.dry_run,
            allowed_branches: Vec::new(),
            quiet_hours: None,
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        },
    );
    write_config(config_path, &config)?;

    println!("Wrote repository config:");
    println!("  name: {name}");
    println!("  path: {}", repo_path.display());
    println!("  config: {}", expand_tilde(config_path).display());
    println!("  interval_seconds: {}", args.interval);
    println!("  enabled: {}", !args.disabled);
    println!("  paused: false");
    println!("  dry_run: {}", args.dry_run);
    println!("  allowed_branches: []");
    println!("  quiet_hours: none");
    Ok(())
}

fn probe_repository(syncer: &RepoSyncer, fetch_remote: bool) -> Result<(Snapshot, SyncReport)> {
    let context = syncer.read_context()?;
    if fetch_remote {
        syncer.fetch_ref(&context.remote, &context.remote_branch)?;
    }
    let snapshot = syncer.read_snapshot()?;
    let report = RepoSyncer::report(&snapshot);
    Ok((snapshot, report))
}

fn branch_allowed(config: &RepositoryConfig, branch: &str) -> bool {
    config.allowed_branches.is_empty() || config.allowed_branches.iter().any(|item| item == branch)
}

fn parse_quiet_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .with_context(|| format!("expected HH:MM, got {value}"))
}

fn in_quiet_hours(config: &RepositoryConfig, now: NaiveTime) -> Result<Option<String>> {
    let Some(quiet_hours) = &config.quiet_hours else {
        return Ok(None);
    };

    let start = parse_quiet_time(&quiet_hours.start)?;
    let end = parse_quiet_time(&quiet_hours.end)?;
    let in_window = if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    };

    if in_window {
        Ok(Some(format!("{}-{}", quiet_hours.start, quiet_hours.end)))
    } else {
        Ok(None)
    }
}

fn effective_report(
    config: &RepositoryConfig,
    snapshot: &Snapshot,
    report: &SyncReport,
) -> EffectiveReport {
    if config.paused {
        return EffectiveReport {
            decision: "paused".to_owned(),
            message: format!("{} is paused in config; skipping auto-pull", config.name),
            level: "WARN",
            blocks_auto_pull: true,
        };
    }

    if let Ok(Some(window)) = in_quiet_hours(config, Local::now().time()) {
        return EffectiveReport {
            decision: "quiet-hours".to_owned(),
            message: format!(
                "{} is inside quiet_hours {}; skipping auto-pull",
                config.name, window
            ),
            level: "WARN",
            blocks_auto_pull: true,
        };
    }

    if !branch_allowed(config, &snapshot.branch) {
        return EffectiveReport {
            decision: "branch-not-allowed".to_owned(),
            message: format!(
                "{} is not in allowed_branches [{}]; skipping auto-pull",
                snapshot.branch,
                config.allowed_branches.join(", ")
            ),
            level: "WARN",
            blocks_auto_pull: true,
        };
    }

    EffectiveReport {
        decision: report.decision.as_str().to_owned(),
        message: report.message.clone(),
        level: report.level,
        blocks_auto_pull: decision_blocks_auto_pull(&report.decision),
    }
}

fn escape_applescript_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run_desktop_notification(
    settings: &NotificationSettings,
    event: &str,
    repo_name: &str,
    repo_path: &Path,
    title: &str,
    message: &str,
) -> Result<()> {
    if let Some(command) = settings.command.as_deref() {
        let output = Command::new("sh")
            .arg("-lc")
            .arg(command)
            .env("REPO_AUTO_PULLER_EVENT", event)
            .env("REPO_AUTO_PULLER_REPO_NAME", repo_name)
            .env(
                "REPO_AUTO_PULLER_REPO_PATH",
                repo_path.display().to_string(),
            )
            .env("REPO_AUTO_PULLER_NOTIFICATION_TITLE", title)
            .env("REPO_AUTO_PULLER_NOTIFICATION_BODY", message)
            .output()
            .with_context(|| format!("failed to execute notification command for {repo_name}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.is_empty() {
                bail!("notification command exited unsuccessfully");
            }
            bail!("notification command exited unsuccessfully: {stderr}");
        }
        return Ok(());
    }

    match std::env::consts::OS {
        "linux" => {
            let output = Command::new("notify-send")
                .arg(title)
                .arg(message)
                .output()
                .context("failed to execute notify-send")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                if stderr.is_empty() {
                    bail!("notify-send exited unsuccessfully");
                }
                bail!("notify-send exited unsuccessfully: {stderr}");
            }
            Ok(())
        }
        "macos" => {
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                escape_applescript_text(message),
                escape_applescript_text(title)
            );
            let output = Command::new("osascript")
                .args(["-e", &script])
                .output()
                .context("failed to execute osascript")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                if stderr.is_empty() {
                    bail!("osascript exited unsuccessfully");
                }
                bail!("osascript exited unsuccessfully: {stderr}");
            }
            Ok(())
        }
        other => bail!("desktop notifications are not supported on this operating system: {other}"),
    }
}

fn maybe_send_desktop_notification(
    logger: &mut Logger,
    managed: &ManagedRepo,
    event: &str,
    title: &str,
    message: &str,
) -> Result<()> {
    let enabled = match event {
        "pull" => managed.notification_settings.on_pull,
        "failure" => managed.notification_settings.on_failure,
        _ => false,
    };
    if !enabled {
        return Ok(());
    }

    if let Err(err) = run_desktop_notification(
        &managed.notification_settings,
        event,
        &managed.config.name,
        managed.syncer.repo_path(),
        title,
        message,
    ) {
        logger.log(
            "WARN",
            &managed.config.name,
            format!("desktop notification failed: {err}"),
        )?;
    }

    Ok(())
}

fn quote_systemd_arg(arg: &str) -> String {
    if arg.contains(char::is_whitespace) || arg.contains('"') || arg.contains('\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg.to_owned()
    }
}

fn build_program_args(binary: &Path, config: &Path, repos: &[String]) -> Vec<String> {
    let mut program_args = vec![
        binary.display().to_string(),
        "--config".to_owned(),
        config.display().to_string(),
    ];
    for repo in repos {
        program_args.push("--repo".to_owned());
        program_args.push(repo.clone());
    }
    program_args
}

fn render_systemd_service(program_args: &[String]) -> String {
    let exec_start = program_args
        .iter()
        .map(|arg| quote_systemd_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=Repo Auto Puller\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={exec_start}\nRestart=always\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_launchd_plist(label: &str, program_args: &[String]) -> String {
    let args = program_args
        .iter()
        .map(|arg| format!("    <string>{}</string>", escape_xml(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
{}
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#,
        escape_xml(label),
        args
    )
}

fn command_probe(program: &str, args: &[&str], display_name: &str) -> CommandProbe {
    match Command::new(program).args(args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if output.status.success() {
                CommandProbe {
                    ok: true,
                    detail: if stdout.is_empty() {
                        "ok".to_owned()
                    } else {
                        stdout
                    },
                }
            } else {
                let detail = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("{display_name} exited with status {}", output.status)
                };
                CommandProbe { ok: false, detail }
            }
        }
        Err(err) => CommandProbe {
            ok: false,
            detail: format!("failed to execute {display_name}: {err}"),
        },
    }
}

fn print_doctor_check(label: &str, ok: bool, detail: impl AsRef<str>) {
    let status = if ok { "ok" } else { "issue" };
    println!("  {label}: {status} ({})", detail.as_ref());
}

fn decision_blocks_auto_pull(decision: &SyncDecision) -> bool {
    matches!(
        decision,
        SyncDecision::Diverged | SyncDecision::DirtyBehind | SyncDecision::AheadOnly
    )
}

fn systemd_unit_path(service_name: &str) -> PathBuf {
    expand_tilde(&PathBuf::from(format!(
        "~/.config/systemd/user/{service_name}.service"
    )))
}

fn launchd_plist_path(service_name: &str) -> PathBuf {
    expand_tilde(&PathBuf::from(format!(
        "~/Library/LaunchAgents/{service_name}.plist"
    )))
}

fn windows_task_script_path(service_name: &str) -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| home_dir_path().map(|home| home.join("AppData/Roaming")));
    match base {
        Some(base) => base
            .join("repo-auto-puller")
            .join(format!("{service_name}.cmd")),
        None => PathBuf::from(format!(
            "~/AppData/Roaming/repo-auto-puller/{service_name}.cmd"
        )),
    }
}

fn quote_windows_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_owned();
    }
    if arg.contains(char::is_whitespace) || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\"\""))
    } else {
        arg.to_owned()
    }
}

fn render_windows_command_script(program_args: &[String]) -> String {
    let command = program_args
        .iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!("@echo off\r\n{command}\r\n")
}

fn install_systemd_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let unit_path = systemd_unit_path(&service_name);
    ensure_parent_dir(&unit_path)?;

    let binary = service_binary_path(args.binary.as_deref());
    let config = expand_tilde(config_path);
    let program_args = build_program_args(&binary, &config, &args.repo);
    let service = render_systemd_service(&program_args);
    fs::write(&unit_path, service)
        .with_context(|| format!("failed to write {}", unit_path.display()))?;

    println!("Wrote systemd service file: {}", unit_path.display());

    if args.enable || args.start {
        run_systemctl_user(["daemon-reload"])?;
    }
    if args.enable {
        run_systemctl_user(["enable", &format!("{service_name}.service")])?;
    }
    if args.start {
        run_systemctl_user(["start", &format!("{service_name}.service")])?;
    }

    Ok(())
}

fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to execute id -u")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("id -u failed: {stderr}");
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if uid.is_empty() {
        bail!("id -u returned an empty uid");
    }
    Ok(format!("gui/{uid}"))
}

fn run_launchctl(args: &[&str], context: &str) -> Result<()> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute launchctl {context}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("launchctl {context} failed: {stderr}");
    }
    Ok(())
}

fn service_binary_path(binary: Option<&Path>) -> PathBuf {
    binary
        .map(expand_tilde)
        .unwrap_or_else(|| expand_tilde(&default_binary_path()))
}

fn run_schtasks(args: &[&str], context: &str) -> Result<()> {
    let output = Command::new("schtasks")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute schtasks {context}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("schtasks {context} exited with status {}", output.status)
        };
        bail!("schtasks {context} failed: {detail}");
    }
    Ok(())
}

fn warn_if_command_fails(
    program: &str,
    args: &[&str],
    display_name: &str,
    context: &str,
) -> Result<()> {
    let output = match Command::new(program).args(args).output() {
        Ok(output) => output,
        Err(err) => {
            eprintln!("warning: failed to execute {display_name} {context}: {err}");
            return Ok(());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let suffix = if stderr.is_empty() {
            "command returned non-zero exit status".to_owned()
        } else {
            stderr
        };
        eprintln!("warning: {display_name} {context} failed: {suffix}");
    }

    Ok(())
}

fn install_launchd_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let plist_path = launchd_plist_path(&service_name);
    ensure_parent_dir(&plist_path)?;

    let binary = service_binary_path(args.binary.as_deref());
    let config = expand_tilde(config_path);
    let program_args = build_program_args(&binary, &config, &args.repo);
    let plist = render_launchd_plist(&service_name, &program_args);
    fs::write(&plist_path, plist)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;

    println!("Wrote launchd plist: {}", plist_path.display());

    if args.enable || args.start {
        let domain = launchd_domain()?;
        let plist_path_string = plist_path.display().to_string();
        let service_target = format!("{domain}/{service_name}");
        let _ = Command::new("launchctl")
            .args(["bootout", &service_target])
            .output();
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist_path_string])
            .output();
        run_launchctl(
            &["bootstrap", &domain, &plist_path_string],
            &format!("bootstrap {domain} {plist_path_string}"),
        )?;
        if args.enable {
            run_launchctl(
                &["enable", &service_target],
                &format!("enable {service_target}"),
            )?;
        }
        if args.start {
            run_launchctl(
                &["kickstart", "-k", &service_target],
                &format!("kickstart -k {service_target}"),
            )?;
        }
    }

    Ok(())
}

fn install_windows_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let script_path = windows_task_script_path(&service_name);
    ensure_parent_dir(&script_path)?;

    let binary = service_binary_path(args.binary.as_deref());
    let config = expand_tilde(config_path);
    let program_args = build_program_args(&binary, &config, &args.repo);
    let script = render_windows_command_script(&program_args);
    fs::write(&script_path, script)
        .with_context(|| format!("failed to write {}", script_path.display()))?;

    println!("Wrote Windows task launcher: {}", script_path.display());

    let script_path_string = script_path.display().to_string();
    run_schtasks(
        &[
            "/Create",
            "/TN",
            &service_name,
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/TR",
            &script_path_string,
            "/F",
        ],
        &format!("create task {service_name}"),
    )?;

    if args.enable {
        run_schtasks(
            &["/Change", "/TN", &service_name, "/ENABLE"],
            &format!("enable task {service_name}"),
        )?;
    } else {
        run_schtasks(
            &["/Change", "/TN", &service_name, "/DISABLE"],
            &format!("disable task {service_name}"),
        )?;
    }

    if args.start {
        run_schtasks(
            &["/Run", "/TN", &service_name],
            &format!("run task {service_name}"),
        )?;
    }

    Ok(())
}

fn uninstall_systemd_service(args: &UninstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let unit_name = format!("{service_name}.service");
    let unit_path = systemd_unit_path(&service_name);

    warn_if_command_fails(
        "systemctl",
        &["--user", "stop", &unit_name],
        "systemctl --user",
        &format!("stop {unit_name}"),
    )?;
    warn_if_command_fails(
        "systemctl",
        &["--user", "disable", &unit_name],
        "systemctl --user",
        &format!("disable {unit_name}"),
    )?;

    if unit_path.exists() {
        fs::remove_file(&unit_path)
            .with_context(|| format!("failed to remove {}", unit_path.display()))?;
        println!("Removed systemd service file: {}", unit_path.display());
    } else {
        println!("No systemd service file found at {}", unit_path.display());
    }

    warn_if_command_fails(
        "systemctl",
        &["--user", "daemon-reload"],
        "systemctl --user",
        "daemon-reload",
    )?;
    warn_if_command_fails(
        "systemctl",
        &["--user", "reset-failed", &unit_name],
        "systemctl --user",
        &format!("reset-failed {unit_name}"),
    )?;

    Ok(())
}

fn uninstall_launchd_service(args: &UninstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let plist_path = launchd_plist_path(&service_name);
    let domain = launchd_domain()?;
    let service_target = format!("{domain}/{service_name}");
    let plist_path_string = plist_path.display().to_string();

    warn_if_command_fails(
        "launchctl",
        &["bootout", &service_target],
        "launchctl",
        &format!("bootout {service_target}"),
    )?;
    warn_if_command_fails(
        "launchctl",
        &["bootout", &domain, &plist_path_string],
        "launchctl",
        &format!("bootout {domain} {plist_path_string}"),
    )?;
    warn_if_command_fails(
        "launchctl",
        &["disable", &service_target],
        "launchctl",
        &format!("disable {service_target}"),
    )?;

    if plist_path.exists() {
        fs::remove_file(&plist_path)
            .with_context(|| format!("failed to remove {}", plist_path.display()))?;
        println!("Removed launchd plist: {}", plist_path.display());
    } else {
        println!("No launchd plist found at {}", plist_path.display());
    }

    Ok(())
}

fn uninstall_windows_service(args: &UninstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let script_path = windows_task_script_path(&service_name);

    warn_if_command_fails(
        "schtasks",
        &["/Delete", "/TN", &service_name, "/F"],
        "schtasks",
        &format!("delete task {service_name}"),
    )?;

    if script_path.exists() {
        fs::remove_file(&script_path)
            .with_context(|| format!("failed to remove {}", script_path.display()))?;
        println!("Removed Windows task launcher: {}", script_path.display());
    } else {
        println!(
            "No Windows task launcher found at {}",
            script_path.display()
        );
    }

    Ok(())
}

fn install_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    if args.service_name.trim().is_empty() {
        bail!("--service-name must not be empty");
    }

    match std::env::consts::OS {
        "linux" => install_systemd_service(config_path, args),
        "macos" => install_launchd_service(config_path, args),
        "windows" => install_windows_service(config_path, args),
        other => {
            bail!("install-service is not supported on this operating system: {other}")
        }
    }
}

fn uninstall_service(args: &UninstallServiceArgs) -> Result<()> {
    if args.service_name.trim().is_empty() {
        bail!("--service-name must not be empty");
    }

    match std::env::consts::OS {
        "linux" => uninstall_systemd_service(args),
        "macos" => uninstall_launchd_service(args),
        "windows" => uninstall_windows_service(args),
        other => {
            bail!("uninstall-service is not supported on this operating system: {other}")
        }
    }
}

fn run_systemctl_user<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute systemctl --user {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("systemctl --user {} failed: {}", args.join(" "), stderr);
    }
    Ok(())
}

fn run_failure_hook(logger: &mut Logger, managed: &ManagedRepo, error: &str) -> Result<()> {
    let Some(command) = managed.on_failure_command.as_deref() else {
        return Ok(());
    };

    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .env("REPO_AUTO_PULLER_EVENT", "sync_error")
        .env("REPO_AUTO_PULLER_REPO_NAME", &managed.config.name)
        .env(
            "REPO_AUTO_PULLER_REPO_PATH",
            managed.syncer.repo_path().display().to_string(),
        )
        .env("REPO_AUTO_PULLER_ERROR", error)
        .output()
        .with_context(|| format!("failed to execute failure hook for {}", managed.config.name))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        logger.log(
            "WARN",
            &managed.config.name,
            format!("failure hook exited unsuccessfully: {stderr}"),
        )?;
    }

    Ok(())
}

fn run_pull_hook(
    logger: &mut Logger,
    managed: &ManagedRepo,
    event: &str,
    command: Option<&str>,
    snapshot: &Snapshot,
    strict: bool,
) -> Result<()> {
    let Some(command) = command else {
        return Ok(());
    };

    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .env("REPO_AUTO_PULLER_EVENT", event)
        .env("REPO_AUTO_PULLER_REPO_NAME", &managed.config.name)
        .env(
            "REPO_AUTO_PULLER_REPO_PATH",
            managed.syncer.repo_path().display().to_string(),
        )
        .env("REPO_AUTO_PULLER_BRANCH", &snapshot.branch)
        .env("REPO_AUTO_PULLER_UPSTREAM", &snapshot.upstream)
        .env("REPO_AUTO_PULLER_REMOTE", &snapshot.remote)
        .env("REPO_AUTO_PULLER_REMOTE_BRANCH", &snapshot.remote_branch)
        .output()
        .with_context(|| format!("failed to execute {event} hook for {}", managed.config.name))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let message = if stderr.is_empty() {
        format!("{event} hook exited unsuccessfully")
    } else {
        format!("{event} hook exited unsuccessfully: {stderr}")
    };

    if strict {
        bail!(message);
    }

    logger.log("WARN", &managed.config.name, message)?;
    Ok(())
}

fn sync_repo(
    logger: &mut Logger,
    state_store: &mut StateStore,
    history_store: &HistoryStore,
    managed: &mut ManagedRepo,
    global_dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let (snapshot, report) = probe_repository(&managed.syncer, true)?;
    let effective = effective_report(&managed.config, &snapshot, &report);

    if verbose
        || effective.level != "IDLE"
        || managed.last_message.as_deref() != Some(effective.message.as_str())
    {
        logger.log(effective.level, &managed.config.name, &effective.message)?;
        managed.last_message = Some(effective.message.clone());
    }
    if effective.blocks_auto_pull {
        state_store.update_success(managed, &snapshot, &effective)?;
        history_store.append_success(managed, &snapshot, &effective)?;
        managed.last_error = None;
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
        state_store.update_success(managed, &snapshot, &effective)?;
        history_store.append_success(managed, &snapshot, &effective)?;
        managed.last_error = None;
        return Ok(());
    }

    run_pull_hook(
        logger,
        managed,
        "before_pull",
        managed.before_pull_command.as_deref(),
        &snapshot,
        true,
    )?;

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
    maybe_send_desktop_notification(
        logger,
        managed,
        "pull",
        "repo-auto-puller",
        &format!("{} updated successfully", managed.config.name),
    )?;
    run_pull_hook(
        logger,
        managed,
        "after_pull",
        managed.after_pull_command.as_deref(),
        &snapshot,
        false,
    )?;
    let (post_snapshot, post_report) = probe_repository(&managed.syncer, false)?;
    let post_effective = effective_report(&managed.config, &post_snapshot, &post_report);
    state_store.update_success(managed, &post_snapshot, &post_effective)?;
    history_store.append_success(managed, &post_snapshot, &post_effective)?;
    managed.last_error = None;
    Ok(())
}

fn handle_sync_error(
    logger: &mut Logger,
    state_store: &mut StateStore,
    history_store: &HistoryStore,
    managed: &mut ManagedRepo,
    err: anyhow::Error,
) -> Result<()> {
    let error = err.to_string();
    logger.log("ERROR", &managed.config.name, &error)?;
    state_store.update_error(managed, &error)?;
    history_store.append_error(managed, &error)?;
    if managed.last_error.as_deref() != Some(error.as_str()) {
        maybe_send_desktop_notification(
            logger,
            managed,
            "failure",
            "repo-auto-puller",
            &format!("{}: {}", managed.config.name, error),
        )?;
        run_failure_hook(logger, managed, &error)?;
    }
    managed.last_error = Some(error);
    Ok(())
}

fn render_status(config_path: &Path, args: &StatusArgs) -> Result<()> {
    let config = load_config(config_path)?;
    let repos = selected_repositories(&config, &args.repo)?;
    let mut output = Vec::with_capacity(repos.len());

    for repo in repos {
        let syncer = RepoSyncer::new(expand_tilde(&repo.config.path))
            .with_context(|| format!("failed to initialize repository {}", repo.config.name))?;
        let name = repo.config.name.clone();
        let path = syncer.repo_path().display().to_string();

        match probe_repository(&syncer, !args.no_fetch) {
            Ok((snapshot, report)) => {
                let effective = effective_report(&repo.config, &snapshot, &report);
                if args.json {
                    output.push(StatusEntry {
                        name,
                        path,
                        branch: Some(snapshot.branch),
                        upstream: Some(snapshot.upstream),
                        ahead: Some(snapshot.ahead),
                        behind: Some(snapshot.behind),
                        dirty: Some(snapshot.dirty),
                        decision: Some(effective.decision),
                        message: Some(effective.message),
                        error: None,
                    });
                } else {
                    println!("Repository: {name}");
                    println!("  path: {path}");
                    println!("  branch: {}", snapshot.branch);
                    println!("  upstream: {}", snapshot.upstream);
                    println!("  ahead: {}", snapshot.ahead);
                    println!("  behind: {}", snapshot.behind);
                    println!("  dirty: {}", snapshot.dirty);
                    println!("  decision: {}", effective.decision);
                    println!("  message: {}", effective.message);
                }
            }
            Err(err) => {
                if args.json {
                    output.push(StatusEntry {
                        name,
                        path,
                        branch: None,
                        upstream: None,
                        ahead: None,
                        behind: None,
                        dirty: None,
                        decision: None,
                        message: None,
                        error: Some(err.to_string()),
                    });
                } else {
                    println!("Repository: {name}");
                    println!("  path: {path}");
                    println!("  error: {err}");
                }
            }
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&StatusOutput {
                repositories: output,
            })
            .context("failed to serialize status output")?
        );
    }

    Ok(())
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn url_decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

fn repo_name_from_path(path: &str, prefix: &str) -> Option<String> {
    path.strip_prefix(prefix)
        .map(url_decode_component)
        .filter(|value| !value.is_empty())
}

fn render_dashboard_html(
    selected: &[SelectedRepo],
    state: &PersistedState,
    refresh_seconds: u64,
) -> String {
    let mut cards = String::new();

    for repo in selected {
        let persisted = state.repositories.get(&repo.config.name);
        let status_class = match persisted.map(|entry| entry.level.as_str()) {
            Some("ERROR") => "error",
            Some("WARN") => "warn",
            Some("INFO") => "info",
            Some("IDLE") => "idle",
            _ => "unknown",
        };
        let summary = match persisted {
            Some(entry) => entry
                .error
                .as_deref()
                .or(entry.message.as_deref())
                .unwrap_or("No details recorded yet"),
            None => "No sync recorded yet",
        };
        let branch = persisted
            .and_then(|entry| entry.branch.as_deref())
            .unwrap_or("-");
        let upstream = persisted
            .and_then(|entry| entry.upstream.as_deref())
            .unwrap_or("-");
        let ahead = persisted
            .and_then(|entry| entry.ahead)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let behind = persisted
            .and_then(|entry| entry.behind)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let dirty = persisted
            .and_then(|entry| entry.dirty)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let updated_at = persisted
            .map(|entry| entry.updated_at.as_str())
            .unwrap_or("Never");
        let action_path = if repo.config.paused {
            format!("/resume/{}", url_encode_component(&repo.config.name))
        } else {
            format!("/pause/{}", url_encode_component(&repo.config.name))
        };
        let action_label = if repo.config.paused {
            "Resume Auto Pull"
        } else {
            "Pause Auto Pull"
        };

        cards.push_str(&format!(
            r#"<section class="card {status_class}">
<h2>{name}</h2>
<p class="summary">{summary}</p>
<dl>
<div><dt>Path</dt><dd>{path}</dd></div>
<div><dt>Last Sync</dt><dd>{updated_at}</dd></div>
<div><dt>Branch</dt><dd>{branch}</dd></div>
<div><dt>Upstream</dt><dd>{upstream}</dd></div>
<div><dt>Ahead</dt><dd>{ahead}</dd></div>
<div><dt>Behind</dt><dd>{behind}</dd></div>
<div><dt>Dirty</dt><dd>{dirty}</dd></div>
</dl>
<a class="action" href="{action_path}">{action_label}</a>
</section>
"#,
            status_class = status_class,
            name = escape_html(&repo.config.name),
            summary = escape_html(summary),
            path = escape_html(&expand_tilde(&repo.config.path).display().to_string()),
            updated_at = escape_html(updated_at),
            branch = escape_html(branch),
            upstream = escape_html(upstream),
            ahead = escape_html(&ahead),
            behind = escape_html(&behind),
            dirty = escape_html(&dirty),
            action_path = escape_html(&action_path),
            action_label = action_label,
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="{refresh_seconds}">
<title>Repo Auto Puller Dashboard</title>
<style>
:root {{
  color-scheme: light;
  --bg: #f4efe7;
  --panel: rgba(255, 252, 246, 0.92);
  --text: #1e1c18;
  --muted: #6f685f;
  --border: rgba(40, 34, 24, 0.12);
  --error: #a11d33;
  --warn: #b96a00;
  --info: #155e75;
  --idle: #1f6f43;
}}
body {{
  margin: 0;
  font-family: "Georgia", "Times New Roman", serif;
  background:
    radial-gradient(circle at top left, rgba(196, 120, 72, 0.16), transparent 38%),
    linear-gradient(135deg, #efe7da, var(--bg));
  color: var(--text);
}}
main {{
  max-width: 1100px;
  margin: 0 auto;
  padding: 40px 20px 60px;
}}
h1 {{
  margin: 0 0 10px;
  font-size: clamp(2rem, 4vw, 3.4rem);
}}
.lead {{
  margin: 0 0 30px;
  color: var(--muted);
  max-width: 60ch;
}}
.grid {{
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 18px;
}}
.card {{
  border: 1px solid var(--border);
  border-left-width: 6px;
  border-radius: 18px;
  padding: 18px;
  background: var(--panel);
  box-shadow: 0 18px 45px rgba(33, 27, 18, 0.08);
  backdrop-filter: blur(8px);
}}
.card.error {{ border-left-color: var(--error); }}
.card.warn {{ border-left-color: var(--warn); }}
.card.info {{ border-left-color: var(--info); }}
.card.idle {{ border-left-color: var(--idle); }}
.card.unknown {{ border-left-color: var(--muted); }}
.card h2 {{
  margin: 0 0 10px;
  font-size: 1.35rem;
}}
.action {{
  display: inline-block;
  margin-top: 16px;
  padding: 10px 14px;
  border-radius: 999px;
  text-decoration: none;
  color: white;
  background: linear-gradient(135deg, #1e1c18, #6a4b2e);
}}
.summary {{
  margin: 0 0 16px;
  color: var(--muted);
  min-height: 3em;
}}
dl {{
  margin: 0;
  display: grid;
  gap: 10px;
}}
dl div {{
  display: grid;
  gap: 4px;
}}
dt {{
  font-size: 0.82rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}}
dd {{
  margin: 0;
  word-break: break-word;
}}
</style>
</head>
<body>
<main>
<h1>Repo Auto Puller</h1>
<p class="lead">Read-only dashboard for recent sync state. Refreshes every {refresh_seconds} seconds.</p>
<div class="grid">
{cards}</div>
</main>
</body>
</html>
"#,
        refresh_seconds = refresh_seconds.max(5),
        cards = cards,
    )
}

fn serve_dashboard(config_path: &Path, args: &DashboardArgs) -> Result<()> {
    let listener = TcpListener::bind(&args.listen)
        .with_context(|| format!("failed to bind dashboard listener on {}", args.listen))?;

    println!("Dashboard listening on http://{}", args.listen);

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("warning: failed to accept dashboard connection: {err}");
                continue;
            }
        };

        let mut buffer = [0_u8; 1024];
        let bytes_read = stream.read(&mut buffer).unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..bytes_read]);
        let request_line = request.lines().next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .to_owned();
        let (status_line, extra_headers, body) =
            if let Some(repo_name) = repo_name_from_path(&path, "/pause/") {
                set_repository_paused(config_path, &repo_name, true)?;
                ("HTTP/1.1 303 See Other", "Location: /\r\n", String::new())
            } else if let Some(repo_name) = repo_name_from_path(&path, "/resume/") {
                set_repository_paused(config_path, &repo_name, false)?;
                ("HTTP/1.1 303 See Other", "Location: /\r\n", String::new())
            } else if path == "/" {
                let config = load_config(config_path)?;
                let selected = selected_repositories(&config, &args.repo)?;
                let state_store = build_state_store(&config)?;
                (
                    "HTTP/1.1 200 OK",
                    "",
                    render_dashboard_html(&selected, &state_store.state, args.refresh_seconds),
                )
            } else if path == "/healthz" {
                ("HTTP/1.1 200 OK", "", "ok".to_owned())
            } else {
                ("HTTP/1.1 404 Not Found", "", "not found".to_owned())
            };
        let content_type = if body.starts_with("<!DOCTYPE html>") {
            "text/html; charset=utf-8"
        } else {
            "text/plain; charset=utf-8"
        };
        let response = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .context("failed to write dashboard response")?;
        stream
            .flush()
            .context("failed to flush dashboard response")?;
    }

    Ok(())
}

fn doctor_service(service_name: &str) -> bool {
    println!("Service:");
    match std::env::consts::OS {
        "linux" => {
            let unit_name = format!("{service_name}.service");
            let unit_path = systemd_unit_path(service_name);
            let definition_ok = unit_path.exists();
            println!("  manager: systemd --user");
            print_doctor_check("definition", definition_ok, unit_path.display().to_string());

            let enabled = command_probe(
                "systemctl",
                &["--user", "is-enabled", &unit_name],
                "systemctl --user is-enabled",
            );
            print_doctor_check("enabled", enabled.ok, enabled.detail);

            let active = command_probe(
                "systemctl",
                &["--user", "is-active", &unit_name],
                "systemctl --user is-active",
            );
            print_doctor_check("active", active.ok, active.detail);

            !(definition_ok && enabled.ok && active.ok)
        }
        "macos" => {
            let plist_path = launchd_plist_path(service_name);
            let definition_ok = plist_path.exists();
            println!("  manager: launchd");
            print_doctor_check(
                "definition",
                definition_ok,
                plist_path.display().to_string(),
            );

            let service_target = match launchd_domain() {
                Ok(domain) => format!("{domain}/{service_name}"),
                Err(err) => {
                    print_doctor_check("loaded", false, err.to_string());
                    return true;
                }
            };
            let loaded = command_probe("launchctl", &["print", &service_target], "launchctl print");
            print_doctor_check("loaded", loaded.ok, loaded.detail);

            !(definition_ok && loaded.ok)
        }
        "windows" => {
            let script_path = windows_task_script_path(service_name);
            let definition_ok = script_path.exists();
            println!("  manager: Task Scheduler");
            print_doctor_check(
                "definition",
                definition_ok,
                script_path.display().to_string(),
            );

            let registered = command_probe(
                "schtasks",
                &["/Query", "/TN", service_name],
                "schtasks /Query",
            );
            print_doctor_check("registered", registered.ok, registered.detail);

            !(definition_ok && registered.ok)
        }
        other => {
            println!("  manager: unsupported");
            print_doctor_check(
                "manager",
                false,
                format!("doctor does not support service diagnostics on {other}"),
            );
            true
        }
    }
}

fn render_doctor(config_path: &Path, args: &DoctorArgs) -> Result<()> {
    let mut found_issues = false;
    let config_path = expand_tilde(config_path);

    println!("Doctor");
    println!("Config:");
    print_doctor_check(
        "path",
        config_path.exists(),
        config_path.display().to_string(),
    );

    let config = match load_config(&config_path) {
        Ok(config) => {
            let enabled = config
                .repositories
                .iter()
                .filter(|repo| repo.enabled)
                .count();
            print_doctor_check("parse", true, format!("{enabled} enabled repositories"));
            config
        }
        Err(err) => {
            print_doctor_check("parse", false, err.to_string());
            println!();
            let _ = doctor_service(&args.service_name);
            bail!("doctor found issues");
        }
    };

    println!();
    found_issues |= doctor_service(&args.service_name);

    println!();
    println!("Repositories:");
    let repos = match selected_repositories(&config, &args.repo) {
        Ok(repos) => repos,
        Err(err) => {
            print_doctor_check("selection", false, err.to_string());
            bail!("doctor found issues");
        }
    };

    for repo in repos {
        let repo_path = expand_tilde(&repo.config.path);
        println!("Repository: {}", repo.config.name);
        print_doctor_check("path", repo_path.exists(), repo_path.display().to_string());

        let syncer = match RepoSyncer::new(&repo_path) {
            Ok(syncer) => {
                print_doctor_check("init", true, syncer.repo_path().display().to_string());
                syncer
            }
            Err(err) => {
                print_doctor_check("init", false, err.to_string());
                found_issues = true;
                println!();
                continue;
            }
        };

        match probe_repository(&syncer, !args.no_fetch) {
            Ok((snapshot, report)) => {
                let effective = effective_report(&repo.config, &snapshot, &report);
                print_doctor_check("fetch", true, "remote probe succeeded");
                println!("  branch: {}", snapshot.branch);
                println!("  upstream: {}", snapshot.upstream);
                println!("  ahead: {}", snapshot.ahead);
                println!("  behind: {}", snapshot.behind);
                println!("  dirty: {}", snapshot.dirty);
                println!("  decision: {}", effective.decision);
                println!("  message: {}", effective.message);
                if effective.blocks_auto_pull {
                    found_issues = true;
                }
            }
            Err(err) => {
                print_doctor_check("fetch", false, err.to_string());
                found_issues = true;
            }
        }

        println!();
    }

    if found_issues {
        bail!("doctor found issues");
    }

    println!("Doctor OK");
    Ok(())
}

fn check_config(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    let repos = selected_repositories(&config, &[])?;

    for repo in &repos {
        RepoSyncer::new(expand_tilde(&repo.config.path))
            .with_context(|| format!("failed to initialize repository {}", repo.config.name))?;
    }

    println!(
        "Config OK: {} enabled repositories in {}",
        repos.len(),
        expand_tilde(config_path).display()
    );
    Ok(())
}

fn run(config_path: &Path, run_args: RunArgs) -> Result<()> {
    let config = load_config(config_path)?;
    let (mut logger, mut state_store, history_store, verbose, mut repos) =
        build_managed_repos(config, &run_args)?;

    let keep_running = Arc::new(AtomicBool::new(true));
    let signal_flag = Arc::clone(&keep_running);
    ctrlc::set_handler(move || {
        signal_flag.store(false, Ordering::SeqCst);
    })
    .context("failed to install signal handlers")?;

    if run_args.once {
        for managed in &mut repos {
            if let Err(err) = sync_repo(
                &mut logger,
                &mut state_store,
                &history_store,
                managed,
                run_args.dry_run,
                verbose,
            ) {
                handle_sync_error(&mut logger, &mut state_store, &history_store, managed, err)?;
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

            if let Err(err) = sync_repo(
                &mut logger,
                &mut state_store,
                &history_store,
                managed,
                run_args.dry_run,
                verbose,
            ) {
                handle_sync_error(&mut logger, &mut state_store, &history_store, managed, err)?;
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
        Some(Commands::Status(args)) => render_status(&config, &args),
        Some(Commands::Dashboard(args)) => serve_dashboard(&config, &args),
        Some(Commands::Pause(args)) => set_repository_paused(&config, &args.repo, true),
        Some(Commands::Resume(args)) => set_repository_paused(&config, &args.repo, false),
        Some(Commands::MigrateConfig) => migrate_config(&config),
        Some(Commands::CheckConfig) => check_config(&config),
        Some(Commands::Doctor(args)) => render_doctor(&config, &args),
        Some(Commands::InstallService(args)) => install_service(&config, &args),
        Some(Commands::UninstallService(args)) => uninstall_service(&args),
        None => run(&config, run_args),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, CURRENT_CONFIG_VERSION, CommandProbe, DefaultsConfig,
        DesktopNotificationsConfig, HistoryRecord, HistoryStore, NotificationSettings,
        PersistedRepoState, PersistedState, QuietHoursConfig, RepositoryConfig, StatusEntry,
        StatusOutput, branch_allowed, build_program_args, decision_blocks_auto_pull,
        effective_report, escape_applescript_text, in_quiet_hours, infer_repo_name,
        launchd_plist_path, load_config, merge_notification_settings, migrate_config,
        quote_systemd_arg, quote_windows_arg, render_dashboard_html, render_launchd_plist,
        render_systemd_service, render_windows_command_script, run_desktop_notification,
        sanitize_repo_name, selected_repositories, set_repository_paused, systemd_unit_path,
        upsert_repository, validate_config_schema, windows_task_script_path,
    };
    use chrono::NaiveTime;
    use repo_auto_puller_core::{Snapshot, SyncDecision};
    use std::fs;
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
            config_version: CURRENT_CONFIG_VERSION,
            defaults: DefaultsConfig::default(),
            repositories: vec![RepositoryConfig {
                name: "openclawcode".into(),
                path: PathBuf::from("/old"),
                interval_seconds: 60.0,
                enabled: true,
                paused: false,
                dry_run: false,
                allowed_branches: Vec::new(),
                quiet_hours: None,
                desktop_notifications: None,
                before_pull_command: None,
                after_pull_command: None,
                on_failure_command: None,
            }],
        };

        upsert_repository(
            &mut config,
            RepositoryConfig {
                name: "openclawcode".into(),
                path: PathBuf::from("/new"),
                interval_seconds: 30.0,
                enabled: true,
                paused: false,
                dry_run: true,
                allowed_branches: vec!["main".into()],
                quiet_hours: None,
                desktop_notifications: None,
                before_pull_command: Some("echo before".into()),
                after_pull_command: Some("echo after".into()),
                on_failure_command: Some("echo failed".into()),
            },
        );

        assert_eq!(config.repositories.len(), 1);
        assert_eq!(config.repositories[0].path, PathBuf::from("/new"));
        assert_eq!(config.repositories[0].interval_seconds, 30.0);
        assert!(config.repositories[0].dry_run);
        assert_eq!(config.repositories[0].allowed_branches, vec!["main"]);
        assert_eq!(
            config.repositories[0].before_pull_command.as_deref(),
            Some("echo before")
        );
        assert_eq!(
            config.repositories[0].after_pull_command.as_deref(),
            Some("echo after")
        );
        assert_eq!(
            config.repositories[0].on_failure_command.as_deref(),
            Some("echo failed")
        );
    }

    #[test]
    fn validates_duplicate_names() {
        let config = AppConfig {
            config_version: CURRENT_CONFIG_VERSION,
            defaults: DefaultsConfig::default(),
            repositories: vec![
                RepositoryConfig {
                    name: "same".into(),
                    path: PathBuf::from("/one"),
                    interval_seconds: 60.0,
                    enabled: true,
                    paused: false,
                    dry_run: false,
                    allowed_branches: Vec::new(),
                    quiet_hours: None,
                    desktop_notifications: None,
                    before_pull_command: None,
                    after_pull_command: None,
                    on_failure_command: None,
                },
                RepositoryConfig {
                    name: "same".into(),
                    path: PathBuf::from("/two"),
                    interval_seconds: 60.0,
                    enabled: true,
                    paused: false,
                    dry_run: false,
                    allowed_branches: Vec::new(),
                    quiet_hours: None,
                    desktop_notifications: None,
                    before_pull_command: None,
                    after_pull_command: None,
                    on_failure_command: None,
                },
            ],
        };

        let err = validate_config_schema(&config).expect_err("should reject duplicate names");
        assert!(err.to_string().contains("duplicate repository name"));
    }

    #[test]
    fn quotes_systemd_args_with_spaces() {
        assert_eq!(
            quote_systemd_arg("/tmp/path with spaces"),
            "\"/tmp/path with spaces\""
        );
        assert_eq!(quote_systemd_arg("/tmp/plain"), "/tmp/plain");
    }

    #[test]
    fn builds_program_args_with_repo_filters() {
        let args = build_program_args(
            PathBuf::from("/tmp/repo-auto-puller").as_path(),
            PathBuf::from("/tmp/config.toml").as_path(),
            &["openclawcode".into(), "docs".into()],
        );

        assert_eq!(
            args,
            vec![
                "/tmp/repo-auto-puller",
                "--config",
                "/tmp/config.toml",
                "--repo",
                "openclawcode",
                "--repo",
                "docs",
            ]
        );
    }

    #[test]
    fn renders_systemd_service_with_quoted_exec_start() {
        let service = render_systemd_service(&[
            "/tmp/repo auto puller".into(),
            "--config".into(),
            "/tmp/config with spaces.toml".into(),
        ]);

        assert!(service.contains(
            "ExecStart=\"/tmp/repo auto puller\" --config \"/tmp/config with spaces.toml\""
        ));
    }

    #[test]
    fn renders_launchd_plist_with_repo_filters() {
        let plist = render_launchd_plist(
            "repo-auto-puller",
            &[
                "/tmp/repo-auto-puller".into(),
                "--config".into(),
                "/tmp/config.toml".into(),
                "--repo".into(),
                "openclawcode".into(),
            ],
        );

        assert!(plist.contains("<string>repo-auto-puller</string>"));
        assert!(plist.contains("<string>/tmp/repo-auto-puller</string>"));
        assert!(plist.contains("<string>openclawcode</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn builds_systemd_unit_path_from_service_name() {
        let path = systemd_unit_path("repo-auto-puller");
        assert!(path.ends_with(".config/systemd/user/repo-auto-puller.service"));
    }

    #[test]
    fn builds_launchd_plist_path_from_service_name() {
        let path = launchd_plist_path("repo-auto-puller");
        assert!(path.ends_with("Library/LaunchAgents/repo-auto-puller.plist"));
    }

    #[test]
    fn quotes_windows_args_with_spaces() {
        assert_eq!(
            quote_windows_arg("C:/Program Files/repo-auto-puller.exe"),
            "\"C:/Program Files/repo-auto-puller.exe\""
        );
        assert_eq!(quote_windows_arg("plain"), "plain");
    }

    #[test]
    fn renders_windows_command_script_with_crlf() {
        let script = render_windows_command_script(&[
            "C:/Program Files/repo-auto-puller.exe".into(),
            "--config".into(),
            "C:/Users/test/AppData/Roaming/repo-auto-puller/config.toml".into(),
        ]);

        assert!(script.starts_with("@echo off\r\n"));
        assert!(script.contains("\"C:/Program Files/repo-auto-puller.exe\""));
        assert!(script.ends_with("\r\n"));
    }

    #[test]
    fn builds_windows_task_script_path_from_service_name() {
        let path = windows_task_script_path("repo-auto-puller");
        let display = path.display().to_string();
        assert!(display.contains("repo-auto-puller"));
        assert!(display.ends_with("repo-auto-puller.cmd"));
    }

    #[test]
    fn blocks_auto_pull_for_guard_decisions() {
        assert!(decision_blocks_auto_pull(&SyncDecision::AheadOnly));
        assert!(decision_blocks_auto_pull(&SyncDecision::DirtyBehind));
        assert!(decision_blocks_auto_pull(&SyncDecision::Diverged));
        assert!(!decision_blocks_auto_pull(&SyncDecision::UpToDate));
        assert!(!decision_blocks_auto_pull(&SyncDecision::PullFastForward));
    }

    #[test]
    fn command_probe_success_uses_stdout() {
        let probe = CommandProbe {
            ok: true,
            detail: "active".into(),
        };
        assert!(probe.ok);
        assert_eq!(probe.detail, "active");
    }

    #[test]
    fn serializes_status_output_with_error() {
        let output = StatusOutput {
            repositories: vec![StatusEntry {
                name: "openclawcode".into(),
                path: "/tmp/openclawcode".into(),
                branch: None,
                upstream: None,
                ahead: None,
                behind: None,
                dirty: None,
                decision: None,
                message: None,
                error: Some("fetch failed".into()),
            }],
        };

        let json = serde_json::to_string(&output).expect("status output should serialize");
        assert!(json.contains("\"name\":\"openclawcode\""));
        assert!(json.contains("\"error\":\"fetch failed\""));
    }

    #[test]
    fn appends_history_records_as_json_lines() {
        let path = std::env::temp_dir().join(format!(
            "repo-auto-puller-history-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time should be after epoch")
                .as_nanos()
        ));
        let store = HistoryStore::new(&path);
        let record = HistoryRecord {
            recorded_at: "2026-04-13T16:00:23+00:00".into(),
            name: "openclawcode".into(),
            path: "/tmp/openclawcode".into(),
            level: "INFO".into(),
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: Some(0),
            behind: Some(0),
            dirty: Some(false),
            decision: Some("up-to-date".into()),
            message: Some("repository is up to date".into()),
            error: None,
        };

        store
            .append(&record)
            .expect("history record should append successfully");
        let contents = fs::read_to_string(&path).expect("history file should be readable");

        assert!(contents.contains("\"name\":\"openclawcode\""));
        assert!(contents.ends_with('\n'));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn allows_all_branches_when_allowed_branches_is_empty() {
        let config = RepositoryConfig {
            name: "openclawcode".into(),
            path: PathBuf::from("/tmp/openclawcode"),
            interval_seconds: 60.0,
            enabled: true,
            paused: false,
            dry_run: false,
            allowed_branches: Vec::new(),
            quiet_hours: None,
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        };

        assert!(branch_allowed(&config, "main"));
        assert!(branch_allowed(&config, "feature/xyz"));
    }

    #[test]
    fn overrides_pull_decision_when_branch_is_not_allowed() {
        let config = RepositoryConfig {
            name: "openclawcode".into(),
            path: PathBuf::from("/tmp/openclawcode"),
            interval_seconds: 60.0,
            enabled: true,
            paused: false,
            dry_run: false,
            allowed_branches: vec!["main".into()],
            quiet_hours: None,
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        };
        let snapshot = Snapshot {
            branch: "feature".into(),
            upstream: "origin/feature".into(),
            remote: "origin".into(),
            remote_branch: "feature".into(),
            ahead: 0,
            behind: 2,
            dirty: false,
        };
        let report = repo_auto_puller_core::RepoSyncer::report(&snapshot);
        let effective = effective_report(&config, &snapshot, &report);

        assert_eq!(effective.decision, "branch-not-allowed");
        assert!(effective.blocks_auto_pull);
        assert!(effective.message.contains("allowed_branches [main]"));
    }

    #[test]
    fn overrides_report_when_repository_is_paused() {
        let config = RepositoryConfig {
            name: "openclawcode".into(),
            path: PathBuf::from("/tmp/openclawcode"),
            interval_seconds: 60.0,
            enabled: true,
            paused: true,
            dry_run: false,
            allowed_branches: vec!["main".into()],
            quiet_hours: None,
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        };
        let snapshot = Snapshot {
            branch: "main".into(),
            upstream: "origin/main".into(),
            remote: "origin".into(),
            remote_branch: "main".into(),
            ahead: 0,
            behind: 2,
            dirty: false,
        };
        let report = repo_auto_puller_core::RepoSyncer::report(&snapshot);
        let effective = effective_report(&config, &snapshot, &report);

        assert_eq!(effective.decision, "paused");
        assert!(effective.blocks_auto_pull);
        assert!(effective.message.contains("paused in config"));
    }

    #[test]
    fn matches_quiet_hours_within_same_day_window() {
        let config = RepositoryConfig {
            name: "openclawcode".into(),
            path: PathBuf::from("/tmp/openclawcode"),
            interval_seconds: 60.0,
            enabled: true,
            paused: false,
            dry_run: false,
            allowed_branches: Vec::new(),
            quiet_hours: Some(QuietHoursConfig {
                start: "09:00".into(),
                end: "17:00".into(),
            }),
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        };

        let hit = in_quiet_hours(
            &config,
            NaiveTime::from_hms_opt(10, 30, 0).expect("valid time"),
        )
        .expect("should parse quiet hours");
        let miss = in_quiet_hours(
            &config,
            NaiveTime::from_hms_opt(18, 0, 0).expect("valid time"),
        )
        .expect("should parse quiet hours");

        assert_eq!(hit.as_deref(), Some("09:00-17:00"));
        assert!(miss.is_none());
    }

    #[test]
    fn matches_quiet_hours_across_midnight() {
        let config = RepositoryConfig {
            name: "openclawcode".into(),
            path: PathBuf::from("/tmp/openclawcode"),
            interval_seconds: 60.0,
            enabled: true,
            paused: false,
            dry_run: false,
            allowed_branches: Vec::new(),
            quiet_hours: Some(QuietHoursConfig {
                start: "23:00".into(),
                end: "07:00".into(),
            }),
            desktop_notifications: None,
            before_pull_command: None,
            after_pull_command: None,
            on_failure_command: None,
        };

        let late = in_quiet_hours(
            &config,
            NaiveTime::from_hms_opt(23, 30, 0).expect("valid time"),
        )
        .expect("should parse quiet hours");
        let early = in_quiet_hours(
            &config,
            NaiveTime::from_hms_opt(6, 30, 0).expect("valid time"),
        )
        .expect("should parse quiet hours");
        let midday = in_quiet_hours(
            &config,
            NaiveTime::from_hms_opt(12, 0, 0).expect("valid time"),
        )
        .expect("should parse quiet hours");

        assert_eq!(late.as_deref(), Some("23:00-07:00"));
        assert_eq!(early.as_deref(), Some("23:00-07:00"));
        assert!(midday.is_none());
    }

    #[test]
    fn merges_notification_settings_with_repo_override() {
        let defaults = DesktopNotificationsConfig {
            on_pull: Some(false),
            on_failure: Some(true),
            command: Some("echo default".into()),
        };
        let repo = DesktopNotificationsConfig {
            on_pull: Some(true),
            on_failure: None,
            command: None,
        };

        let merged = merge_notification_settings(Some(&defaults), Some(&repo));

        assert!(merged.on_pull);
        assert!(merged.on_failure);
        assert_eq!(merged.command.as_deref(), Some("echo default"));
    }

    #[test]
    fn escapes_applescript_text() {
        assert_eq!(
            escape_applescript_text("repo \"main\" \\ updated"),
            "repo \\\"main\\\" \\\\ updated"
        );
    }

    #[test]
    fn selects_notification_settings_from_defaults() {
        let config: AppConfig = toml::from_str(
            r#"
[defaults.desktop_notifications]
on_pull = true
on_failure = true
command = "echo notify"

[[repositories]]
name = "openclawcode"
path = "/tmp/openclawcode"
"#,
        )
        .expect("config should parse");

        let selected = selected_repositories(&config, &[]).expect("repo should be selected");

        assert_eq!(selected.len(), 1);
        assert!(selected[0].notification_settings.on_pull);
        assert!(selected[0].notification_settings.on_failure);
        assert_eq!(
            selected[0].notification_settings.command.as_deref(),
            Some("echo notify")
        );
    }

    #[test]
    fn runs_custom_desktop_notification_command() {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time should be after epoch")
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(format!("repo-auto-puller-notify-{unique}"));
        fs::create_dir_all(&dir).expect("temp dir should be created");
        let log_path = dir.join("notifications.log");
        let script_path = dir.join("notify.sh");
        fs::write(
            &script_path,
            format!(
                "#!/usr/bin/env bash\nprintf '%s|%s|%s\\n' \"$REPO_AUTO_PULLER_EVENT\" \"$REPO_AUTO_PULLER_NOTIFICATION_TITLE\" \"$REPO_AUTO_PULLER_NOTIFICATION_BODY\" >> \"{}\"\n",
                log_path.display()
            ),
        )
        .expect("script should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&script_path)
                .expect("script metadata should exist")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions)
                .expect("script should be made executable");
        }

        let settings = NotificationSettings {
            on_pull: true,
            on_failure: true,
            command: Some(script_path.display().to_string()),
        };

        run_desktop_notification(
            &settings,
            "pull",
            "openclawcode",
            PathBuf::from("/tmp/openclawcode").as_path(),
            "repo-auto-puller",
            "openclawcode updated successfully",
        )
        .expect("notification command should execute");

        let contents = fs::read_to_string(&log_path).expect("notification log should exist");
        assert!(contents.contains("pull|repo-auto-puller|openclawcode updated successfully"));

        let _ = fs::remove_file(script_path);
        let _ = fs::remove_file(log_path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn renders_dashboard_html_with_recent_state() {
        let config: AppConfig = toml::from_str(
            r#"
[[repositories]]
name = "openclawcode"
path = "/tmp/openclawcode"
"#,
        )
        .expect("config should parse");
        let selected = selected_repositories(&config, &[]).expect("repo should be selected");
        let mut state = PersistedState::default();
        state.repositories.insert(
            "openclawcode".into(),
            PersistedRepoState {
                name: "openclawcode".into(),
                path: "/tmp/openclawcode".into(),
                updated_at: "2026-04-13T17:00:00+00:00".into(),
                level: "ERROR".into(),
                branch: Some("main".into()),
                upstream: Some("origin/main".into()),
                ahead: Some(0),
                behind: Some(2),
                dirty: Some(false),
                decision: Some("pull-fast-forward".into()),
                message: Some("main is behind origin/main by 2 commit(s)".into()),
                error: Some("fetch failed".into()),
            },
        );

        let html = render_dashboard_html(&selected, &state, 15);

        assert!(html.contains("Repo Auto Puller"));
        assert!(html.contains("openclawcode"));
        assert!(html.contains("fetch failed"));
        assert!(html.contains("2026-04-13T17:00:00+00:00"));
        assert!(html.contains("/pause/openclawcode"));
    }

    #[test]
    fn toggles_repository_pause_in_config() {
        let path = std::env::temp_dir().join(format!(
            "repo-auto-puller-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time should be after epoch")
                .as_nanos()
        ));
        fs::write(
            &path,
            r#"
[[repositories]]
name = "openclawcode"
path = "/tmp/openclawcode"
"#,
        )
        .expect("config file should be written");

        set_repository_paused(&path, "openclawcode", true).expect("pause should succeed");
        let paused = load_config(&path).expect("config should reload");
        assert!(paused.repositories[0].paused);

        set_repository_paused(&path, "openclawcode", false).expect("resume should succeed");
        let resumed = load_config(&path).expect("config should reload");
        assert!(!resumed.repositories[0].paused);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn migrates_legacy_config_to_current_version() {
        let path = std::env::temp_dir().join(format!(
            "repo-auto-puller-migrate-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time should be after epoch")
                .as_nanos()
        ));
        fs::write(
            &path,
            r#"
[[repositories]]
name = "openclawcode"
path = "/tmp/openclawcode"
"#,
        )
        .expect("config file should be written");

        migrate_config(&path).expect("migration should succeed");
        let migrated = load_config(&path).expect("migrated config should load");

        assert_eq!(migrated.config_version, CURRENT_CONFIG_VERSION);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_future_config_versions() {
        let config: AppConfig = toml::from_str(
            r#"
config_version = 999

[[repositories]]
name = "openclawcode"
path = "/tmp/openclawcode"
"#,
        )
        .expect("config should deserialize");

        let err = validate_config_schema(&config).expect_err("future version should be rejected");
        assert!(err.to_string().contains("config_version 999 is newer"));
    }
}
