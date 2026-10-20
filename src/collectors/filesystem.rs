use std::{ops::Div, path::PathBuf};

use async_trait::async_trait;

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Measurement, MeasurementKind, Unit},
};

#[derive(Debug)]
pub struct FilesystemCollector {
    id: CollectorId,
    path: PathBuf,
}

impl FilesystemCollector {
    pub fn new(id: CollectorId, path: impl Into<PathBuf>) -> Self {
        Self {
            id,
            path: path.into(),
        }
    }
}

#[async_trait]
impl Collector for FilesystemCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let stats = rustix::fs::statvfs(&self.path)
            .map_err(|error| CollectorError::new("statvfs", error.to_string()))?;
        let total = stats
            .f_blocks
            .checked_mul(stats.f_frsize)
            .ok_or_else(|| CollectorError::new("overflow", "filesystem size is too large"))?;
        let available = stats.f_bavail.checked_mul(stats.f_frsize).ok_or_else(|| {
            CollectorError::new("overflow", "filesystem availability is too large")
        })?;
        let used = total.saturating_sub(available);
        let used_ratio = ratio(used, total);
        let inodes_used = stats.f_files.saturating_sub(stats.f_favail);
        let inodes_used_ratio = ratio(inodes_used, stats.f_files);
        Ok(Collection::new(
            vec![
                scalar("total", Unit::Bytes, total as f64)?,
                scalar("available", Unit::Bytes, available as f64)?,
                scalar("used", Unit::Bytes, used as f64)?,
                scalar("used_ratio", Unit::Ratio, used_ratio)?,
                scalar("inodes_total", Unit::Count, stats.f_files as f64)?,
                scalar("inodes_available", Unit::Count, stats.f_favail as f64)?,
                scalar("inodes_used_ratio", Unit::Ratio, inodes_used_ratio)?,
            ],
            Vec::new(),
        ))
    }
}

fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64).div(total as f64)
    }
}

fn scalar(name: &str, unit: Unit, value: f64) -> Result<Measurement, CollectorError> {
    Measurement::scalar(name, MeasurementKind::Gauge, unit, value)
        .map_err(|error| CollectorError::new("invalid_measurement", error.to_string()))
}
