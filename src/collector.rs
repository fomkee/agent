use std::fmt;

use async_trait::async_trait;
use thiserror::Error;

use crate::model::{CollectorId, Fact, Measurement};

const MAX_ERROR_SUMMARY_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub measurements: Vec<Measurement>,
    pub facts: Vec<Fact>,
}

impl Collection {
    pub fn new(measurements: Vec<Measurement>, facts: Vec<Fact>) -> Self {
        Self {
            measurements,
            facts,
        }
    }
}

#[derive(Debug, Error)]
#[error("collector failed ({code}): {summary}")]
pub struct CollectorError {
    code: &'static str,
    summary: String,
}

impl CollectorError {
    pub fn new(code: &'static str, summary: impl Into<String>) -> Self {
        let summary = summary
            .into()
            .chars()
            .take(MAX_ERROR_SUMMARY_CHARS)
            .collect();
        Self { code, summary }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn summary(&self) -> &str {
        self.summary.as_str()
    }
}

#[async_trait]
pub trait Collector: Send + Sync + fmt::Debug {
    fn id(&self) -> &CollectorId;

    async fn collect(&self) -> Result<Collection, CollectorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_summary_is_bounded() {
        // Arrange
        let oversized = "x".repeat(MAX_ERROR_SUMMARY_CHARS.saturating_add(50));

        // Act
        let error = CollectorError::new("fixture", oversized);

        // Assert
        assert_eq!(error.summary().chars().count(), MAX_ERROR_SUMMARY_CHARS);
    }
}
