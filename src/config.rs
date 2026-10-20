use std::{collections::BTreeSet, net::SocketAddr, path::Path, time::Duration};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::model::{CollectorId, MeasurementKind, MeasurementName, Unit};

const MAX_CONFIG_BYTES: usize = 262_144;
const MAX_COLLECTORS: usize = 64;
const MAX_MAPPINGS_PER_COLLECTOR: usize = 128;
pub(crate) const MAX_SECRET_BYTES: usize = 8_192;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read agent configuration")]
    Read(#[source] std::io::Error),
    #[error("failed to parse agent configuration")]
    Parse(#[source] toml::de::Error),
    #[error("agent configuration exceeds {MAX_CONFIG_BYTES} bytes")]
    TooLarge,
    #[error("agent configuration is not valid UTF-8")]
    Encoding(#[source] std::str::Utf8Error),
    #[error("configuration requires at least one collector")]
    NoCollectors,
    #[error("collector ids must be unique: {0}")]
    DuplicateCollector(CollectorId),
    #[error("schedule values must be greater than zero")]
    InvalidSchedule,
    #[error("environment variable {0} is not set")]
    MissingEnvironment(String),
    #[error("collector {0} uses a source URL with an unsupported or unsafe shape")]
    InvalidSourceUrl(CollectorId),
    #[error("at most {MAX_COLLECTORS} collectors are allowed")]
    TooManyCollectors,
    #[error("collector {0} exceeds the {MAX_MAPPINGS_PER_COLLECTOR} mapping limit")]
    TooManyMappings(CollectorId),
    #[error("collector {collector_id} maps measurement {measurement} more than once")]
    DuplicateMeasurement {
        collector_id: CollectorId,
        measurement: MeasurementName,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    pub collectors: Vec<CollectorConfig>,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = tokio::fs::read(path).await.map_err(ConfigError::Read)?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge);
        }
        let contents = std::str::from_utf8(&bytes).map_err(ConfigError::Encoding)?;
        let config: Self = toml::from_str(contents).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.collectors.is_empty() {
            return Err(ConfigError::NoCollectors);
        }
        if self.collectors.len() > MAX_COLLECTORS {
            return Err(ConfigError::TooManyCollectors);
        }
        if self.schedule.interval_secs == 0
            || self.schedule.collector_timeout_secs == 0
            || self.schedule.max_concurrency == 0
        {
            return Err(ConfigError::InvalidSchedule);
        }
        let mut ids = self
            .collectors
            .iter()
            .map(CollectorConfig::id)
            .collect::<Vec<_>>();
        ids.sort();
        for pair in ids.windows(2) {
            if let [left, right] = pair
                && left == right
            {
                return Err(ConfigError::DuplicateCollector((*left).clone()));
            }
        }
        for collector in &self.collectors {
            let mapping_count = match collector {
                CollectorConfig::HttpJson { mappings, .. } => mappings.len(),
                CollectorConfig::Openmetrics { mappings, .. } => mappings.len(),
                CollectorConfig::System { .. }
                | CollectorConfig::Filesystem { .. }
                | CollectorConfig::Systemd { .. }
                | CollectorConfig::Container { .. }
                | CollectorConfig::Postgres { .. }
                | CollectorConfig::Backup { .. } => 0,
            };
            if mapping_count > MAX_MAPPINGS_PER_COLLECTOR {
                return Err(ConfigError::TooManyMappings(collector.id().clone()));
            }
            let measurement_names: Vec<&MeasurementName> = match collector {
                CollectorConfig::HttpJson { mappings, .. } => {
                    mappings.iter().map(|mapping| &mapping.name).collect()
                }
                CollectorConfig::Openmetrics { mappings, .. } => {
                    mappings.iter().map(|mapping| &mapping.name).collect()
                }
                CollectorConfig::System { .. }
                | CollectorConfig::Filesystem { .. }
                | CollectorConfig::Systemd { .. }
                | CollectorConfig::Container { .. }
                | CollectorConfig::Postgres { .. }
                | CollectorConfig::Backup { .. } => Vec::new(),
            };
            let mut unique_names = BTreeSet::new();
            for name in measurement_names {
                if !unique_names.insert(name) {
                    return Err(ConfigError::DuplicateMeasurement {
                        collector_id: collector.id().clone(),
                        measurement: name.clone(),
                    });
                }
            }
            let url = match collector {
                CollectorConfig::HttpJson { url, .. }
                | CollectorConfig::Openmetrics { url, .. } => Some(url),
                CollectorConfig::System { .. }
                | CollectorConfig::Filesystem { .. }
                | CollectorConfig::Systemd { .. }
                | CollectorConfig::Container { .. }
                | CollectorConfig::Postgres { .. }
                | CollectorConfig::Backup { .. } => None,
            };
            if let Some(url) = url
                && (!matches!(url.scheme(), "http" | "https")
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some())
            {
                return Err(ConfigError::InvalidSourceUrl(collector.id().clone()));
            }
        }
        Ok(())
    }

    pub fn bearer_token(&self) -> Result<BearerToken, ConfigError> {
        let value = std::env::var(&self.server.bearer_token_env)
            .map_err(|_| ConfigError::MissingEnvironment(self.server.bearer_token_env.clone()))?;
        BearerToken::new(value)
            .ok_or_else(|| ConfigError::MissingEnvironment(self.server.bearer_token_env.clone()))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default = "default_token_env")]
    pub bearer_token_env: String,
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            bearer_token_env: default_token_env(),
            metrics_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_timeout")]
    pub collector_timeout_secs: u64,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            collector_timeout_secs: default_timeout(),
            max_concurrency: default_concurrency(),
        }
    }
}

impl ScheduleConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }

    pub fn collector_timeout(&self) -> Duration {
        Duration::from_secs(self.collector_timeout_secs)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollectorConfig {
    System {
        id: CollectorId,
    },
    Filesystem {
        id: CollectorId,
        path: String,
    },
    Systemd {
        id: CollectorId,
        unit: String,
    },
    Container {
        id: CollectorId,
        name: String,
    },
    Postgres {
        id: CollectorId,
        database_url_env: String,
    },
    Backup {
        id: CollectorId,
        directory: String,
        prefix: Option<String>,
    },
    HttpJson {
        id: CollectorId,
        url: Url,
        auth: Option<HttpAuthConfig>,
        mappings: Vec<JsonMapping>,
    },
    Openmetrics {
        id: CollectorId,
        url: Url,
        auth: Option<HttpAuthConfig>,
        mappings: Vec<OpenMetricsMapping>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpAuthConfig {
    Bearer { token_env: String },
    Header { name: String, value_env: String },
}

impl CollectorConfig {
    pub fn id(&self) -> &CollectorId {
        match self {
            Self::System { id }
            | Self::Filesystem { id, .. }
            | Self::Systemd { id, .. }
            | Self::Container { id, .. }
            | Self::Postgres { id, .. }
            | Self::Backup { id, .. }
            | Self::HttpJson { id, .. }
            | Self::Openmetrics { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonMapping {
    pub pointer: String,
    pub name: MeasurementName,
    pub kind: MeasurementKind,
    pub unit: Unit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenMetricsMapping {
    pub source: String,
    pub name: MeasurementName,
    pub kind: MeasurementKind,
    pub unit: Unit,
}

#[derive(Clone)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(value: String) -> Option<Self> {
        if value.is_empty() || value.len() > MAX_SECRET_BYTES {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 9469))
}

fn default_token_env() -> String {
    "FOMKEE_AGENT_TOKEN".to_owned()
}

fn default_true() -> bool {
    true
}

fn default_interval() -> u64 {
    30
}

fn default_timeout() -> u64 {
    10
}

fn default_concurrency() -> usize {
    4
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_rejects_unknown_configuration_field() {
        // Arrange
        let input = r#"
            unknown = true
            [[collectors]]
            type = "system"
            id = "host"
        "#;

        // Act
        let result = toml::from_str::<Config>(input);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_rejects_source_url_with_embedded_credentials() {
        // Arrange
        let input = r#"
            [[collectors]]
            type = "openmetrics"
            id = "source"
            url = "https://secret@example.com/metrics"
            mappings = []
        "#;
        let config = toml::from_str::<Config>(input).expect("valid TOML shape");

        // Act
        let result = config.validate();

        // Assert
        assert!(matches!(result, Err(ConfigError::InvalidSourceUrl(_))));
    }

    #[test]
    fn test_rejects_unbounded_collector_count() {
        // Arrange
        let collector = CollectorConfig::System {
            id: CollectorId::new("host").expect("valid fixture"),
        };
        let config = Config {
            server: ServerConfig::default(),
            schedule: ScheduleConfig::default(),
            collectors: vec![collector; MAX_COLLECTORS.saturating_add(1)],
        };

        // Act
        let result = config.validate();

        // Assert
        assert!(matches!(result, Err(ConfigError::TooManyCollectors)));
    }

    #[test]
    fn test_rejects_unbounded_bearer_token() {
        // Arrange
        let token = "x".repeat(MAX_SECRET_BYTES.saturating_add(1));

        // Act
        let result = BearerToken::new(token);

        // Assert
        assert!(result.is_none());
    }
}
