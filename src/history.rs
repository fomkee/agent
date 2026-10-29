use std::{collections::BTreeMap, ops::Sub};

use thiserror::Error;

use crate::model::{
    CollectorId, Measurement, MeasurementKind, MeasurementName, Observation, ObservationResult,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    collector_id: CollectorId,
    measurement_name: MeasurementName,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("failed to construct derived counter delta: {0}")]
    InvalidDelta(#[from] crate::model::ModelError),
    #[error("derived measurement collides with a collector measurement: {0}")]
    DeltaCollision(MeasurementName),
}

#[derive(Debug, Default)]
pub struct CounterHistory {
    previous: BTreeMap<SeriesKey, f64>,
}

impl CounterHistory {
    pub fn add_deltas(&mut self, observations: &mut [Observation]) -> Result<(), HistoryError> {
        for observation in observations {
            let ObservationResult::Collected { measurements, .. } = &mut observation.result else {
                continue;
            };
            let mut derived = Vec::new();
            for measurement in measurements.iter() {
                if measurement.kind != MeasurementKind::Counter || !measurement.labels.is_empty() {
                    continue;
                }
                let key = SeriesKey {
                    collector_id: observation.collector_id.clone(),
                    measurement_name: measurement.name.clone(),
                };
                let current = measurement.value.get();
                if let Some(previous) = self.previous.insert(key, current) {
                    let delta = if current >= previous {
                        current.sub(previous)
                    } else {
                        current
                    };
                    let name = MeasurementName::new(format!("{}_delta", measurement.name))?;
                    if measurements.iter().any(|item| item.name == name) {
                        return Err(HistoryError::DeltaCollision(name));
                    }
                    derived.push(Measurement::new(
                        name,
                        MeasurementKind::Gauge,
                        measurement.unit,
                        crate::model::FiniteValue::new(delta)?,
                        Default::default(),
                    )?);
                }
            }
            measurements.extend(derived);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::unreachable)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{CollectionInterval, Unit};

    fn observation(value: f64) -> Observation {
        Observation {
            collector_id: CollectorId::new("queue").expect("valid fixture"),
            interval: CollectionInterval::from_secs(30).expect("valid fixture"),
            collected_at: Utc::now(),
            duration_ms: 1,
            result: ObservationResult::Collected {
                measurements: vec![
                    Measurement::scalar(
                        "dead_letters_total",
                        MeasurementKind::Counter,
                        Unit::Count,
                        value,
                    )
                    .expect("valid fixture"),
                ],
                facts: Vec::new(),
            },
        }
    }

    #[test]
    fn test_counter_delta_uses_current_value_after_reset() {
        // Arrange
        let mut history = CounterHistory::default();
        history
            .add_deltas(&mut [observation(10.0)])
            .expect("valid history");
        let mut current = [observation(3.0)];

        // Act
        history.add_deltas(&mut current).expect("valid history");

        // Assert
        let ObservationResult::Collected { measurements, .. } = &current[0].result else {
            unreachable!("fixture must be collected");
        };
        assert_eq!(measurements[1].value.get(), 3.0);
    }
}
