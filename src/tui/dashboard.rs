use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    config::{CapacityThresholds, CollectorConfig, Config},
    model::{CollectorId, ObservationResult},
};

use super::TuiModel;

mod panels;
mod values;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DashboardDefinition {
    collectors: Vec<DashboardCollector>,
    capacity: CapacityThresholds,
}

impl DashboardDefinition {
    pub(super) fn from_config(config: &Config) -> Self {
        Self {
            collectors: config
                .collectors
                .iter()
                .filter_map(DashboardCollector::from_config)
                .collect(),
            capacity: config.tui.capacity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DashboardCollector {
    System { id: CollectorId },
    Filesystem { id: CollectorId, path: String },
    Systemd { id: CollectorId, unit: String },
    Container { id: CollectorId, name: String },
    Postgres { id: CollectorId },
    Backup { id: CollectorId, directory: String },
}

impl DashboardCollector {
    fn from_config(config: &CollectorConfig) -> Option<Self> {
        match config {
            CollectorConfig::System { id, .. } => Some(Self::System { id: id.clone() }),
            CollectorConfig::Filesystem { id, path, .. } => Some(Self::Filesystem {
                id: id.clone(),
                path: path.clone(),
            }),
            CollectorConfig::Systemd { id, unit, .. } => Some(Self::Systemd {
                id: id.clone(),
                unit: unit.clone(),
            }),
            CollectorConfig::Container { id, name, .. } => Some(Self::Container {
                id: id.clone(),
                name: name.clone(),
            }),
            CollectorConfig::Postgres { id, .. } => Some(Self::Postgres { id: id.clone() }),
            CollectorConfig::Backup { id, directory, .. } => Some(Self::Backup {
                id: id.clone(),
                directory: directory.clone(),
            }),
            CollectorConfig::HttpJson { .. } | CollectorConfig::Openmetrics { .. } => None,
        }
    }
}

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let lines = dashboard_lines(model);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Dashboard ").borders(Borders::ALL))
            .scroll((model.dashboard_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn dashboard_lines(model: &TuiModel) -> Vec<Line<'static>> {
    let mut lines = summary_lines(model);
    if model.dashboard.collectors.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(
            "No built-in collectors are configured. Press 2 for custom collector data.",
        ));
        return lines;
    }
    let mut storage_rendered = false;
    for collector in &model.dashboard.collectors {
        lines.push(Line::from(""));
        if matches!(collector, DashboardCollector::Filesystem { .. }) {
            if !storage_rendered {
                panels::append_filesystems(
                    &mut lines,
                    &model.dashboard.collectors,
                    &model.history,
                    model.dashboard.capacity,
                );
                storage_rendered = true;
            } else {
                lines.pop();
            }
        } else {
            panels::append(
                &mut lines,
                collector,
                &model.history,
                model.dashboard.capacity,
            );
        }
    }
    lines
}

fn summary_lines(model: &TuiModel) -> Vec<Line<'static>> {
    let retained = model.history.len();
    let capacity = model.history.capacity();
    let duration = values::history_duration(&model.history);
    let failures = model.latest_snapshot().map_or(0, |snapshot| {
        snapshot
            .observations
            .iter()
            .filter(|observation| matches!(observation.result, ObservationResult::Failed { .. }))
            .count()
    });
    let attention = if failures == 0 {
        Line::styled(
            "Collector failures: none",
            Style::default().fg(Color::Green),
        )
    } else {
        Line::styled(
            format!("Collector failures: {failures}"),
            Style::default().fg(Color::Red),
        )
    };
    vec![
        Line::from(format!(
            "Local history: {retained}/{capacity} snapshots · {duration}"
        )),
        attention,
    ]
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::CollectorId;

    #[test]
    fn test_definition_includes_built_ins_and_excludes_mapped_collectors() {
        // Arrange
        let collectors = vec![
            CollectorConfig::System {
                id: CollectorId::new("host").expect("valid fixture"),
                interval_secs: None,
            },
            CollectorConfig::HttpJson {
                id: CollectorId::new("custom").expect("valid fixture"),
                url: "http://127.0.0.1/data".parse().expect("valid fixture"),
                auth: None,
                mappings: Vec::new(),
                interval_secs: None,
            },
        ];

        // Arrange
        let config = Config {
            server: crate::config::ServerConfig::default(),
            schedule: crate::config::ScheduleConfig::default(),
            tui: crate::config::TuiConfig::default(),
            heartbeat: None,
            collectors,
        };

        // Act
        let definition = DashboardDefinition::from_config(&config);

        // Assert
        assert_eq!(
            definition.collectors,
            vec![DashboardCollector::System {
                id: CollectorId::new("host").expect("valid fixture")
            }]
        );
    }
}
