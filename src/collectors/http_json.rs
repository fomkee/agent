use async_trait::async_trait;
use reqwest::Client;
use url::Url;

use crate::{
    collector::{Collection, Collector, CollectorError},
    config::JsonMapping,
    model::{CollectorId, Measurement},
};

use super::http_source::{self, SourceAuth};

#[derive(Debug)]
pub struct HttpJsonCollector {
    id: CollectorId,
    url: Url,
    auth: Option<SourceAuth>,
    mappings: Vec<JsonMapping>,
    client: Client,
}

impl HttpJsonCollector {
    pub fn new(
        id: CollectorId,
        url: Url,
        auth: Option<SourceAuth>,
        mappings: Vec<JsonMapping>,
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
impl Collector for HttpJsonCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let body = http_source::fetch(&self.client, &self.url, self.auth.as_ref()).await?;
        let source: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| CollectorError::new("json_parse", error.to_string()))?;
        let measurements = self
            .mappings
            .iter()
            .map(|mapping| map_value(&source, mapping))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Collection::new(measurements, Vec::new()))
    }
}

fn map_value(
    source: &serde_json::Value,
    mapping: &JsonMapping,
) -> Result<Measurement, CollectorError> {
    let value = source
        .pointer(&mapping.pointer)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| {
            CollectorError::new(
                "json_mapping",
                format!("{} does not select a numeric value", mapping.pointer),
            )
        })?;
    Measurement::new(
        mapping.name.clone(),
        mapping.kind,
        mapping.unit,
        crate::model::FiniteValue::new(value)
            .map_err(|error| CollectorError::new("json_mapping", error.to_string()))?,
        Default::default(),
    )
    .map_err(|error| CollectorError::new("json_mapping", error.to_string()))
}
