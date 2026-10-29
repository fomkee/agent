#![forbid(unsafe_code)]

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::{Parser, Subcommand};
use fomkee_agent::{
    clock::{Clock, SystemClock},
    collector::Collector,
    collectors::build_collectors,
    config::{Config, ConfigError},
    heartbeat, local_socket,
    model::Snapshot,
    render,
    scheduler::{CollectorJob, SchedulerSettings, collect_once, run_scheduler},
    server::{HttpState, serve},
    tui,
};
use tokio::{sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "fomkee-agent", version, about)]
struct Cli {
    #[arg(
        long,
        env = "FOMKEE_AGENT_CONFIG",
        default_value = "/etc/fomkee-agent/config.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve,
    Tui,
    Snapshot {
        #[arg(long)]
        json: bool,
    },
    Validate,
}

#[derive(Debug)]
enum CriticalTask {
    Scheduler(Result<(), anyhow::Error>),
    Server(Result<(), anyhow::Error>),
    LocalSocket(Result<(), anyhow::Error>),
    HeartbeatPublisher(Result<(), anyhow::Error>),
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fomkee-agent: {}", safe_diagnostic(&error));
            ExitCode::FAILURE
        }
    }
}

fn safe_diagnostic(error: &anyhow::Error) -> String {
    if let Some(config_error) = error.downcast_ref::<ConfigError>() {
        format!("{config_error}")
    } else {
        format!("{error:#}")
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing()?;
    let config = Config::load(&cli.config)
        .await
        .with_context(|| format!("load configuration from {}", cli.config.display()))?;
    match cli.command {
        Command::Validate => {
            println!("configuration is valid");
            Ok(())
        }
        Command::Snapshot { json } => snapshot(config, json).await,
        Command::Serve => daemon(config).await,
        Command::Tui => run_tui(&config).await,
    }
}

async fn run_tui(config: &Config) -> Result<()> {
    let max_snapshot_age = config
        .shortest_collector_interval()
        .context("derive shortest collector interval")?
        .checked_mul(3)
        .ok_or_else(|| anyhow!("schedule interval is too large"))?;
    let settings = tui::TuiSettings::from_config(config).context("build TUI settings")?;
    tui::run(&config.server.unix_socket, max_snapshot_age, settings)
        .await
        .context("run agent TUI")
}

async fn snapshot(config: Config, as_json: bool) -> Result<()> {
    let collectors = build_collectors(&config.collectors).context("build collectors")?;
    let jobs = configure_jobs(collectors, &config.collectors, config.schedule.interval())?;
    let observations = collect_once(
        jobs,
        config.schedule.collector_timeout(),
        config.schedule.max_concurrency,
    )
    .await
    .context("collect snapshot")?;
    let snapshot = Snapshot {
        schema_version: 1,
        sequence: 1,
        generated_at: Utc::now(),
        observations,
    };
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&render::json::compact(&snapshot))
                .context("serialize compact snapshot")?
        );
    } else {
        println!("{}", render::openmetrics::render(&snapshot)?);
    }
    Ok(())
}

async fn daemon(config: Config) -> Result<()> {
    let collectors = build_collectors(&config.collectors).context("build collectors")?;
    let jobs = configure_jobs(collectors, &config.collectors, config.schedule.interval())?;
    let token = config.bearer_token().context("load HTTP bearer token")?;
    let cancellation = CancellationToken::new();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let initial = Arc::new(Snapshot::empty(clock.now()));
    let (snapshot_sender, snapshot_receiver) = watch::channel(initial);
    let settings = SchedulerSettings {
        collector_timeout: config.schedule.collector_timeout(),
        max_concurrency: config.schedule.max_concurrency,
    };
    let max_snapshot_age = config
        .shortest_collector_interval()
        .context("derive shortest collector interval")?
        .checked_mul(3)
        .ok_or_else(|| anyhow!("schedule interval is too large"))?;
    let mut tasks = JoinSet::new();
    let scheduler_cancellation = cancellation.child_token();
    let scheduler_clock = Arc::clone(&clock);
    tasks.spawn(async move {
        CriticalTask::Scheduler(
            run_scheduler(
                jobs,
                settings,
                snapshot_sender,
                scheduler_cancellation,
                scheduler_clock,
            )
            .await
            .map_err(anyhow::Error::from),
        )
    });
    let server_cancellation = cancellation.child_token();
    let local_snapshot_receiver = snapshot_receiver.clone();
    let heartbeat_snapshot_receiver = snapshot_receiver.clone();
    let server_state = HttpState::new(
        snapshot_receiver,
        token,
        config.server.metrics_enabled,
        max_snapshot_age,
        clock,
    );
    let listen = config.server.listen;
    tasks.spawn(async move {
        CriticalTask::Server(
            serve(listen, server_state, server_cancellation)
                .await
                .map_err(anyhow::Error::from),
        )
    });
    let local_socket_cancellation = cancellation.child_token();
    let unix_socket = config.server.unix_socket.clone();
    tasks.spawn(async move {
        CriticalTask::LocalSocket(
            local_socket::serve(
                unix_socket,
                local_snapshot_receiver,
                local_socket_cancellation,
            )
            .await
            .map_err(anyhow::Error::from),
        )
    });
    if let Some(endpoint) = config
        .heartbeat_endpoint()
        .context("load Fomkee heartbeat endpoint")?
    {
        let heartbeat_cancellation = cancellation.child_token();
        tasks.spawn(async move {
            CriticalTask::HeartbeatPublisher(
                heartbeat::run(
                    endpoint,
                    heartbeat_snapshot_receiver,
                    heartbeat_cancellation,
                )
                .await
                .map_err(anyhow::Error::from),
            )
        });
    } else if config.heartbeat.is_some() {
        tracing::warn!("Fomkee heartbeat publishing is configured but its URL is not installed");
    }
    tracing::info!(%listen, "fomkee agent started");

    let outcome = tokio::select! {
        signal = shutdown_signal() => {
            signal?;
            cancellation.cancel();
            None
        }
        joined = tasks.join_next() => {
            cancellation.cancel();
            Some(joined.ok_or_else(|| anyhow!("critical task set became empty"))?)
        }
    };
    let mut failures = Vec::new();
    if let Some(joined) = outcome {
        inspect_join(joined, true, &mut failures);
    }
    while let Some(joined) = tasks.join_next().await {
        inspect_join(joined, false, &mut failures);
    }
    if failures.is_empty() {
        tracing::info!("fomkee agent stopped");
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

fn configure_jobs(
    collectors: Vec<Arc<dyn Collector>>,
    configs: &[fomkee_agent::config::CollectorConfig],
    default_interval: std::time::Duration,
) -> Result<Vec<CollectorJob>> {
    if collectors.len() != configs.len() {
        return Err(anyhow!("collector construction count mismatch"));
    }
    collectors
        .into_iter()
        .zip(configs)
        .map(|(collector, config)| {
            config
                .interval(default_interval)
                .map(|interval| CollectorJob::new(collector, interval))
                .context("derive collector interval")
        })
        .collect()
}

fn inspect_join(
    joined: Result<CriticalTask, tokio::task::JoinError>,
    unexpected_exit: bool,
    failures: &mut Vec<String>,
) {
    match joined {
        Ok(CriticalTask::Scheduler(Ok(()))) if unexpected_exit => {
            failures.push("scheduler exited unexpectedly".to_owned());
        }
        Ok(CriticalTask::Server(Ok(()))) if unexpected_exit => {
            failures.push("HTTP server exited unexpectedly".to_owned());
        }
        Ok(CriticalTask::LocalSocket(Ok(()))) if unexpected_exit => {
            failures.push("local snapshot server exited unexpectedly".to_owned());
        }
        Ok(CriticalTask::HeartbeatPublisher(Ok(()))) if unexpected_exit => {
            failures.push("heartbeat publisher exited unexpectedly".to_owned());
        }
        Ok(
            CriticalTask::Scheduler(Ok(()))
            | CriticalTask::Server(Ok(()))
            | CriticalTask::LocalSocket(Ok(()))
            | CriticalTask::HeartbeatPublisher(Ok(())),
        ) => {}
        Ok(CriticalTask::Scheduler(Err(error))) => {
            failures.push(format!("scheduler failed: {error:#}"));
        }
        Ok(CriticalTask::Server(Err(error))) => {
            failures.push(format!("HTTP server failed: {error:#}"));
        }
        Ok(CriticalTask::LocalSocket(Err(error))) => {
            failures.push(format!("local snapshot server failed: {error:#}"));
        }
        Ok(CriticalTask::HeartbeatPublisher(Err(error))) => {
            failures.push(format!("heartbeat publisher failed: {error:#}"));
        }
        Err(error) => failures.push(format!("critical task join failed: {error}")),
    }
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("listen for Ctrl-C")?,
            _ = terminate.recv() => {},
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.context("listen for Ctrl-C")
    }
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let format = std::env::var("FOMKEE_LOG_FORMAT").unwrap_or_else(|_| "pretty".to_owned());
    if format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .try_init()
            .map_err(|error| anyhow!("initialize JSON tracing: {error}"))?;
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .try_init()
            .map_err(|error| anyhow!("initialize tracing: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_configuration_diagnostic_does_not_expose_source_text() {
        // Arrange
        let source = toml::from_str::<Config>("secret = \"CONFIG_PRIVATE_MARKER\"")
            .expect_err("fixture must be invalid configuration");
        let error = anyhow::Error::new(ConfigError::Parse(source)).context("load configuration");

        // Act
        let diagnostic = safe_diagnostic(&error);

        // Assert
        assert_eq!(diagnostic, "failed to parse agent configuration");
    }
}
