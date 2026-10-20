use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    collector::{Collection, Collector, CollectorError},
    model::{CollectorId, Fact, FactName, FactValue, Measurement, MeasurementKind, Unit},
};

#[derive(Debug)]
pub struct SystemdCollector {
    id: CollectorId,
    unit: String,
}

impl SystemdCollector {
    pub fn new(id: CollectorId, unit: String) -> Self {
        Self { id, unit }
    }
}

#[async_trait]
impl Collector for SystemdCollector {
    fn id(&self) -> &CollectorId {
        &self.id
    }

    async fn collect(&self) -> Result<Collection, CollectorError> {
        let mut command = Command::new("systemctl");
        command
            .kill_on_drop(true)
            .arg("show")
            .arg("--no-pager")
            .arg("--property=ActiveState,SubState,NRestarts")
            .arg(&self.unit);
        let output = command
            .output()
            .await
            .map_err(|error| CollectorError::new("systemctl_exec", error.to_string()))?;
        if !output.status.success() {
            return Err(CollectorError::new(
                "systemctl_status",
                "systemctl could not inspect the configured unit",
            ));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|error| CollectorError::new("systemctl_output", error.to_string()))?;
        let active = property(&text, "ActiveState")?;
        let sub = property(&text, "SubState")?;
        let restarts = property(&text, "NRestarts")?
            .parse::<f64>()
            .map_err(|error| CollectorError::new("systemctl_output", error.to_string()))?;
        Ok(Collection::new(
            vec![
                Measurement::scalar(
                    "restarts_total",
                    MeasurementKind::Counter,
                    Unit::Count,
                    restarts,
                )
                .map_err(|error| CollectorError::new("invalid_measurement", error.to_string()))?,
            ],
            vec![
                Fact {
                    name: FactName::new("active").map_err(model_error)?,
                    value: FactValue::Boolean(active == "active"),
                },
                Fact {
                    name: FactName::new("active_state").map_err(model_error)?,
                    value: FactValue::text(active).map_err(model_error)?,
                },
                Fact {
                    name: FactName::new("sub_state").map_err(model_error)?,
                    value: FactValue::text(sub).map_err(model_error)?,
                },
            ],
        ))
    }
}

fn property<'a>(contents: &'a str, name: &str) -> Result<&'a str, CollectorError> {
    contents
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|value| value.strip_prefix('='))
        })
        .ok_or_else(|| CollectorError::new("systemctl_output", format!("missing {name}")))
}

fn model_error(error: impl std::fmt::Display) -> CollectorError {
    CollectorError::new("invalid_value", error.to_string())
}
