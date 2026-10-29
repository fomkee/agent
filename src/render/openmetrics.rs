use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    time::Duration,
};

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
    let mut families = BTreeMap::<String, (&str, Vec<String>)>::new();
    let mut series = BTreeSet::new();
    for observation in &snapshot.observations {
        let status = match &observation.result {
            ObservationResult::Collected { measurements, .. } => {
                for measurement in measurements {
                    let name = metric_name(
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
                    let sample = if labels.is_empty() {
                        name.clone()
                    } else {
                        format!("{name}{{{labels}}}")
                    };
                    let (family, kind) = match measurement.kind {
                        MeasurementKind::Gauge => (name.as_str(), "gauge"),
                        MeasurementKind::Counter => {
                            (name.strip_suffix("_total").unwrap_or(&name), "counter")
                        }
                    };
                    add_sample(
                        &mut families,
                        &mut series,
                        family,
                        kind,
                        sample,
                        measurement.value.get(),
                    )?;
                }
                1.0
            }
            ObservationResult::Failed { .. } => 0.0,
        };
        let collector = escape_label(observation.collector_id.as_str());
        add_sample(
            &mut families,
            &mut series,
            "fomkee_agent_collector_up",
            "gauge",
            format!("fomkee_agent_collector_up{{collector=\"{collector}\"}}"),
            status,
        )?;
        add_sample(
            &mut families,
            &mut series,
            "fomkee_agent_collector_duration_seconds",
            "gauge",
            format!("fomkee_agent_collector_duration_seconds{{collector=\"{collector}\"}}"),
            duration_seconds(observation.duration_ms),
        )?;
        add_sample(
            &mut families,
            &mut series,
            "fomkee_agent_collector_interval_seconds",
            "gauge",
            format!("fomkee_agent_collector_interval_seconds{{collector=\"{collector}\"}}"),
            observation.interval.duration().as_secs_f64(),
        )?;
        add_sample(
            &mut families,
            &mut series,
            "fomkee_agent_collector_last_collection_timestamp_seconds",
            "gauge",
            format!(
                "fomkee_agent_collector_last_collection_timestamp_seconds{{collector=\"{collector}\"}}"
            ),
            observation.collected_at.timestamp() as f64,
        )?;
    }
    let mut output = String::new();
    for (name, (kind, samples)) in families {
        writeln!(output, "# TYPE {name} {kind}").map_err(|_| OpenMetricsError::Formatting)?;
        for sample in samples {
            writeln!(output, "{sample}").map_err(|_| OpenMetricsError::Formatting)?;
        }
    }
    output.push_str("# EOF\n");
    Ok(output)
}

fn add_sample<'a>(
    families: &mut BTreeMap<String, (&'a str, Vec<String>)>,
    series: &mut BTreeSet<String>,
    family: &str,
    kind: &'a str,
    sample: String,
    value: f64,
) -> Result<(), OpenMetricsError> {
    if !series.insert(sample.clone()) {
        return Err(OpenMetricsError::DuplicateSeries(sample));
    }
    let (existing_kind, samples) = families
        .entry(family.to_owned())
        .or_insert((kind, Vec::new()));
    if *existing_kind != kind {
        return Err(OpenMetricsError::DuplicateSeries(family.to_owned()));
    }
    samples.push(format!("{sample} {value}"));
    Ok(())
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
    use crate::model::{CollectionInterval, CollectorId, Measurement, Observation};

    fn failed_observation(id: &str) -> Observation {
        Observation {
            collector_id: CollectorId::new(id).expect("valid fixture"),
            interval: CollectionInterval::from_secs(30).expect("valid fixture"),
            collected_at: Utc::now(),
            duration_ms: 1,
            result: ObservationResult::Failed {
                code: "fixture".to_owned(),
                summary: "expected failure".to_owned(),
            },
        }
    }

    #[test]
    fn test_collector_samples_are_grouped_in_one_metric_family() {
        // Arrange
        let snapshot = Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: Utc::now(),
            observations: vec![failed_observation("one"), failed_observation("two")],
        };

        // Act
        let output = render(&snapshot).expect("snapshot renders");

        // Assert
        assert!(output.contains(
            "# TYPE fomkee_agent_collector_up gauge\nfomkee_agent_collector_up{collector=\"one\"} 0\nfomkee_agent_collector_up{collector=\"two\"} 0\n"
        ));
        assert_eq!(
            output.matches("# TYPE fomkee_agent_collector_up ").count(),
            1
        );
    }

    #[test]
    fn test_counter_unit_is_rendered_before_total_suffix() {
        // Arrange
        let snapshot = Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: Utc::now(),
            observations: vec![Observation {
                collector_id: CollectorId::new("postgres").expect("valid fixture"),
                interval: CollectionInterval::from_secs(30).expect("valid fixture"),
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
        assert!(output.contains("# TYPE fomkee_agent_postgres_temp_bytes counter\nfomkee_agent_postgres_temp_bytes_total 42"));
    }

    #[test]
    fn test_exposes_collector_cadence_and_last_collection_time() {
        // Arrange
        let observation = failed_observation("disk");
        let expected_timestamp = observation.collected_at.timestamp().to_string();
        let snapshot = Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: Utc::now(),
            observations: vec![observation],
        };

        // Act
        let output = render(&snapshot).expect("snapshot renders");

        // Assert
        assert!(output.contains("fomkee_agent_collector_interval_seconds{collector=\"disk\"} 30"));
        assert!(output.contains(&format!(
            "fomkee_agent_collector_last_collection_timestamp_seconds{{collector=\"disk\"}} {expected_timestamp}"
        )));
    }
}
