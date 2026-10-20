use std::{collections::BTreeMap, fmt, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 128;
const MAX_LABELS: usize = 12;
const MAX_LABEL_VALUE_LEN: usize = 128;
const MAX_FACT_TEXT_LEN: usize = 512;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    #[error("{kind} must contain 1 to {max} ASCII letters, digits, '_' or '-'")]
    InvalidIdentifier { kind: &'static str, max: usize },
    #[error("measurement value must be finite")]
    NonFiniteValue,
    #[error("at most {MAX_LABELS} labels are allowed")]
    TooManyLabels,
    #[error("label value exceeds {MAX_LABEL_VALUE_LEN} bytes")]
    LabelValueTooLong,
    #[error("fact text exceeds {MAX_FACT_TEXT_LEN} bytes")]
    FactTextTooLong,
    #[error("collection duration is too large")]
    DurationTooLarge,
}

macro_rules! validated_name {
    ($name:ident, $kind:literal, $max:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= $max
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
                if valid {
                    Ok(Self(value))
                } else {
                    Err(ModelError::InvalidIdentifier {
                        kind: $kind,
                        max: $max,
                    })
                }
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ModelError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

validated_name!(CollectorId, "collector id", MAX_ID_LEN);
validated_name!(MeasurementName, "measurement name", MAX_NAME_LEN);
validated_name!(LabelName, "label name", MAX_ID_LEN);
validated_name!(FactName, "fact name", MAX_NAME_LEN);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    Gauge,
    Counter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    None,
    Bytes,
    Seconds,
    Ratio,
    Count,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FiniteValue(f64);

impl FiniteValue {
    pub fn new(value: f64) -> Result<Self, ModelError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(ModelError::NonFiniteValue)
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for FiniteValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub name: MeasurementName,
    pub kind: MeasurementKind,
    pub unit: Unit,
    pub value: FiniteValue,
    pub labels: BTreeMap<LabelName, String>,
}

impl Measurement {
    pub fn new(
        name: MeasurementName,
        kind: MeasurementKind,
        unit: Unit,
        value: FiniteValue,
        labels: BTreeMap<LabelName, String>,
    ) -> Result<Self, ModelError> {
        if labels.len() > MAX_LABELS {
            return Err(ModelError::TooManyLabels);
        }
        if labels
            .values()
            .any(|value| value.len() > MAX_LABEL_VALUE_LEN)
        {
            return Err(ModelError::LabelValueTooLong);
        }
        Ok(Self {
            name,
            kind,
            unit,
            value,
            labels,
        })
    }

    pub fn scalar(
        name: &str,
        kind: MeasurementKind,
        unit: Unit,
        value: f64,
    ) -> Result<Self, ModelError> {
        Self::new(
            MeasurementName::new(name)?,
            kind,
            unit,
            FiniteValue::new(value)?,
            BTreeMap::new(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum FactValue {
    Boolean(bool),
    Text(String),
    Timestamp(DateTime<Utc>),
}

impl FactValue {
    pub fn text(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.len() > MAX_FACT_TEXT_LEN {
            return Err(ModelError::FactTextTooLong);
        }
        Ok(Self::Text(value))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub name: FactName,
    #[serde(flatten)]
    pub value: FactValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ObservationResult {
    Collected {
        measurements: Vec<Measurement>,
        facts: Vec<Fact>,
    },
    Failed {
        code: String,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub collector_id: CollectorId,
    pub collected_at: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(flatten)]
    pub result: ObservationResult,
}

impl Observation {
    pub fn duration_millis(duration: Duration) -> Result<u64, ModelError> {
        u64::try_from(duration.as_millis()).map_err(|_| ModelError::DurationTooLarge)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u16,
    pub sequence: u64,
    pub generated_at: DateTime<Utc>,
    pub observations: Vec<Observation>,
}

impl Snapshot {
    pub fn empty(now: DateTime<Utc>) -> Self {
        Self {
            schema_version: 1,
            sequence: 0,
            generated_at: now,
            observations: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rejects_invalid_measurement_name() {
        // Arrange
        let invalid = "disk usage";

        // Act
        let result = MeasurementName::new(invalid);

        // Assert
        assert!(matches!(result, Err(ModelError::InvalidIdentifier { .. })));
    }

    #[test]
    fn test_rejects_non_finite_measurement_value() {
        // Arrange
        let value = f64::NAN;

        // Act
        let result = FiniteValue::new(value);

        // Assert
        assert_eq!(result, Err(ModelError::NonFiniteValue));
    }

    #[test]
    fn test_deserialization_validates_collector_id() {
        // Arrange
        let json = r#""not valid""#;

        // Act
        let result = serde_json::from_str::<CollectorId>(json);

        // Assert
        assert!(result.is_err());
    }
}
