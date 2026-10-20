use std::{collections::BTreeSet, fmt::Write, time::Duration};

use thiserror::Error;

use crate::model::{MeasurementKind, ObservationResult, Snapshot, Unit};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OpenMetricsError {
    #[error("two measurements render to the same OpenMetrics series: {0}")]
    DuplicateSeries(String),
    #[error("failed to format OpenMetrics output")]
    Formatting,
}

pub fn render(snapshot: &Snapshot) -> Result<String, OpenMetricsError> {
    let mut output = String::new();
    let mut series = BTreeSet::new();
    for observation in &snapshot.observations {
        let status = match &observation.result {
            ObservationResult::Collected { measurements, .. } => {
                for measurement in measurements {
                    let metric_name = metric_name(
                        observation.collector_id.as_str(),
                        measurement.name.as_str(),
                        measurement.kind,
                        measurement.unit,
                    );
                    let labels = measurement
                        .labels
                        .iter()
                        .map(|(name, value)| {
                            format!("{}=\"{}\"", normalize(name.as_str()), escape_label(value))
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    let key = format!("{metric_name}{{{labels}}}");
                    if !series.insert(key.clone()) {
                        return Err(OpenMetricsError::DuplicateSeries(key));
                    }
                    let metric_type = match measurement.kind {
                        MeasurementKind::Gauge => "gauge",
                        MeasurementKind::Counter => "counter",
                    };
                    writeln!(output, "# TYPE {metric_name} {metric_type}")
                        .map_err(|_| OpenMetricsError::Formatting)?;
                    if labels.is_empty() {
                        writeln!(output, "{metric_name} {}", measurement.value.get())
                            .map_err(|_| OpenMetricsError::Formatting)?;
                    } else {
                        writeln!(
                            output,
                            "{metric_name}{{{labels}}} {}",
                            measurement.value.get()
                        )
                        .map_err(|_| OpenMetricsError::Formatting)?;
                    }
                }
                1
            }
            ObservationResult::Failed { .. } => 0,
        };
        writeln!(
            output,
            "fomkee_agent_collector_up{{collector=\"{}\"}} {status}",
            escape_label(observation.collector_id.as_str())
        )
        .map_err(|_| OpenMetricsError::Formatting)?;
        writeln!(
            output,
            "fomkee_agent_collector_duration_seconds{{collector=\"{}\"}} {}",
            escape_label(observation.collector_id.as_str()),
            duration_seconds(observation.duration_ms)
        )
        .map_err(|_| OpenMetricsError::Formatting)?;
    }
    output.push_str("# EOF\n");
    Ok(output)
}

fn metric_name(collector: &str, measurement: &str, kind: MeasurementKind, unit: Unit) -> String {
    let suffix = match unit {
        Unit::Bytes => "_bytes",
        Unit::Seconds => "_seconds",
        Unit::Ratio | Unit::Count | Unit::None => "",
    };
    let normalized = normalize(measurement);
    let (base, counter_suffix) = match kind {
        MeasurementKind::Counter => (
            normalized.strip_suffix("_total").unwrap_or(&normalized),
            "_total",
        ),
        MeasurementKind::Gauge => (normalized.as_str(), ""),
    };
    let unit_suffix = if suffix.is_empty() || base.ends_with(suffix) {
        ""
    } else {
        suffix
    };
    format!(
        "fomkee_agent_{}_{}{}{}",
        normalize(collector),
        base,
        unit_suffix,
        counter_suffix
    )
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'A'..='Z' => character.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '_' => character,
            '-' => '_',
            _ => '_',
        })
        .collect()
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn duration_seconds(duration_ms: u64) -> f64 {
    Duration::from_millis(duration_ms).as_secs_f64()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{CollectorId, Measurement, Observation};

    #[test]
    fn test_counter_unit_is_rendered_before_total_suffix() {
        // Arrange
        let snapshot = Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: Utc::now(),
            observations: vec![Observation {
                collector_id: CollectorId::new("postgres").expect("valid fixture"),
                collected_at: Utc::now(),
                duration_ms: 1,
                result: ObservationResult::Collected {
                    measurements: vec![
                        Measurement::scalar(
                            "temp_bytes_total",
                            MeasurementKind::Counter,
                            Unit::Bytes,
                            42.0,
                        )
                        .expect("valid fixture"),
                    ],
                    facts: Vec::new(),
                },
            }],
        };

        // Act
        let output = render(&snapshot).expect("snapshot renders");

        // Assert
        assert!(output.contains("fomkee_agent_postgres_temp_bytes_total 42"));
    }
}
