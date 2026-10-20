use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Fact, FactName, FactValue, Measurement, MeasurementKind, Unit},
};

#[derive(Debug)]
pub struct ContainerCollector {
    id: CollectorId,
    name: String,
}

impl ContainerCollector {
    pub fn new(id: CollectorId, name: String) -> Self {
        Self { id, name }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerState {
    running: bool,
    status: String,
    #[serde(default)]
    exit_code: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerInspection {
    state: ContainerState,
    #[serde(default)]
    restart_count: u64,
}

#[async_trait]
impl Collector for ContainerCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let mut command = Command::new("docker");
        command.kill_on_drop(true).arg("inspect").arg(&self.name);
        let output = command
            .output()
            .await
            .map_err(|error| CollectorError::new("docker_exec", error.to_string()))?;
        if !output.status.success() {
            return Err(CollectorError::new(
                "docker_status",
                "docker could not inspect the configured container",
            ));
        }
        let inspections: Vec<ContainerInspection> = serde_json::from_slice(&output.stdout)
            .map_err(|error| CollectorError::new("docker_output", error.to_string()))?;
        let inspection = inspections
            .first()
            .ok_or_else(|| CollectorError::new("docker_output", "empty inspection response"))?;
        Ok(Collection::new(
            vec![
                Measurement::scalar(
                    "restarts_total",
                    MeasurementKind::Counter,
                    Unit::Count,
                    inspection.restart_count as f64,
                )
                .map_err(model_error)?,
                Measurement::scalar(
                    "exit_code",
                    MeasurementKind::Gauge,
                    Unit::None,
                    inspection.state.exit_code as f64,
                )
                .map_err(model_error)?,
            ],
            vec![
                Fact {
                    name: FactName::new("running").map_err(model_error)?,
                    value: FactValue::Boolean(inspection.state.running),
                },
                Fact {
                    name: FactName::new("status").map_err(model_error)?,
                    value: FactValue::text(&inspection.state.status).map_err(model_error)?,
                },
            ],
        ))
    }
}

fn model_error(error: impl std::fmt::Display) -> CollectorError {
    CollectorError::new("invalid_value", error.to_string())
}
