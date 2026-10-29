use std::{ops::Div, path::PathBuf};

use async_trait::async_trait;

use crate::{
    collector::{Collection, Collector, CollectorError},
    config::MountRequirement,
    model::{CollectorId, Fact, FactName, FactValue, Measurement, MeasurementKind, Unit},
};

use super::filesystem_mounts::{self, MountMetadata};

#[derive(Debug)]
pub struct FilesystemCollector {
    id: CollectorId,
    path: PathBuf,
    mount_requirement: MountRequirement,
}

impl FilesystemCollector {
    pub fn new(
        id: CollectorId,
        path: impl Into<PathBuf>,
        mount_requirement: MountRequirement,
    ) -> Self {
        Self {
            id,
            path: path.into(),
            mount_requirement,
        }
    }
}

#[async_trait]
impl Collector for FilesystemCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let mount = filesystem_mounts::inspect(&self.path, self.mount_requirement).await?;
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
        let used_ratio = ratio(used, total)?;
        let mut measurements = vec![
            scalar("total", Unit::Bytes, total as f64)?,
            scalar("available", Unit::Bytes, available as f64)?,
            scalar("used", Unit::Bytes, used as f64)?,
            scalar("used_ratio", Unit::Ratio, used_ratio)?,
        ];
        measurements.extend(inode_measurements(stats.f_files, stats.f_favail)?);
        let mut facts = vec![Fact {
            name: FactName::new("inode_metrics_available")
                .map_err(|error| CollectorError::new("invalid_fact", error.to_string()))?,
            value: FactValue::Boolean(stats.f_files > 0),
        }];
        if let Some(mount) = mount {
            facts.extend(mount_facts(mount)?);
        }
        Ok(Collection::new(measurements, facts))
    }
}

fn mount_facts(mount: MountMetadata) -> Result<Vec<Fact>, CollectorError> {
    Ok(vec![
        text_fact("mount_point", mount.mount_point.display().to_string())?,
        text_fact("mount_source", mount.source)?,
        text_fact("filesystem_type", mount.filesystem_type)?,
        Fact {
            name: fact_name("read_only")?,
            value: FactValue::Boolean(mount.read_only),
        },
        Fact {
            name: fact_name("agent_view_read_only")?,
            value: FactValue::Boolean(mount.agent_view_read_only),
        },
    ])
}

fn text_fact(name: &str, value: String) -> Result<Fact, CollectorError> {
    Ok(Fact {
        name: fact_name(name)?,
        value: FactValue::text(value)
            .map_err(|error| CollectorError::new("invalid_fact", error.to_string()))?,
    })
}

fn fact_name(name: &str) -> Result<FactName, CollectorError> {
    FactName::new(name).map_err(|error| CollectorError::new("invalid_fact", error.to_string()))
}

fn ratio(part: u64, total: u64) -> Result<f64, CollectorError> {
    if total == 0 {
        Err(CollectorError::new(
            "filesystem_capacity",
            "filesystem capacity is unavailable",
        ))
    } else {
        Ok((part as f64).div(total as f64))
    }
}

fn inode_measurements(total: u64, available: u64) -> Result<Vec<Measurement>, CollectorError> {
    if total == 0 {
        return Ok(Vec::new());
    }
    Ok(vec![
        scalar("inodes_total", Unit::Count, total as f64)?,
        scalar("inodes_available", Unit::Count, available as f64)?,
        scalar(
            "inodes_used_ratio",
            Unit::Ratio,
            ratio(total.saturating_sub(available), total)?,
        )?,
    ])
}

fn scalar(name: &str, unit: Unit, value: f64) -> Result<Measurement, CollectorError> {
    Measurement::scalar(name, MeasurementKind::Gauge, unit, value)
        .map_err(|error| CollectorError::new("invalid_measurement", error.to_string()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_unavailable_inode_capacity_does_not_produce_healthy_zeroes() {
        // Arrange
        let total = 0;

        // Act
        let measurements = inode_measurements(total, 0).expect("unsupported inode metrics");

        // Assert
        assert!(measurements.is_empty());
    }

    #[test]
    fn test_zero_byte_capacity_is_an_explicit_failure() {
        // Arrange
        let total = 0;

        // Act
        let result = ratio(0, total);

        // Assert
        assert!(result.is_err());
    }
}
