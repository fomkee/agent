use std::{sync::Arc, time::Duration};

use thiserror::Error;
use tokio::{
    sync::{Semaphore, watch},
    task::JoinSet,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    clock::{Clock, SystemClock},
    collector::Collector,
    history::CounterHistory,
    model::{Observation, ObservationResult, Snapshot},
};

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler has no collectors")]
    NoCollectors,
    #[error("snapshot sequence overflowed")]
    SequenceOverflow,
    #[error("observation duration overflowed")]
    DurationOverflow,
    #[error("snapshot channel has no receivers")]
    NoSnapshotReceivers,
    #[error("collector task failed to join: {0}")]
    CollectorTaskJoin(String),
    #[error("collector concurrency limiter closed unexpectedly")]
    ConcurrencyLimiterClosed,
    #[error("failed to derive counter deltas")]
    CounterHistory(#[from] crate::history::HistoryError),
    #[error("collection cycle was cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerSettings {
    pub interval: Duration,
    pub collector_timeout: Duration,
    pub max_concurrency: usize,
}

pub async fn run_scheduler(
    collectors: Vec<Arc<dyn Collector>>,
    settings: SchedulerSettings,
    snapshots: watch::Sender<Arc<Snapshot>>,
    cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
) -> Result<(), SchedulerError> {
    if collectors.is_empty() {
        return Err(SchedulerError::NoCollectors);
    }
    let concurrency = settings.max_concurrency.max(1);
    let mut sequence = 0_u64;
    let mut history = CounterHistory::default();
    let mut interval = tokio::time::interval_at(Instant::now(), settings.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sequence = sequence.checked_add(1).ok_or(SchedulerError::SequenceOverflow)?;
                let mut observations = match collect_once_with_cancellation(
                    collectors.clone(),
                    settings.collector_timeout,
                    concurrency,
                    cancellation.clone(),
                    Arc::clone(&clock),
                ).await {
                    Ok(observations) => observations,
                    Err(SchedulerError::Cancelled) => return Ok(()),
                    Err(error) => return Err(error),
                };
                history.add_deltas(&mut observations)?;
                let snapshot = Snapshot {
                    schema_version: 1,
                    sequence,
                    generated_at: clock.now(),
                    observations,
                };
                snapshots.send(Arc::new(snapshot)).map_err(|_| SchedulerError::NoSnapshotReceivers)?;
            }
        }
    }
}

pub async fn collect_once(
    collectors: Vec<Arc<dyn Collector>>,
    timeout: Duration,
    concurrency: usize,
) -> Result<Vec<Observation>, SchedulerError> {
    collect_once_with_cancellation(
        collectors,
        timeout,
        concurrency,
        CancellationToken::new(),
        Arc::new(SystemClock),
    )
    .await
}

async fn collect_once_with_cancellation(
    collectors: Vec<Arc<dyn Collector>>,
    timeout: Duration,
    concurrency: usize,
    cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
) -> Result<Vec<Observation>, SchedulerError> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    for collector in collectors {
        let limiter = Arc::clone(&semaphore);
        let collector_clock = Arc::clone(&clock);
        tasks.spawn(async move {
            let _permit = limiter
                .acquire_owned()
                .await
                .map_err(|_| SchedulerError::ConcurrencyLimiterClosed)?;
            collect_one(collector, timeout, collector_clock).await
        });
    }
    let mut observations = Vec::with_capacity(tasks.len());
    while !tasks.is_empty() {
        tokio::select! {
            _ = cancellation.cancelled() => {
                tasks.abort_all();
                drain_cancelled_tasks(&mut tasks).await?;
                return Err(SchedulerError::Cancelled);
            }
            joined = tasks.join_next() => {
                let joined = joined.ok_or_else(|| SchedulerError::CollectorTaskJoin(
                    "task set became empty unexpectedly".to_owned()
                ))?;
                let observation = joined
                    .map_err(|error| SchedulerError::CollectorTaskJoin(error.to_string()))??;
                observations.push(observation);
            }
        }
    }
    observations.sort_by(|left, right| left.collector_id.cmp(&right.collector_id));
    Ok(observations)
}

async fn drain_cancelled_tasks(
    tasks: &mut JoinSet<Result<Observation, SchedulerError>>,
) -> Result<(), SchedulerError> {
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) if error.is_cancelled() => {}
            Err(error) => return Err(SchedulerError::CollectorTaskJoin(error.to_string())),
        }
    }
    Ok(())
}

async fn collect_one(
    collector: Arc<dyn Collector>,
    timeout: Duration,
    clock: Arc<dyn Clock>,
) -> Result<Observation, SchedulerError> {
    let started_at = clock.now();
    let started = Instant::now();
    let collector_id = collector.id().clone();
    let result = match tokio::time::timeout(timeout, collector.collect()).await {
        Ok(Ok(collection)) => ObservationResult::Collected {
            measurements: collection.measurements,
            facts: collection.facts,
        },
        Ok(Err(error)) => ObservationResult::Failed {
            code: error.code().to_owned(),
            summary: error.summary().to_owned(),
        },
        Err(_) => ObservationResult::Failed {
            code: "timeout".to_owned(),
            summary: "collector exceeded its configured timeout".to_owned(),
        },
    };
    let duration_ms = Observation::duration_millis(started.elapsed())
        .map_err(|_| SchedulerError::DurationOverflow)?;
    Ok(Observation {
        collector_id,
        collected_at: started_at,
        duration_ms,
        result,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::{
        collector::{Collection, CollectorError},
        model::CollectorId,
    };

    #[derive(Debug)]
    struct FailingCollector {
        id: CollectorId,
    }

    #[derive(Debug)]
    struct PendingCollector {
        id: CollectorId,
    }

    #[async_trait]
    impl Collector for FailingCollector {
        fn id(&self) -> &CollectorId {
            &self.id
        }

        async fn collect(&self) -> Result<Collection, CollectorError> {
            Err(CollectorError::new("fixture", "expected failure"))
        }
    }

    #[async_trait]
    impl Collector for PendingCollector {
        fn id(&self) -> &CollectorId {
            &self.id
        }

        async fn collect(&self) -> Result<Collection, CollectorError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn test_cycle_records_collector_failure_as_observation() {
        // Arrange
        let collectors: Vec<Arc<dyn Collector>> = vec![Arc::new(FailingCollector {
            id: CollectorId::new("fixture").expect("valid fixture"),
        })];

        // Act
        let observations = collect_once(collectors, Duration::from_secs(1), 1)
            .await
            .expect("cycle should complete");

        // Assert
        assert!(
            matches!(observations.first().map(|item| &item.result), Some(ObservationResult::Failed { code, .. }) if code == "fixture")
        );
    }

    #[tokio::test]
    async fn test_cycle_drains_tasks_when_cancelled() {
        // Arrange
        let collectors: Vec<Arc<dyn Collector>> = vec![Arc::new(PendingCollector {
            id: CollectorId::new("pending").expect("valid fixture"),
        })];
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        // Act
        let result = collect_once_with_cancellation(
            collectors,
            Duration::from_secs(60),
            1,
            cancellation,
            Arc::new(SystemClock),
        )
        .await;

        // Assert
        assert!(matches!(result, Err(SchedulerError::Cancelled)));
    }

    #[tokio::test]
    async fn test_cycle_records_timeout_as_failed_observation() {
        // Arrange
        let collectors: Vec<Arc<dyn Collector>> = vec![Arc::new(PendingCollector {
            id: CollectorId::new("pending").expect("valid fixture"),
        })];

        // Act
        let observations = collect_once(collectors, Duration::from_millis(1), 1)
            .await
            .expect("cycle should complete");

        // Assert
        assert!(
            matches!(observations.first().map(|item| &item.result), Some(ObservationResult::Failed { code, .. }) if code == "timeout")
        );
    }
}
