use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

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
    model::{CollectionInterval, CollectorId, Observation, ObservationResult, Snapshot},
};

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler has no collectors")]
    NoCollectors,
    #[error("snapshot sequence overflowed")]
    SequenceOverflow,
    #[error("collector deadline overflowed")]
    DeadlineOverflow,
    #[error("scheduler has no pending collector deadline")]
    NoPendingDeadline,
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

#[derive(Clone)]
/// Collector and validated cadence admitted to the scheduler.
pub struct CollectorJob {
    collector: Arc<dyn Collector>,
    interval: CollectionInterval,
}

impl CollectorJob {
    /// Creates a scheduled collector job.
    pub fn new(collector: Arc<dyn Collector>, interval: CollectionInterval) -> Self {
        Self {
            collector,
            interval,
        }
    }

    /// Returns the collector identity.
    pub fn id(&self) -> &CollectorId {
        self.collector.id()
    }

    /// Returns the configured cadence.
    pub fn interval(&self) -> CollectionInterval {
        self.interval
    }
}

impl fmt::Debug for CollectorJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollectorJob")
            .field("collector_id", self.id())
            .field("interval", &self.interval)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerSettings {
    pub collector_timeout: Duration,
    pub max_concurrency: usize,
}

#[derive(Debug, Clone, Copy)]
enum ScheduleState {
    Waiting(Instant),
    Running,
}

#[derive(Debug)]
struct CollectorSchedule {
    job: CollectorJob,
    state: ScheduleState,
}

impl CollectorSchedule {
    fn new(job: CollectorJob, first_run: Instant) -> Self {
        Self {
            job,
            state: ScheduleState::Waiting(first_run),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self.state {
            ScheduleState::Waiting(deadline) => Some(deadline),
            ScheduleState::Running => None,
        }
    }

    fn take_if_due(&mut self, now: Instant) -> Option<CollectorJob> {
        match self.state {
            ScheduleState::Waiting(deadline) if deadline <= now => {
                self.state = ScheduleState::Running;
                Some(self.job.clone())
            }
            ScheduleState::Waiting(_) | ScheduleState::Running => None,
        }
    }

    fn finish(&mut self, completed_at: Instant) -> Result<(), SchedulerError> {
        if matches!(self.state, ScheduleState::Running) {
            let deadline = completed_at
                .checked_add(self.job.interval().duration())
                .ok_or(SchedulerError::DeadlineOverflow)?;
            self.state = ScheduleState::Waiting(deadline);
        }
        Ok(())
    }
}

pub async fn run_scheduler(
    jobs: Vec<CollectorJob>,
    settings: SchedulerSettings,
    snapshots: watch::Sender<Arc<Snapshot>>,
    cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
) -> Result<(), SchedulerError> {
    if jobs.is_empty() {
        return Err(SchedulerError::NoCollectors);
    }
    let concurrency = settings.max_concurrency.max(1);
    let first_run = Instant::now();
    let mut schedules = jobs
        .into_iter()
        .map(|job| CollectorSchedule::new(job, first_run))
        .collect::<Vec<_>>();
    let mut latest = BTreeMap::<CollectorId, Observation>::new();
    let mut sequence = 0_u64;
    let mut history = CounterHistory::default();

    loop {
        let deadline = next_deadline(&schedules)?;
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep_until(deadline) => {}
        }
        let due_at = Instant::now();
        let due = schedules
            .iter_mut()
            .filter_map(|schedule| schedule.take_if_due(due_at))
            .collect::<Vec<_>>();
        if due.is_empty() {
            continue;
        }
        let mut observations = match collect_once_with_cancellation(
            due,
            settings.collector_timeout,
            concurrency,
            cancellation.clone(),
            Arc::clone(&clock),
        )
        .await
        {
            Ok(observations) => observations,
            Err(SchedulerError::Cancelled) => return Ok(()),
            Err(error) => return Err(error),
        };
        history.add_deltas(&mut observations)?;
        merge_latest(&mut latest, observations);
        let completed_at = Instant::now();
        for schedule in &mut schedules {
            schedule.finish(completed_at)?;
        }
        sequence = sequence
            .checked_add(1)
            .ok_or(SchedulerError::SequenceOverflow)?;
        let snapshot = Snapshot {
            schema_version: 1,
            sequence,
            generated_at: clock.now(),
            observations: latest.values().cloned().collect(),
        };
        snapshots
            .send(Arc::new(snapshot))
            .map_err(|_| SchedulerError::NoSnapshotReceivers)?;
    }
}

pub async fn collect_once(
    jobs: Vec<CollectorJob>,
    timeout: Duration,
    concurrency: usize,
) -> Result<Vec<Observation>, SchedulerError> {
    collect_once_with_cancellation(
        jobs,
        timeout,
        concurrency,
        CancellationToken::new(),
        Arc::new(SystemClock),
    )
    .await
}

fn next_deadline(schedules: &[CollectorSchedule]) -> Result<Instant, SchedulerError> {
    schedules
        .iter()
        .filter_map(CollectorSchedule::deadline)
        .min()
        .ok_or(SchedulerError::NoPendingDeadline)
}

fn merge_latest(latest: &mut BTreeMap<CollectorId, Observation>, observations: Vec<Observation>) {
    for observation in observations {
        latest.insert(observation.collector_id.clone(), observation);
    }
}

async fn collect_once_with_cancellation(
    jobs: Vec<CollectorJob>,
    timeout: Duration,
    concurrency: usize,
    cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
) -> Result<Vec<Observation>, SchedulerError> {
    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut tasks = JoinSet::new();
    for job in jobs {
        let limiter = Arc::clone(&semaphore);
        let collector_clock = Arc::clone(&clock);
        tasks.spawn(async move {
            let _permit = limiter
                .acquire_owned()
                .await
                .map_err(|_| SchedulerError::ConcurrencyLimiterClosed)?;
            collect_one(job, timeout, collector_clock).await
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
    job: CollectorJob,
    timeout: Duration,
    clock: Arc<dyn Clock>,
) -> Result<Observation, SchedulerError> {
    let started_at = clock.now();
    let started = Instant::now();
    let collector_id = job.id().clone();
    let result = match tokio::time::timeout(timeout, job.collector.collect()).await {
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
        interval: job.interval(),
        collected_at: started_at,
        duration_ms,
        result,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::collector::{Collection, CollectorError};

    #[derive(Debug)]
    struct FailingCollector {
        id: CollectorId,
    }

    #[derive(Debug)]
    struct PendingCollector {
        id: CollectorId,
    }

    #[derive(Debug)]
    struct CountingCollector {
        id: CollectorId,
        collections: Arc<AtomicUsize>,
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

    #[async_trait]
    impl Collector for CountingCollector {
        fn id(&self) -> &CollectorId {
            &self.id
        }

        async fn collect(&self) -> Result<Collection, CollectorError> {
            self.collections.fetch_add(1, Ordering::SeqCst);
            Ok(Collection::new(Vec::new(), Vec::new()))
        }
    }

    #[tokio::test]
    async fn test_scheduler_runs_only_due_collectors_and_preserves_cached_observations() {
        // Arrange
        let fast_count = Arc::new(AtomicUsize::new(0));
        let slow_count = Arc::new(AtomicUsize::new(0));
        let jobs = vec![
            counting_job("fast", 1, Arc::clone(&fast_count)),
            counting_job("slow", 60, Arc::clone(&slow_count)),
        ];
        let cancellation = CancellationToken::new();
        let (sender, mut receiver) = watch::channel(Arc::new(Snapshot::empty(chrono::Utc::now())));
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(run_scheduler(
            jobs,
            SchedulerSettings {
                collector_timeout: Duration::from_secs(1),
                max_concurrency: 2,
            },
            sender,
            task_cancellation,
            Arc::new(SystemClock),
        ));

        // Act
        let _first = next_snapshot(&mut receiver).await;
        let second = next_snapshot(&mut receiver).await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("scheduler shuts down")
            .expect("scheduler task joins");

        // Assert
        assert!(result.is_ok());
        assert_eq!(fast_count.load(Ordering::SeqCst), 2);
        assert_eq!(slow_count.load(Ordering::SeqCst), 1);
        assert_eq!(second.observations.len(), 2);
    }

    #[tokio::test]
    async fn test_cycle_records_collector_failure_and_interval() {
        // Arrange
        let jobs = vec![failing_job("fixture", 30)];

        // Act
        let observations = collect_once(jobs, Duration::from_secs(1), 1)
            .await
            .expect("cycle should complete");

        // Assert
        assert!(matches!(
            observations.first(),
            Some(Observation { interval, result: ObservationResult::Failed { code, .. }, .. })
                if code == "fixture" && interval.as_secs() == 30
        ));
    }

    #[tokio::test]
    async fn test_cycle_drains_tasks_when_cancelled() {
        // Arrange
        let jobs = vec![pending_job("pending", 30)];
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        // Act
        let result = collect_once_with_cancellation(
            jobs,
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
        let jobs = vec![pending_job("pending", 30)];

        // Act
        let observations = collect_once(jobs, Duration::from_millis(1), 1)
            .await
            .expect("cycle should complete");

        // Assert
        assert!(matches!(
            observations.first().map(|item| &item.result),
            Some(ObservationResult::Failed { code, .. }) if code == "timeout"
        ));
    }

    #[test]
    fn test_collector_schedule_waits_for_its_own_interval_after_completion() {
        // Arrange
        let started_at = Instant::now();
        let completed_at = started_at
            .checked_add(Duration::from_secs(2))
            .expect("valid fixture deadline");
        let expected = completed_at
            .checked_add(Duration::from_secs(60))
            .expect("valid fixture deadline");
        let mut schedule = CollectorSchedule::new(failing_job("disk", 60), started_at);
        let _ = schedule.take_if_due(started_at);

        // Act
        schedule.finish(completed_at).expect("valid deadline");

        // Assert
        assert_eq!(schedule.deadline(), Some(expected));
    }

    fn failing_job(id: &str, interval_secs: u64) -> CollectorJob {
        CollectorJob::new(
            Arc::new(FailingCollector {
                id: CollectorId::new(id).expect("valid fixture"),
            }),
            CollectionInterval::from_secs(interval_secs).expect("valid fixture"),
        )
    }

    fn pending_job(id: &str, interval_secs: u64) -> CollectorJob {
        CollectorJob::new(
            Arc::new(PendingCollector {
                id: CollectorId::new(id).expect("valid fixture"),
            }),
            CollectionInterval::from_secs(interval_secs).expect("valid fixture"),
        )
    }

    fn counting_job(id: &str, interval_secs: u64, collections: Arc<AtomicUsize>) -> CollectorJob {
        CollectorJob::new(
            Arc::new(CountingCollector {
                id: CollectorId::new(id).expect("valid fixture"),
                collections,
            }),
            CollectionInterval::from_secs(interval_secs).expect("valid fixture"),
        )
    }

    async fn next_snapshot(receiver: &mut watch::Receiver<Arc<Snapshot>>) -> Arc<Snapshot> {
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .expect("snapshot arrives")
            .expect("snapshot channel remains open");
        receiver.borrow().clone()
    }
}
