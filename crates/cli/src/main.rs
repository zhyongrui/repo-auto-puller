use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
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
    CheckConfig,
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
}

#[derive(Debug, Args)]
struct InstallServiceArgs {
    #[arg(long, default_value = "repo-auto-puller")]
    service_name: String,

    #[arg(long, default_value = "~/.local/bin/repo-auto-puller")]
    binary: PathBuf,

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
    #[serde(default)]
    defaults: DefaultsConfig,
    #[serde(default)]
    repositories: Vec<RepositoryConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DefaultsConfig {
    log_file: Option<PathBuf>,
    #[serde(default)]
    verbose: bool,
    on_failure_command: Option<String>,
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
    on_failure_command: Option<String>,
}

#[derive(Clone)]
struct SelectedRepo {
    config: RepositoryConfig,
    on_failure_command: Option<String>,
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
            on_failure_command: repo
                .on_failure_command
                .clone()
                .or_else(|| config.defaults.on_failure_command.clone()),
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
) -> Result<(Logger, bool, Vec<ManagedRepo>)> {
    let logger = build_logger(&config)?;
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
            on_failure_command: repo.on_failure_command,
        });
    }

    Ok((logger, verbose, managed))
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
            dry_run: args.dry_run,
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
    println!("  dry_run: {}", args.dry_run);
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

fn install_systemd_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    let service_name = args.service_name.clone();
    let unit_path = systemd_unit_path(&service_name);
    ensure_parent_dir(&unit_path)?;

    let program_args = build_program_args(
        &expand_tilde(&args.binary),
        &expand_tilde(config_path),
        &args.repo,
    );
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

    let binary = expand_tilde(&args.binary);
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

fn install_service(config_path: &Path, args: &InstallServiceArgs) -> Result<()> {
    if args.service_name.trim().is_empty() {
        bail!("--service-name must not be empty");
    }

    match std::env::consts::OS {
        "linux" => install_systemd_service(config_path, args),
        "macos" => install_launchd_service(config_path, args),
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

fn sync_repo(
    logger: &mut Logger,
    managed: &mut ManagedRepo,
    global_dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let (snapshot, report) = probe_repository(&managed.syncer, true)?;

    if verbose
        || report.level != "IDLE"
        || managed.last_message.as_deref() != Some(report.message.as_str())
    {
        logger.log(report.level, &managed.config.name, &report.message)?;
        managed.last_message = Some(report.message.clone());
    }

    managed.last_error = None;

    if report.decision != SyncDecision::PullFastForward {
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

fn handle_sync_error(
    logger: &mut Logger,
    managed: &mut ManagedRepo,
    err: anyhow::Error,
) -> Result<()> {
    let error = err.to_string();
    logger.log("ERROR", &managed.config.name, &error)?;
    if managed.last_error.as_deref() != Some(error.as_str()) {
        run_failure_hook(logger, managed, &error)?;
    }
    managed.last_error = Some(error);
    Ok(())
}

fn render_status(config_path: &Path, args: &StatusArgs) -> Result<()> {
    let config = load_config(config_path)?;
    let repos = selected_repositories(&config, &args.repo)?;

    for repo in repos {
        let syncer = RepoSyncer::new(expand_tilde(&repo.config.path))
            .with_context(|| format!("failed to initialize repository {}", repo.config.name))?;
        println!("Repository: {}", repo.config.name);
        println!("  path: {}", syncer.repo_path().display());

        match probe_repository(&syncer, !args.no_fetch) {
            Ok((snapshot, report)) => {
                println!("  branch: {}", snapshot.branch);
                println!("  upstream: {}", snapshot.upstream);
                println!("  ahead: {}", snapshot.ahead);
                println!("  behind: {}", snapshot.behind);
                println!("  dirty: {}", snapshot.dirty);
                println!("  decision: {}", report.decision.as_str());
                println!("  message: {}", report.message);
            }
            Err(err) => {
                println!("  error: {err}");
            }
        }
    }

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
                handle_sync_error(&mut logger, managed, err)?;
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
                handle_sync_error(&mut logger, managed, err)?;
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
        Some(Commands::CheckConfig) => check_config(&config),
        Some(Commands::InstallService(args)) => install_service(&config, &args),
        Some(Commands::UninstallService(args)) => uninstall_service(&args),
        None => run(&config, run_args),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, DefaultsConfig, RepositoryConfig, build_program_args, infer_repo_name,
        launchd_plist_path, quote_systemd_arg, render_launchd_plist, render_systemd_service,
        sanitize_repo_name, systemd_unit_path, upsert_repository, validate_config_schema,
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
                dry_run: true,
                on_failure_command: Some("echo failed".into()),
            },
        );

        assert_eq!(config.repositories.len(), 1);
        assert_eq!(config.repositories[0].path, PathBuf::from("/new"));
        assert_eq!(config.repositories[0].interval_seconds, 30.0);
        assert!(config.repositories[0].dry_run);
        assert_eq!(
            config.repositories[0].on_failure_command.as_deref(),
            Some("echo failed")
        );
    }

    #[test]
    fn validates_duplicate_names() {
        let config = AppConfig {
            defaults: DefaultsConfig::default(),
            repositories: vec![
                RepositoryConfig {
                    name: "same".into(),
                    path: PathBuf::from("/one"),
                    interval_seconds: 60.0,
                    enabled: true,
                    dry_run: false,
                    on_failure_command: None,
                },
                RepositoryConfig {
                    name: "same".into(),
                    path: PathBuf::from("/two"),
                    interval_seconds: 60.0,
                    enabled: true,
                    dry_run: false,
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
}
