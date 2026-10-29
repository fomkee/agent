use std::ops::Add;

use async_trait::async_trait;
use reqwest::Client;
use url::Url;

use crate::{
    collector::{Collection, Collector, CollectorError},
    config::OpenMetricsMapping,
    model::{CollectorId, FiniteValue, Measurement},
};

use super::http_source::{self, SourceAuth};

#[derive(Debug)]
pub struct OpenMetricsCollector {
    id: CollectorId,
    url: Url,
    auth: Option<SourceAuth>,
    mappings: Vec<OpenMetricsMapping>,
    client: Client,
}

impl OpenMetricsCollector {
    pub fn new(
        id: CollectorId,
        url: Url,
        auth: Option<SourceAuth>,
        mappings: Vec<OpenMetricsMapping>,
        client: Client,
    ) -> Self {
        Self {
            id,
            url,
            auth,
            mappings,
            client,
        }
    }
}

#[async_trait]
impl Collector for OpenMetricsCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let body = http_source::fetch(&self.client, &self.url, self.auth.as_ref()).await?;
        let source = std::str::from_utf8(&body)
            .map_err(|error| CollectorError::new("openmetrics_parse", error.to_string()))?;
        let mut measurements = Vec::new();
        for mapping in &self.mappings {
            if let Some(measurement) = map_series(source, mapping)? {
                measurements.push(measurement);
            }
        }
        Ok(Collection::new(measurements, Vec::new()))
    }
}

fn map_series(
    source: &str,
    mapping: &OpenMetricsMapping,
) -> Result<Option<Measurement>, CollectorError> {
    let mut total: Option<f64> = None;
    for line in source.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let Some(series) = fields.next() else {
            continue;
        };
        let name = series.split_once('{').map_or(series, |(name, _)| name);
        if name != mapping.source || !series_labels_match(series, &mapping.labels) {
            continue;
        }
        let value = fields
            .next()
            .ok_or_else(|| CollectorError::new("openmetrics_parse", "sample has no value"))?
            .parse::<f64>()
            .map_err(|error| CollectorError::new("openmetrics_parse", error.to_string()))?;
        total = Some(total.map_or(value, |current| current.add(value)));
    }
    let Some(value) = total else {
        if mapping.required {
            return Err(CollectorError::new(
                "openmetrics_mapping",
                format!("source metric {} was not found", mapping.source),
            ));
        }
        return Ok(None);
    };
    Measurement::new(
        mapping.name.clone(),
        mapping.kind,
        mapping.unit,
        FiniteValue::new(value)
            .map_err(|error| CollectorError::new("openmetrics_mapping", error.to_string()))?,
        Default::default(),
    )
    .map(Some)
    .map_err(|error| CollectorError::new("openmetrics_mapping", error.to_string()))
}

fn series_labels_match(
    series: &str,
    expected: &std::collections::BTreeMap<String, String>,
) -> bool {
    if expected.is_empty() {
        return true;
    }
    let Some((_, labels)) = series.split_once('{') else {
        return false;
    };
    let Some(labels) = labels.strip_suffix('}') else {
        return false;
    };
    let tokens = label_tokens(labels);
    expected.iter().all(|(name, value)| {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('"', "\\\"");
        let wanted = format!("{name}=\"{escaped}\"");
        tokens.iter().any(|token| token == &wanted)
    })
}

fn label_tokens(labels: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in labels.chars() {
        match character {
            ',' if !quoted => {
                tokens.push(token.trim().to_owned());
                token.clear();
            }
            '"' if !escaped => {
                quoted = !quoted;
                token.push(character);
            }
            _ => token.push(character),
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    if !token.is_empty() {
        tokens.push(token.trim().to_owned());
    }
    tokens
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::{MeasurementKind, MeasurementName, Unit};

    #[test]
    fn test_mapping_sums_labeled_samples() {
        // Arrange
        let source = "queue{protocol=\"v1\"} 2\nqueue{protocol=\"v2\"} 3\n";
        let mapping = OpenMetricsMapping {
            source: "queue".to_owned(),
            labels: Default::default(),
            required: true,
            name: MeasurementName::new("pending").expect("valid fixture"),
            kind: MeasurementKind::Gauge,
            unit: Unit::Count,
        };

        // Act
        let measurement = map_series(source, &mapping)
            .expect("valid fixture")
            .expect("present fixture");

        // Assert
        assert_eq!(measurement.value.get(), 5.0);
    }

    #[test]
    fn test_mapping_rejects_malformed_sample() {
        // Arrange
        let mapping = OpenMetricsMapping {
            source: "queue".to_owned(),
            labels: Default::default(),
            required: true,
            name: MeasurementName::new("pending").expect("valid fixture"),
            kind: MeasurementKind::Gauge,
            unit: Unit::Count,
        };

        // Act
        let result = map_series("queue not-a-number\n", &mapping);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_mapping_selects_matching_labels() {
        // Arrange
        let source = concat!(
            "flush{outcome=\"selected\",node=\"worker\"} 7\n",
            "flush{outcome=\"acknowledged\",node=\"worker\"} 5\n",
        );
        let mapping = OpenMetricsMapping {
            source: "flush".to_owned(),
            labels: [("outcome".to_owned(), "acknowledged".to_owned())]
                .into_iter()
                .collect(),
            required: true,
            name: MeasurementName::new("drained_total").expect("valid fixture"),
            kind: MeasurementKind::Counter,
            unit: Unit::Count,
        };

        // Act
        let measurement = map_series(source, &mapping)
            .expect("valid fixture")
            .expect("present fixture");

        // Assert
        assert_eq!(measurement.value.get(), 5.0);
    }

    #[test]
    fn test_optional_mapping_omits_absent_series() {
        // Arrange
        let mapping = OpenMetricsMapping {
            source: "oldest_due_age".to_owned(),
            labels: Default::default(),
            required: false,
            name: MeasurementName::new("oldest_due_age").expect("valid fixture"),
            kind: MeasurementKind::Gauge,
            unit: Unit::Seconds,
        };

        // Act
        let measurement = map_series("queue 0\n", &mapping).expect("optional mapping");

        // Assert
        assert!(measurement.is_none());
    }
}
