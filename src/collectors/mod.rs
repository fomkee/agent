pub mod backup;
pub mod container;
pub mod filesystem;
pub mod http_json;
mod http_source;
pub mod openmetrics;
pub mod postgres;
pub mod system;
pub mod systemd;

use std::str::FromStr;
use std::sync::Arc;

use reqwest::{
    Client,
    header::{HeaderName, HeaderValue},
};

use crate::{
    collector::{Collector, CollectorError},
    config::{BearerToken, CollectorConfig, ConfigError, HttpAuthConfig, MAX_SECRET_BYTES},
};

use self::http_source::SourceAuth;

pub fn build_collectors(
    configs: &[CollectorConfig],
) -> Result<Vec<Arc<dyn Collector>>, CollectorError> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|error| CollectorError::new("http_client", error.to_string()))?;
    configs
        .iter()
        .map(|config| build_collector(config, &client))
        .collect()
}

fn build_collector(
    config: &CollectorConfig,
    client: &Client,
) -> Result<Arc<dyn Collector>, CollectorError> {
    let collector: Arc<dyn Collector> = match config {
        CollectorConfig::System { id } => Arc::new(system::SystemCollector::new(id.clone())),
        CollectorConfig::Filesystem { id, path } => {
            Arc::new(filesystem::FilesystemCollector::new(id.clone(), path))
        }
        CollectorConfig::Systemd { id, unit } => {
            Arc::new(systemd::SystemdCollector::new(id.clone(), unit.clone()))
        }
        CollectorConfig::Container { id, name } => {
            Arc::new(container::ContainerCollector::new(id.clone(), name.clone()))
        }
        CollectorConfig::Postgres {
            id,
            database_url_env,
        } => {
            let database_url = environment(database_url_env)?;
            Arc::new(postgres::PostgresCollector::new(id.clone(), &database_url)?)
        }
        CollectorConfig::Backup {
            id,
            directory,
            prefix,
        } => Arc::new(backup::BackupCollector::new(
            id.clone(),
            directory.clone(),
            prefix.clone(),
        )),
        CollectorConfig::HttpJson {
            id,
            url,
            auth,
            mappings,
        } => Arc::new(http_json::HttpJsonCollector::new(
            id.clone(),
            url.clone(),
            source_auth(auth.as_ref())?,
            mappings.clone(),
            client.clone(),
        )),
        CollectorConfig::Openmetrics {
            id,
            url,
            auth,
            mappings,
        } => Arc::new(openmetrics::OpenMetricsCollector::new(
            id.clone(),
            url.clone(),
            source_auth(auth.as_ref())?,
            mappings.clone(),
            client.clone(),
        )),
    };
    Ok(collector)
}

fn source_auth(config: Option<&HttpAuthConfig>) -> Result<Option<SourceAuth>, CollectorError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let auth = match config {
        HttpAuthConfig::Bearer { token_env } => {
            let token = BearerToken::new(environment(token_env)?).ok_or_else(|| {
                CollectorError::new("invalid_secret", "configured bearer token is empty")
            })?;
            SourceAuth::Bearer(token)
        }
        HttpAuthConfig::Header { name, value_env } => {
            let name = HeaderName::from_str(name)
                .map_err(|error| CollectorError::new("invalid_header", error.to_string()))?;
            let secret = environment(value_env)?;
            if secret.is_empty() || secret.len() > MAX_SECRET_BYTES {
                return Err(CollectorError::new(
                    "invalid_secret",
                    "configured header secret has an invalid length",
                ));
            }
            let value = HeaderValue::from_str(&secret)
                .map_err(|error| CollectorError::new("invalid_secret", error.to_string()))?;
            SourceAuth::Header { name, value }
        }
    };
    Ok(Some(auth))
}

fn environment(name: &str) -> Result<String, CollectorError> {
    std::env::var(name).map_err(|_| {
        let error = ConfigError::MissingEnvironment(name.to_owned());
        CollectorError::new("missing_environment", error.to_string())
    })
}
