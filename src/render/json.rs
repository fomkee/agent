use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{Fact, FactValue, ObservationResult, Snapshot};

#[derive(Debug, Serialize)]
pub struct CompactSnapshot<'a> {
    pub schema_version: u16,
    pub sequence: u64,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub collectors: BTreeMap<&'a str, CompactCollector<'a>>,
}

#[derive(Debug, Serialize)]
pub struct CompactCollector<'a> {
    pub collected_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    #[serde(flatten)]
    pub state: CompactCollectorState<'a>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompactCollectorState<'a> {
    Collected {
        values: BTreeMap<String, f64>,
        facts: BTreeMap<&'a str, CompactFact<'a>>,
    },
    Failed {
        code: &'a str,
        summary: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CompactFact<'a> {
    Boolean(bool),
    Text(&'a str),
    Timestamp(chrono::DateTime<chrono::Utc>),
}

pub fn compact(snapshot: &Snapshot) -> CompactSnapshot<'_> {
    let collectors = snapshot
        .observations
        .iter()
        .map(|observation| {
            let state = match &observation.result {
                ObservationResult::Collected {
                    measurements,
                    facts,
                } => CompactCollectorState::Collected {
                    values: measurements
                        .iter()
                        .map(|measurement| {
                            let name = if measurement.labels.is_empty() {
                                measurement.name.to_string()
                            } else {
                                let suffix = measurement
                                    .labels
                                    .iter()
                                    .map(|(name, value)| format!("{name}={value}"))
                                    .collect::<Vec<_>>()
                                    .join(",");
                                format!("{}{{{suffix}}}", measurement.name)
                            };
                            (name, measurement.value.get())
                        })
                        .collect(),
                    facts: facts.iter().map(compact_fact).collect(),
                },
                ObservationResult::Failed { code, summary } => {
                    CompactCollectorState::Failed { code, summary }
                }
            };
            (
                observation.collector_id.as_str(),
                CompactCollector {
                    collected_at: observation.collected_at,
                    duration_ms: observation.duration_ms,
                    state,
                },
            )
        })
        .collect();
    CompactSnapshot {
        schema_version: snapshot.schema_version,
        sequence: snapshot.sequence,
        generated_at: snapshot.generated_at,
        collectors,
    }
}

fn compact_fact(fact: &Fact) -> (&str, CompactFact<'_>) {
    let value = match &fact.value {
        FactValue::Boolean(value) => CompactFact::Boolean(*value),
        FactValue::Text(value) => CompactFact::Text(value.as_str()),
        FactValue::Timestamp(value) => CompactFact::Timestamp(*value),
    };
    (fact.name.as_str(), value)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{CollectorId, Measurement, MeasurementKind, Observation, Unit};

    #[test]
    fn test_compact_json_omits_measurement_metadata() {
        // Arrange
        let snapshot = Snapshot {
            schema_version: 1,
            sequence: 2,
            generated_at: Utc::now(),
            observations: vec![Observation {
                collector_id: CollectorId::new("disk").expect("valid fixture"),
                collected_at: Utc::now(),
                duration_ms: 4,
                result: ObservationResult::Collected {
                    measurements: vec![
                        Measurement::scalar("used_ratio", MeasurementKind::Gauge, Unit::Ratio, 0.5)
                            .expect("valid fixture"),
                    ],
                    facts: Vec::new(),
                },
            }],
        };

        // Act
        let value = serde_json::to_value(compact(&snapshot)).expect("serializable fixture");

        // Assert
        assert_eq!(value["collectors"]["disk"]["values"]["used_ratio"], 0.5);
        assert!(value.to_string().find("kind").is_none());
    }
}
