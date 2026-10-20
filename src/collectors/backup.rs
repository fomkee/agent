use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Fact, FactName, FactValue, Measurement, MeasurementKind, Unit},
};

#[derive(Debug)]
pub struct BackupCollector {
    id: CollectorId,
    directory: PathBuf,
    prefix: Option<String>,
}

impl BackupCollector {
    pub fn new(id: CollectorId, directory: String, prefix: Option<String>) -> Self {
        Self {
            id,
            directory: directory.into(),
            prefix,
        }
    }
}

#[async_trait]
impl Collector for BackupCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let mut entries = tokio::fs::read_dir(&self.directory)
            .await
            .map_err(|error| CollectorError::new("backup_directory", error.to_string()))?;
        let mut latest: Option<(DateTime<Utc>, u64)> = None;
        let mut count = 0_u64;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| CollectorError::new("backup_directory", error.to_string()))?
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if self
                .prefix
                .as_ref()
                .is_some_and(|prefix| !name.starts_with(prefix))
            {
                continue;
            }
            let metadata = entry
                .metadata()
                .await
                .map_err(|error| CollectorError::new("backup_metadata", error.to_string()))?;
            if !metadata.is_file() {
                continue;
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| CollectorError::new("overflow", "too many backup files"))?;
            let modified = metadata
                .modified()
                .map_err(|error| CollectorError::new("backup_metadata", error.to_string()))?;
            let candidate = (DateTime::<Utc>::from(modified), metadata.len());
            if latest
                .as_ref()
                .is_none_or(|current| candidate.0 > current.0)
            {
                latest = Some(candidate);
            }
        }
        let (modified_at, size) = latest.ok_or_else(|| {
            CollectorError::new("backup_missing", "no matching backup file was found")
        })?;
        let age = Utc::now()
            .signed_duration_since(modified_at)
            .to_std()
            .unwrap_or_default()
            .as_secs_f64();
        Ok(Collection::new(
            vec![
                scalar("files", Unit::Count, count as f64)?,
                scalar("latest_size", Unit::Bytes, size as f64)?,
                scalar("latest_age", Unit::Seconds, age)?,
            ],
            vec![Fact {
                name: FactName::new("latest_modified_at").map_err(model_error)?,
                value: FactValue::Timestamp(modified_at),
            }],
        ))
    }
}

fn scalar(name: &str, unit: Unit, value: f64) -> Result<Measurement, CollectorError> {
    Measurement::scalar(name, MeasurementKind::Gauge, unit, value).map_err(model_error)
}

fn model_error(error: impl std::fmt::Display) -> CollectorError {
    CollectorError::new("invalid_value", error.to_string())
}
