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
        let measurements = self
            .mappings
            .iter()
            .map(|mapping| map_series(source, mapping))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Collection::new(measurements, Vec::new()))
    }
}

fn map_series(source: &str, mapping: &OpenMetricsMapping) -> Result<Measurement, CollectorError> {
    let mut total: Option<f64> = None;
    for line in source.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let Some(series) = fields.next() else {
            continue;
        };
        let name = series.split_once('{').map_or(series, |(name, _)| name);
        if name != mapping.source {
            continue;
        }
        let value = fields
            .next()
            .ok_or_else(|| CollectorError::new("openmetrics_parse", "sample has no value"))?
            .parse::<f64>()
            .map_err(|error| CollectorError::new("openmetrics_parse", error.to_string()))?;
        total = Some(total.map_or(value, |current| current.add(value)));
    }
    let value = total.ok_or_else(|| {
        CollectorError::new(
            "openmetrics_mapping",
            format!("source metric {} was not found", mapping.source),
        )
    })?;
    Measurement::new(
        mapping.name.clone(),
        mapping.kind,
        mapping.unit,
        FiniteValue::new(value)
            .map_err(|error| CollectorError::new("openmetrics_mapping", error.to_string()))?,
        Default::default(),
    )
    .map_err(|error| CollectorError::new("openmetrics_mapping", error.to_string()))
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
            name: MeasurementName::new("pending").expect("valid fixture"),
            kind: MeasurementKind::Gauge,
            unit: Unit::Count,
        };

        // Act
        let measurement = map_series(source, &mapping).expect("valid fixture");

        // Assert
        assert_eq!(measurement.value.get(), 5.0);
    }

    #[test]
    fn test_mapping_rejects_malformed_sample() {
        // Arrange
        let mapping = OpenMetricsMapping {
            source: "queue".to_owned(),
            name: MeasurementName::new("pending").expect("valid fixture"),
            kind: MeasurementKind::Gauge,
            unit: Unit::Count,
        };

        // Act
        let result = map_series("queue not-a-number\n", &mapping);

        // Assert
        assert!(result.is_err());
    }
}
