use std::ops::{Add, Div, Mul, Sub};

use crate::config::CapacityThresholds;
use crate::model::{CollectorId, FactValue, Observation, ObservationResult, Snapshot, Unit};

use super::super::{format, history::SnapshotHistory};

const BAR_WIDTH: usize = 24;
const SPARK_CHARACTERS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CapacityState {
    Normal,
    Warning,
    Critical,
}

pub(super) fn observation<'a>(
    snapshot: &'a Snapshot,
    collector_id: &CollectorId,
) -> Option<&'a Observation> {
    snapshot
        .observations
        .iter()
        .find(|observation| &observation.collector_id == collector_id)
}

pub(super) fn measurement(observation: &Observation, name: &str) -> Option<f64> {
    let ObservationResult::Collected { measurements, .. } = &observation.result else {
        return None;
    };
    measurements
        .iter()
        .find(|measurement| measurement.name.as_str() == name)
        .map(|measurement| measurement.value.get())
}

pub(super) fn fact<'a>(observation: &'a Observation, name: &str) -> Option<&'a FactValue> {
    let ObservationResult::Collected { facts, .. } = &observation.result else {
        return None;
    };
    facts
        .iter()
        .find(|fact| fact.name.as_str() == name)
        .map(|fact| &fact.value)
}

pub(super) fn formatted(observation: &Observation, name: &str, unit: Unit) -> String {
    measurement(observation, name).map_or_else(
        || "unavailable".to_owned(),
        |value| format::measurement(value, unit),
    )
}

pub(super) fn fact_text(observation: &Observation, name: &str) -> String {
    fact(observation, name).map_or_else(|| "unavailable".to_owned(), format::fact)
}

pub(super) fn capacity(used: f64, total: f64) -> String {
    if total <= 0.0 {
        return "capacity unavailable".to_owned();
    }
    let ratio = used.div(total).clamp(0.0, 1.0);
    let filled = ratio.mul(BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH.saturating_sub(filled);
    format!(
        "{}{} {}",
        "█".repeat(filled),
        "░".repeat(empty),
        format::measurement(ratio, Unit::Ratio)
    )
}

pub(super) fn capacity_ratio(used: f64, total: f64) -> Option<f64> {
    (total > 0.0).then(|| used.div(total).clamp(0.0, 1.0))
}

pub(super) fn capacity_state(ratio: f64, thresholds: CapacityThresholds) -> CapacityState {
    let percent = ratio.mul(100.0);
    if percent >= f64::from(thresholds.critical_percent) {
        CapacityState::Critical
    } else if percent >= f64::from(thresholds.warning_percent) {
        CapacityState::Warning
    } else {
        CapacityState::Normal
    }
}

pub(super) fn capacity_bar(ratio: f64, width: usize) -> (String, String) {
    let filled = ratio.clamp(0.0, 1.0).mul(width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    ("█".repeat(filled), "░".repeat(empty))
}

pub(super) fn series(
    history: &SnapshotHistory,
    collector_id: &CollectorId,
    name: &str,
) -> Vec<f64> {
    let mut previous_collection = None;
    history
        .iter()
        .filter_map(|snapshot| observation(snapshot, collector_id))
        .filter(|observation| {
            if previous_collection == Some(observation.collected_at) {
                false
            } else {
                previous_collection = Some(observation.collected_at);
                true
            }
        })
        .filter_map(|observation| measurement(observation, name))
        .collect()
}

pub(super) fn sparkline(series: &[f64]) -> String {
    let minimum = series.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = series.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !minimum.is_finite() {
        return "collecting".to_owned();
    }
    series
        .iter()
        .map(|value| spark_character(*value, minimum, maximum))
        .collect()
}

pub(super) fn range(series: &[f64], unit: Unit) -> String {
    let minimum = series.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = series.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if minimum.is_finite() {
        format!(
            "min {} · max {}",
            format::measurement(minimum, unit),
            format::measurement(maximum, unit)
        )
    } else {
        "range unavailable".to_owned()
    }
}

pub(super) fn session_change(
    history: &SnapshotHistory,
    collector_id: &CollectorId,
    name: &str,
    unit: Unit,
) -> String {
    let series = series(history, collector_id, name);
    let Some(first) = series.first() else {
        return "unavailable".to_owned();
    };
    let Some(last) = series.last() else {
        return "unavailable".to_owned();
    };
    let delta = last.sub(*first);
    let formatted = format::measurement(delta, unit);
    if delta > 0.0 {
        format!("+{formatted}")
    } else {
        formatted
    }
}

pub(super) fn session_activity(
    history: &SnapshotHistory,
    collector_id: &CollectorId,
    delta_name: &str,
    unit: Unit,
) -> String {
    let total = series(history, collector_id, delta_name)
        .into_iter()
        .fold(0.0, f64::add);
    format::measurement(total, unit)
}

pub(super) fn history_duration(history: &SnapshotHistory) -> String {
    let first = history.iter().next();
    let last = history.iter().next_back();
    match (first, last) {
        (Some(first), Some(last)) => last
            .generated_at
            .signed_duration_since(first.generated_at)
            .to_std()
            .map_or_else(
                |_| "starting".to_owned(),
                |duration| format::measurement(duration.as_secs_f64(), Unit::Seconds),
            ),
        _ => "starting".to_owned(),
    }
}

fn spark_character(value: f64, minimum: f64, maximum: f64) -> char {
    if minimum == maximum {
        return SPARK_CHARACTERS.first().copied().unwrap_or('▁');
    }
    let normalized = value.sub(minimum).div(maximum.sub(minimum));
    let index = normalized
        .mul(SPARK_CHARACTERS.len().saturating_sub(1) as f64)
        .round() as usize;
    SPARK_CHARACTERS.get(index).copied().unwrap_or('▁')
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::model::{CollectionInterval, Measurement, MeasurementKind};

    #[test]
    fn test_capacity_includes_full_scale_context() {
        // Arrange
        let used = 75.0;
        let total = 100.0;

        // Act
        let rendered = capacity(used, total);

        // Assert
        assert!(rendered.contains("75%"));
        assert!(rendered.contains('█'));
        assert!(rendered.contains('░'));
    }

    #[test]
    fn test_flat_series_remains_visibly_flat() {
        // Arrange
        let values = [4.0, 4.0, 4.0];

        // Act
        let rendered = sparkline(&values);

        // Assert
        assert_eq!(rendered, "▁▁▁");
    }

    #[test]
    fn test_series_ignores_republished_cached_observation() {
        // Arrange
        let collector_id = CollectorId::new("disk").expect("valid fixture");
        let cached = observation(&collector_id, 42.0, 1);
        let mut history = SnapshotHistory::new(3);
        history.push(snapshot(1, cached.clone()));
        history.push(snapshot(2, cached));
        history.push(snapshot(3, observation(&collector_id, 43.0, 2)));

        // Act
        let values = series(&history, &collector_id, "used");

        // Assert
        assert_eq!(values, vec![42.0, 43.0]);
    }

    fn observation(collector_id: &CollectorId, value: f64, second: u32) -> Observation {
        Observation {
            collector_id: collector_id.clone(),
            interval: CollectionInterval::from_secs(60).expect("valid fixture"),
            collected_at: Utc
                .with_ymd_and_hms(2026, 10, 21, 10, 0, second)
                .single()
                .expect("valid fixture"),
            duration_ms: 1,
            result: ObservationResult::Collected {
                measurements: vec![
                    Measurement::scalar("used", MeasurementKind::Gauge, Unit::Bytes, value)
                        .expect("valid fixture"),
                ],
                facts: Vec::new(),
            },
        }
    }

    fn snapshot(sequence: u64, observation: Observation) -> Snapshot {
        Snapshot {
            schema_version: 1,
            sequence,
            generated_at: observation.collected_at,
            observations: vec![observation],
        }
    }
}
