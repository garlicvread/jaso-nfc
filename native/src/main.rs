use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};
use jaso_nfc::{config::Config, control, normalizer::Normalizer, service};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Normalize filenames once, then process native macOS change events."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read, preview, or save folder settings from the native setup window.
    Setup {
        #[command(subcommand)]
        command: SetupCommand,
    },
    /// Inspect filenames. Renames require an explicit --apply.
    Scan(Options),
    /// Run the resident worker in the foreground.
    Watch(Options),
    /// Run the installed worker and open its native menu.
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    /// Show persisted progress and actual worker state.
    Status(Options),
    /// Read the worker's live activity without waiting for directory operations.
    Activity(Options),
    /// Show storage used by Jaso and free capacity on its data volumes.
    Storage(Options),
    /// Compact app data while the worker is stopped.
    Maintain(Options),
    /// Browse changes and request restoration of one original name.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Queue a subtree (or the configured roots) for reconciliation.
    Reconcile {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        path: Vec<String>,
    },
    /// Pause renaming while continuing to record changes.
    Pause(Options),
    /// Resume queued work.
    Resume(Options),
    /// Undo successful operations; the worker must be stopped.
    Revert {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        journal: PathBuf,
    },
    /// Start the installed worker.
    Start,
    /// Stop the installed worker until it is started again or login occurs.
    Stop,
    /// Restart the installed worker after its previous process has stopped.
    Restart,
    /// Install the native app and its login job.
    Install {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        app: PathBuf,
    },
    /// Keep the app and recovery history, remove login jobs.
    Uninstall,
    /// Enable or disable starting at login (does not stop a running worker).
    Startup {
        #[arg(value_parser=["on","off","status"])]
        mode: String,
    },
}
#[derive(Subcommand)]
enum SetupCommand {
    StartDrive {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        uuid: String,
        #[arg(long)]
        revision: String,
    },
    Read {
        #[arg(long)]
        config: PathBuf,
    },
    Preview {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        draft: String,
    },
    Save {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        draft: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        start: bool,
    },
}
#[derive(Subcommand)]
enum HistoryCommand {
    List {
        #[command(flatten)]
        options: Options,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value = "")]
        search: String,
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        result: Option<String>,
    },
    Preview {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        id: String,
        #[arg(long)]
        revision: String,
    },
    Restore {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        revision: String,
    },
    Result {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        request_id: String,
    },
}
#[derive(Args, Default)]
struct Options {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, conflicts_with = "all_user_files")]
    root: Vec<String>,
    #[arg(long)]
    all_user_files: bool,
    #[arg(long)]
    exclude: Vec<String>,
    #[arg(long)]
    state_dir: Option<String>,
    #[arg(long)]
    log_dir: Option<String>,
    #[arg(long)]
    apply: bool,
}

fn configuration_path(options: &Options) -> PathBuf {
    options.config.clone().unwrap_or_else(|| {
        PathBuf::from(
            options
                .state_dir
                .as_deref()
                .unwrap_or(&Config::default().state_dir),
        )
        .join("config.json")
    })
}

fn configuration(options: &Options, preview: bool) -> Result<Config> {
    let mut initial = Config::default();
    if let Some(state) = &options.state_dir {
        initial.state_dir = state.clone();
    }
    let path = configuration_path(options);
    let loaded = path.exists();
    let mut c = if loaded {
        Config::load(path)?
    } else {
        ensure!(options.config.is_none(), "configuration does not exist");
        initial
    };
    if let Some(state) = &options.state_dir {
        c.state_dir = state.clone();
        if !loaded && options.log_dir.is_none() {
            c.log_dir = None;
        }
    }
    if let Some(log) = &options.log_dir {
        c.log_dir = Some(log.clone());
    }
    if !options.root.is_empty() {
        c.scope = "configured".into();
        c.roots = options.root.clone();
    }
    if options.all_user_files {
        c.scope = "all-user-files".into();
        c.roots.clear();
        c.skip_hidden_tops.clear();
        if !loaded {
            c.excludes.clear();
        }
    }
    c.excludes.extend(options.exclude.clone());
    if options.apply || preview {
        c.apply = options.apply;
    }
    c.validate()?;
    Ok(c)
}
fn output(value: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
}

fn execute(command: Command) -> Result<()> {
    match command {
        Command::Setup { command } => {
            use jaso_nfc::setup;
            output(match command {
                SetupCommand::StartDrive {
                    config,
                    uuid,
                    revision,
                } => setup::start_drive(&config, &uuid, &revision)?,
                SetupCommand::Read { config } => setup::read(&config)?,
                SetupCommand::Preview { config, draft } => {
                    setup::preview(&config, &setup::Draft::parse(&draft)?)?
                }
                SetupCommand::Save {
                    config,
                    draft,
                    revision,
                    start,
                } => setup::save(&config, &setup::Draft::parse(&draft)?, &revision, start)?,
            });
        }
        Command::Run { config } => {
            let c = Config::load(&config)?;
            if let Err(error) = service::run(&c, &config) {
                service::diagnostic(&c, &format!("ERROR {error:#}"));
                return Err(error);
            }
        }
        Command::Watch(options) => {
            let c = configuration(&options, false)?;
            if let Err(error) = service::watch_at(&c, &configuration_path(&options)) {
                service::diagnostic(&c, &format!("ERROR {error:#}"));
                return Err(error);
            }
        }
        Command::Status(options) => {
            output(service::runtime_status(&configuration(&options, false)?)?)
        }
        Command::Storage(options) => output(jaso_nfc::storage::snapshot(&configuration(
            &options, false,
        )?)?),
        Command::Maintain(options) => {
            let config = configuration(&options, false)?;
            let _lock = control::RuntimeLock::acquire(&config, Duration::ZERO)?;
            let before = jaso_nfc::storage::snapshot(&config)?;
            let index_path = config.state_path("index.sqlite3");
            if index_path.exists() {
                drop(jaso_nfc::index::Index::new(&index_path, false)?);
            }
            let history = jaso_nfc::history::maintain(&config)?;
            let backups_removed =
                jaso_nfc::retention::prune_backups(std::path::Path::new(&config.state_dir))?;
            output(
                json!({"before":before,"after":jaso_nfc::storage::snapshot(&config)?,"history":history,"backups_removed":backups_removed}),
            );
        }
        Command::Activity(options) => output(jaso_nfc::activity_transport::snapshot_for(
            &configuration(&options, false)?,
        )?),
        Command::History { command } => {
            use jaso_nfc::history::{self, HistoryQuery, RestoreRequest};
            output(match command {
                HistoryCommand::List {
                    options,
                    limit,
                    offset,
                    search,
                    date,
                    result,
                } => serde_json::to_value(history::list(
                    &configuration(&options, false)?,
                    &HistoryQuery {
                        limit,
                        offset,
                        search,
                        date,
                        result,
                    },
                )?)?,
                HistoryCommand::Preview {
                    options,
                    id,
                    revision,
                } => serde_json::to_value(history::preview(
                    &configuration(&options, false)?,
                    &id,
                    &revision,
                )?)?,
                HistoryCommand::Restore {
                    options,
                    request_id,
                    operation_id,
                    revision,
                } => serde_json::to_value(history::request_restore(
                    &configuration(&options, false)?,
                    &RestoreRequest {
                        request_id,
                        operation_id,
                        revision,
                    },
                )?)?,
                HistoryCommand::Result {
                    options,
                    request_id,
                } => serde_json::to_value(history::restore_result(
                    &configuration(&options, false)?,
                    &request_id,
                )?)?,
            });
        }
        Command::Pause(options) | Command::Resume(options) => {
            unreachable!("handled before move: {:?}", options.config)
        }
        Command::Reconcile { options, path } => {
            let c = configuration(&options, false)?;
            ensure!(
                c.state_path("index.sqlite3").exists(),
                "no index exists; start the worker first"
            );
            let coverage = jaso_nfc::sources::resolve_coverage(&c);
            let index = jaso_nfc::index::Index::new(c.state_path("index.sqlite3"), false)?;
            index.bind_policy(jaso_nfc::sources::policy_for(&c, &coverage, None))?;
            index.request_reconcile(if path.is_empty() { None } else { Some(&path) })?;
            output(
                json!({"queued":if path.is_empty(){coverage.roots}else{path},"worker_notified":control::signal_wakeup(c.state_path("wake.fifo"))}),
            );
        }
        Command::Scan(options) => {
            let c = configuration(&options, true)?;
            let coverage = jaso_nfc::sources::resolve_coverage(&c);
            let policy = jaso_nfc::sources::policy_for(&c, &coverage, None);
            let _lock = if c.apply {
                Some(control::RuntimeLock::acquire(&c, Duration::ZERO)?)
            } else {
                None
            };
            let mut normalizer = if c.apply {
                let mut n = service::make_normalizer(&c)?;
                n.policy = policy;
                n
            } else {
                Normalizer::new(policy, None, None, None, false)?
            };
            let mut entries = 0u64;
            let mut renamed = 0u64;
            let mut candidates = Vec::new();
            let mut errors = Vec::new();
            // Keep the traversal frontier on disk instead of retaining a full tree.
            let temporary =
                std::env::temp_dir().join(format!("jaso-scan-{}.sqlite3", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let db = rusqlite::Connection::open(&temporary)?;
                db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA cache_size=-512; CREATE TABLE pending(path TEXT PRIMARY KEY)")?;
                for root in &coverage.roots {
                    db.execute("INSERT OR IGNORE INTO pending VALUES(?)", [root])?;
                }
                loop {
                    use rusqlite::OptionalExtension;
                    let path: Option<String> = db
                        .query_row("SELECT path FROM pending LIMIT 1", [], |r| r.get(0))
                        .optional()?;
                    let Some(path) = path else { break };
                    db.execute("DELETE FROM pending WHERE path=?", [&path])?;
                    let result = normalizer.reconcile(&path, false)?;
                    entries += result.entries.len() as u64;
                    renamed += result.renamed;
                    errors.extend(result.errors);
                    for entry in result.entries {
                        if matches!(entry.kind.as_str(), "dir" | "directory")
                            && normalizer.policy.descend(&entry.path)
                        {
                            db.execute("INSERT OR IGNORE INTO pending VALUES(?)", [&entry.path])?;
                        }
                        let name = std::path::Path::new(&entry.path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("");
                        if jaso_nfc::filename_repair::entry_target(
                            name,
                            entry.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32,
                        ) != name
                        {
                            candidates.push(entry.path);
                        }
                    }
                }
                Ok(())
            })();
            let _ = std::fs::remove_file(&temporary);
            result?;
            output(
                json!({"mode":if c.apply{"apply"}else{"scan"},"entries":entries,"renamed":renamed,"candidates":candidates,"errors":errors}),
            );
            ensure!(errors.is_empty(), "some paths could not be scanned");
        }
        Command::Revert { options, journal } => {
            let c = configuration(&options, false)?;
            let _lock = control::RuntimeLock::acquire(&c, Duration::ZERO)?;
            let (done, failed) = jaso_nfc::journal::revert(
                &journal,
                Some(&PathBuf::from(c.logs()).join("reverts.jsonl")),
            )?;
            output(json!({"reverted":done,"failed":failed}));
            ensure!(failed == 0, "some operations could not be reverted");
        }
        Command::Start => {
            jaso_nfc::install::start()?;
            output(json!({"started":service::LABEL}));
        }
        Command::Stop => {
            jaso_nfc::install::stop()?;
            output(json!({"stopped":service::LABEL}));
        }
        Command::Restart => {
            jaso_nfc::install::restart()?;
            output(json!({"restarted":service::LABEL}));
        }
        Command::Install { options, app } => {
            let c = configuration(&options, false)?;
            output(jaso_nfc::install::install(&c, &app)?);
        }
        Command::Uninstall => {
            jaso_nfc::install::uninstall()?;
            output(json!({"removed":"login jobs","recovery_history_retained":true}));
        }
        Command::Startup { mode } => output(jaso_nfc::install::startup(&mode)?),
    }
    Ok(())
}

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).is_none()
        && let Err(error) = jaso_nfc::app_bundle::dispatch_gui_if_bundled()
    {
        eprintln!("jaso-nfc: {error:#}");
        std::process::exit(1);
    }
    let command = Cli::parse().command;
    let result = match command {
        Command::Pause(options) => configuration(&options, false).and_then(|c| {
            let notified = jaso_nfc::install::set_paused(&c, true)?;
            output(json!({"paused":true,"worker_notified":notified}));
            Ok(())
        }),
        Command::Resume(options) => configuration(&options, false).and_then(|c| {
            let notified = jaso_nfc::install::set_paused(&c, false)?;
            output(json!({"paused":false,"worker_notified":notified}));
            Ok(())
        }),
        other => execute(other),
    };
    if let Err(error) = result {
        eprintln!("jaso-nfc: {error:#}");
        std::process::exit(1);
    }
}
