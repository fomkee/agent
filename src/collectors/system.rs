use async_trait::async_trait;

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Measurement, MeasurementKind, Unit},
};

#[derive(Debug)]
pub struct SystemCollector {
    id: CollectorId,
}

impl SystemCollector {
    pub fn new(id: CollectorId) -> Self {
        Self { id }
    }
}

#[async_trait]
impl Collector for SystemCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let (uptime, load, memory) = tokio::try_join!(
            tokio::fs::read_to_string("/proc/uptime"),
            tokio::fs::read_to_string("/proc/loadavg"),
            tokio::fs::read_to_string("/proc/meminfo")
        )
        .map_err(|error| CollectorError::new("procfs_read", error.to_string()))?;

        let uptime_seconds = first_number(&uptime, "uptime")?;
        let load_one = first_number(&load, "load average")?;
        let (memory_total, memory_available) = parse_memory(&memory)?;
        let memory_used = memory_total.saturating_sub(memory_available);
        Ok(Collection::new(
            vec![
                scalar("uptime", Unit::Seconds, uptime_seconds)?,
                scalar("load_1m", Unit::None, load_one)?,
                scalar("memory_total", Unit::Bytes, memory_total as f64)?,
                scalar("memory_available", Unit::Bytes, memory_available as f64)?,
                scalar("memory_used", Unit::Bytes, memory_used as f64)?,
            ],
            Vec::new(),
        ))
    }
}

fn first_number(contents: &str, field: &str) -> Result<f64, CollectorError> {
    contents
        .split_whitespace()
        .next()
        .ok_or_else(|| CollectorError::new("procfs_parse", format!("missing {field}")))?
        .parse::<f64>()
        .map_err(|error| CollectorError::new("procfs_parse", error.to_string()))
}

fn parse_memory(contents: &str) -> Result<(u64, u64), CollectorError> {
    let total = memory_kib(contents, "MemTotal:")?;
    let available = memory_kib(contents, "MemAvailable:")?;
    let multiplier = 1_024_u64;
    let total_bytes = total
        .checked_mul(multiplier)
        .ok_or_else(|| CollectorError::new("overflow", "memory total is too large"))?;
    let available_bytes = available
        .checked_mul(multiplier)
        .ok_or_else(|| CollectorError::new("overflow", "memory available is too large"))?;
    Ok((total_bytes, available_bytes))
}

fn memory_kib(contents: &str, key: &str) -> Result<u64, CollectorError> {
    let line = contents
        .lines()
        .find(|line| line.starts_with(key))
        .ok_or_else(|| CollectorError::new("procfs_parse", format!("missing {key}")))?;
    line.split_whitespace()
        .nth(1)
        .ok_or_else(|| CollectorError::new("procfs_parse", format!("missing value for {key}")))?
        .parse::<u64>()
        .map_err(|error| CollectorError::new("procfs_parse", error.to_string()))
}

fn scalar(name: &str, unit: Unit, value: f64) -> Result<Measurement, CollectorError> {
    Measurement::scalar(name, MeasurementKind::Gauge, unit, value)
        .map_err(|error| CollectorError::new("invalid_measurement", error.to_string()))
}
