#![forbid(unsafe_code)]

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::{Parser, Subcommand};
use fomkee_agent::{
    clock::{Clock, SystemClock},
    collectors::build_collectors,
    config::Config,
    model::Snapshot,
    render,
    scheduler::{SchedulerSettings, collect_once, run_scheduler},
    server::{HttpState, serve},
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
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fomkee-agent: {error:#}");
            ExitCode::FAILURE
        }
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
    }
}

async fn snapshot(config: Config, as_json: bool) -> Result<()> {
    let collectors = build_collectors(&config.collectors).context("build collectors")?;
    let observations = collect_once(
        collectors,
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
    let token = config.bearer_token().context("load HTTP bearer token")?;
    let cancellation = CancellationToken::new();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let initial = Arc::new(Snapshot::empty(clock.now()));
    let (snapshot_sender, snapshot_receiver) = watch::channel(initial);
    let settings = SchedulerSettings {
        interval: config.schedule.interval(),
        collector_timeout: config.schedule.collector_timeout(),
        max_concurrency: config.schedule.max_concurrency,
    };
    let max_snapshot_age = settings
        .interval
        .checked_mul(3)
        .ok_or_else(|| anyhow!("schedule interval is too large"))?;
    let mut tasks = JoinSet::new();
    let scheduler_cancellation = cancellation.child_token();
    let scheduler_clock = Arc::clone(&clock);
    tasks.spawn(async move {
        CriticalTask::Scheduler(
            run_scheduler(
                collectors,
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
        Ok(CriticalTask::Scheduler(Ok(())) | CriticalTask::Server(Ok(()))) => {}
        Ok(CriticalTask::Scheduler(Err(error))) => {
            failures.push(format!("scheduler failed: {error:#}"));
        }
        Ok(CriticalTask::Server(Err(error))) => {
            failures.push(format!("HTTP server failed: {error:#}"));
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
