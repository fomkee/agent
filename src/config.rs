use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::model::{
    CollectionInterval, CollectorId, MeasurementKind, MeasurementName, ModelError, Unit,
};

const MAX_CONFIG_BYTES: usize = 262_144;
const MAX_COLLECTORS: usize = 64;
const MAX_MAPPINGS_PER_COLLECTOR: usize = 128;
pub(crate) const MAX_TUI_HISTORY_SNAPSHOTS: usize = 10_000;
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
    #[error("collector {0} interval must be greater than zero")]
    InvalidCollectorInterval(CollectorId),
    #[error("TUI history snapshots must be between 1 and {MAX_TUI_HISTORY_SNAPSHOTS}")]
    InvalidTuiHistory,
    #[error("TUI capacity thresholds must satisfy 0 < warning < critical <= 100")]
    InvalidTuiCapacityThresholds,
    #[error("environment variable {0} is not set")]
    MissingEnvironment(String),
    #[error("heartbeat URL from the configured environment variable is invalid")]
    InvalidHeartbeatUrl,
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
    #[serde(default)]
    pub tui: TuiConfig,
    pub heartbeat: Option<HeartbeatConfig>,
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
        if !(1..=MAX_TUI_HISTORY_SNAPSHOTS).contains(&self.tui.history_snapshots) {
            return Err(ConfigError::InvalidTuiHistory);
        }
        if !self.tui.capacity.is_valid() {
            return Err(ConfigError::InvalidTuiCapacityThresholds);
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
            if collector.interval_secs() == Some(0) {
                return Err(ConfigError::InvalidCollectorInterval(
                    collector.id().clone(),
                ));
            }
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

    pub fn heartbeat_endpoint(&self) -> Result<Option<HeartbeatEndpoint>, ConfigError> {
        let Some(config) = &self.heartbeat else {
            return Ok(None);
        };
        let value = match std::env::var(&config.url_env) {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidHeartbeatUrl);
            }
        };
        let url = Url::parse(&value).map_err(|_| ConfigError::InvalidHeartbeatUrl)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ConfigError::InvalidHeartbeatUrl);
        }
        Ok(Some(HeartbeatEndpoint(url)))
    }

    /// Returns the shortest effective cadence across configured collectors.
    pub fn shortest_collector_interval(&self) -> Result<Duration, ModelError> {
        self.collectors
            .iter()
            .map(|collector| collector.interval(self.schedule.interval()))
            .try_fold(self.schedule.interval(), |shortest, interval| {
                interval.map(|interval| shortest.min(interval.duration()))
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatConfig {
    pub url_env: String,
}

#[derive(Clone)]
pub struct HeartbeatEndpoint(Url);

impl HeartbeatEndpoint {
    pub fn expose(&self) -> &Url {
        &self.0
    }
}

impl std::fmt::Debug for HeartbeatEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HeartbeatEndpoint([REDACTED])")
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
    #[serde(default = "default_unix_socket")]
    pub unix_socket: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            bearer_token_env: default_token_env(),
            metrics_enabled: true,
            unix_socket: default_unix_socket(),
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

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TuiDefaultView {
    #[default]
    Dashboard,
    Collectors,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    #[serde(default = "default_tui_history_snapshots")]
    pub history_snapshots: usize,
    #[serde(default)]
    pub default_view: TuiDefaultView,
    #[serde(default)]
    pub capacity: CapacityThresholds,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            history_snapshots: default_tui_history_snapshots(),
            default_view: TuiDefaultView::default(),
            capacity: CapacityThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapacityThresholds {
    #[serde(default = "default_capacity_warning_percent")]
    pub warning_percent: u8,
    #[serde(default = "default_capacity_critical_percent")]
    pub critical_percent: u8,
}

impl CapacityThresholds {
    fn is_valid(self) -> bool {
        self.warning_percent > 0
            && self.warning_percent < self.critical_percent
            && self.critical_percent <= 100
    }
}

impl Default for CapacityThresholds {
    fn default() -> Self {
        Self {
            warning_percent: default_capacity_warning_percent(),
            critical_percent: default_capacity_critical_percent(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MountRequirement {
    #[default]
    Path,
    Mountpoint,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollectorConfig {
    System {
        id: CollectorId,
        interval_secs: Option<u64>,
    },
    Filesystem {
        id: CollectorId,
        path: String,
        #[serde(default)]
        mount_requirement: MountRequirement,
        interval_secs: Option<u64>,
    },
    Systemd {
        id: CollectorId,
        unit: String,
        interval_secs: Option<u64>,
    },
    Container {
        id: CollectorId,
        name: String,
        interval_secs: Option<u64>,
    },
    Postgres {
        id: CollectorId,
        database_url_env: String,
        interval_secs: Option<u64>,
    },
    Backup {
        id: CollectorId,
        directory: String,
        prefix: Option<String>,
        interval_secs: Option<u64>,
    },
    HttpJson {
        id: CollectorId,
        url: Url,
        auth: Option<HttpAuthConfig>,
        mappings: Vec<JsonMapping>,
        interval_secs: Option<u64>,
    },
    Openmetrics {
        id: CollectorId,
        url: Url,
        auth: Option<HttpAuthConfig>,
        mappings: Vec<OpenMetricsMapping>,
        interval_secs: Option<u64>,
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
            Self::System { id, .. }
            | Self::Filesystem { id, .. }
            | Self::Systemd { id, .. }
            | Self::Container { id, .. }
            | Self::Postgres { id, .. }
            | Self::Backup { id, .. }
            | Self::HttpJson { id, .. }
            | Self::Openmetrics { id, .. } => id,
        }
    }

    /// Resolves this collector's explicit cadence or the global default.
    pub fn interval(&self, default: Duration) -> Result<CollectionInterval, ModelError> {
        let seconds = self.interval_secs().unwrap_or(default.as_secs());
        CollectionInterval::from_secs(seconds)
    }

    fn interval_secs(&self) -> Option<u64> {
        match self {
            Self::System { interval_secs, .. }
            | Self::Filesystem { interval_secs, .. }
            | Self::Systemd { interval_secs, .. }
            | Self::Container { interval_secs, .. }
            | Self::Postgres { interval_secs, .. }
            | Self::Backup { interval_secs, .. }
            | Self::HttpJson { interval_secs, .. }
            | Self::Openmetrics { interval_secs, .. } => *interval_secs,
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
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub required: bool,
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

fn default_unix_socket() -> PathBuf {
    PathBuf::from("/run/fomkee-agent/agent.sock")
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

fn default_tui_history_snapshots() -> usize {
    120
}

fn default_capacity_warning_percent() -> u8 {
    80
}

fn default_capacity_critical_percent() -> u8 {
    90
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_bundled_example_validates_without_credentials() {
        // Arrange
        let input = include_str!("../examples/config.toml");
        let config = toml::from_str::<Config>(input).expect("example has valid TOML shape");

        // Act
        let result = config.validate();

        // Assert
        assert!(result.is_ok());
        assert_eq!(config.collectors.len(), 8);
    }

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
            interval_secs: None,
        };
        let config = Config {
            server: ServerConfig::default(),
            schedule: ScheduleConfig::default(),
            tui: TuiConfig::default(),
            heartbeat: None,
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

    #[test]
    fn test_rejects_unbounded_tui_history() {
        // Arrange
        let input = r#"
            [tui]
            history_snapshots = 0

            [[collectors]]
            type = "system"
            id = "host"
        "#;
        let config = toml::from_str::<Config>(input).expect("valid TOML shape");

        // Act
        let result = config.validate();

        // Assert
        assert!(matches!(result, Err(ConfigError::InvalidTuiHistory)));
    }

    #[test]
    fn test_collector_interval_defaults_to_global_schedule() {
        // Arrange
        let input = r#"
            [schedule]
            interval_secs = 45

            [[collectors]]
            type = "system"
            id = "host"
        "#;
        let config = toml::from_str::<Config>(input).expect("valid TOML shape");
        let collector = config.collectors.first().expect("collector fixture");

        // Act
        let interval = collector
            .interval(config.schedule.interval())
            .expect("valid interval");

        // Assert
        assert_eq!(interval.as_secs(), 45);
    }

    #[test]
    fn test_rejects_zero_collector_interval() {
        // Arrange
        let input = r#"
            [[collectors]]
            type = "filesystem"
            id = "disk"
            path = "/"
            interval_secs = 0
        "#;
        let config = toml::from_str::<Config>(input).expect("valid TOML shape");

        // Act
        let result = config.validate();

        // Assert
        assert!(matches!(
            result,
            Err(ConfigError::InvalidCollectorInterval(id)) if id.as_str() == "disk"
        ));
    }

    #[test]
    fn test_filesystem_mount_requirement_defaults_to_path() {
        // Arrange
        let input = r#"
            [[collectors]]
            type = "filesystem"
            id = "disk"
            path = "/srv/data"
        "#;

        // Act
        let config = toml::from_str::<Config>(input).expect("valid fixture");

        // Assert
        assert!(matches!(
            config.collectors.first(),
            Some(CollectorConfig::Filesystem {
                mount_requirement: MountRequirement::Path,
                ..
            })
        ));
    }

    #[test]
    fn test_parses_exact_filesystem_mount_requirement() {
        // Arrange
        let input = r#"
            [[collectors]]
            type = "filesystem"
            id = "disk"
            path = "/srv/data"
            mount_requirement = "mountpoint"
        "#;

        // Act
        let config = toml::from_str::<Config>(input).expect("valid fixture");

        // Assert
        assert!(matches!(
            config.collectors.first(),
            Some(CollectorConfig::Filesystem {
                mount_requirement: MountRequirement::Mountpoint,
                ..
            })
        ));
    }

    #[test]
    fn test_rejects_inverted_tui_capacity_thresholds() {
        // Arrange
        let input = r#"
            [tui.capacity]
            warning_percent = 95
            critical_percent = 90

            [[collectors]]
            type = "system"
            id = "host"
        "#;
        let config = toml::from_str::<Config>(input).expect("valid TOML shape");

        // Act
        let result = config.validate();

        // Assert
        assert!(matches!(
            result,
            Err(ConfigError::InvalidTuiCapacityThresholds)
        ));
    }
}
