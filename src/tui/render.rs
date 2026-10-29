use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
};

use crate::model::{Measurement, Observation, ObservationResult};

use super::{TuiModel, TuiPage, dashboard, format};

pub(super) fn render(frame: &mut Frame<'_>, model: &TuiModel) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let [header_area, body_area, footer_area] = sections.as_ref() else {
        return;
    };
    render_header(frame, *header_area, model);
    match model.page {
        TuiPage::Dashboard => dashboard::render(frame, *body_area, model),
        TuiPage::Collectors => render_collectors_page(frame, *body_area, model),
    }
    render_footer(frame, *footer_area, model.page);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let (freshness, freshness_color) = model.freshness();
    let sequence = model
        .latest_snapshot()
        .map_or(0, |snapshot| snapshot.sequence);
    let mut spans = vec![Span::styled(
        " Fomkee Agent  ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    spans.extend([
        tab(" 1 Dashboard ", model.page == TuiPage::Dashboard),
        Span::raw(" "),
        tab(" 2 Collectors ", model.page == TuiPage::Collectors),
        Span::raw("   "),
        Span::styled(freshness, Style::default().fg(freshness_color)),
        Span::raw(format!("  snapshot #{sequence}")),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn tab(label: &'static str, selected: bool) -> Span<'static> {
    if selected {
        Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(label, Style::default().fg(Color::DarkGray))
    }
}

fn render_collectors_page(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    let [collector_area, detail_area] = body.as_ref() else {
        return;
    };
    render_collectors(frame, *collector_area, model);
    render_details(frame, *detail_area, model);
}

fn render_collectors(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let rows = model
        .latest_snapshot()
        .map(|snapshot| {
            snapshot
                .observations
                .iter()
                .enumerate()
                .map(|(index, observation)| {
                    let (status, color) = observation_status(observation);
                    let marker = if index == model.selected_collector {
                        ">"
                    } else {
                        " "
                    };
                    Row::new(vec![
                        Cell::from(marker),
                        Cell::from(observation.collector_id.to_string()),
                        Cell::from(status).style(Style::default().fg(color)),
                        Cell::from(format::collection_duration(observation.duration_ms)),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Percentage(48),
            Constraint::Percentage(28),
            Constraint::Percentage(24),
        ],
    )
    .header(
        Row::new(["", "Collector", "Status", "Duration"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().title(" Collectors ").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn render_details(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let lines = model.selected_observation().map_or_else(
        || vec![Line::from("Waiting for the first snapshot…")],
        detail_lines,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Details ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn detail_lines(observation: &Observation) -> Vec<Line<'static>> {
    let (freshness, overdue) = format::freshness(
        observation.collected_at,
        observation.interval,
        chrono::Utc::now(),
    );
    let freshness_color = if overdue { Color::Red } else { Color::Green };
    let mut lines = vec![
        Line::from(Span::styled(
            observation.collector_id.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "collected: {}",
            format::timestamp(observation.collected_at)
        )),
        Line::from(Span::styled(
            format!("updated: {freshness}"),
            Style::default().fg(freshness_color),
        )),
        Line::from(""),
    ];
    match &observation.result {
        ObservationResult::Failed { code, summary } => {
            lines.push(Line::from(Span::styled(
                format!("FAILED [{code}]"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(summary.clone()));
        }
        ObservationResult::Collected {
            measurements,
            facts,
        } => {
            lines.push(Line::from(Span::styled(
                "Measurements",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for measurement in measurements {
                lines.push(Line::from(format!(
                    "  {} = {}{}",
                    measurement.name,
                    format::measurement(measurement.value.get(), measurement.unit),
                    labels(measurement)
                )));
            }
            if !facts.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Facts",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for fact in facts {
                    lines.push(Line::from(format!(
                        "  {} = {}",
                        fact.name,
                        format::fact(&fact.value)
                    )));
                }
            }
        }
    }
    lines
}

fn labels(measurement: &Measurement) -> String {
    if measurement.labels.is_empty() {
        String::new()
    } else {
        format!(
            " {{{}}}",
            measurement
                .labels
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn observation_status(observation: &Observation) -> (&'static str, Color) {
    match observation.result {
        ObservationResult::Collected { .. } => ("collected", Color::Green),
        ObservationResult::Failed { .. } => ("failed", Color::Red),
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, page: TuiPage) {
    let help = match page {
        TuiPage::Dashboard => " ↑/k ↓/j scroll  PgUp/PgDn page  Tab/2 collectors  q/Esc quit ",
        TuiPage::Collectors => " ↑/k ↓/j select  Tab/1 dashboard  q/Esc quit ",
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::model::{
        CollectionInterval, CollectorId, Fact, FactName, FactValue, MeasurementKind, Unit,
    };

    #[test]
    fn test_details_use_human_readable_values_and_safe_failure_summary() {
        // Arrange
        let collected = collected_observation(1_610_612_736.0, MeasurementKind::Gauge);
        let failed = failed_observation();

        // Act
        let collected_text = lines_text(detail_lines(&collected));
        let failed_text = lines_text(detail_lines(&failed));

        // Assert
        assert!(collected_text.contains("memory = 1.5 GiB"));
        assert!(collected_text.contains("updated = 2026-10-20T16:20:33Z"));
        assert!(failed_text.contains("FAILED [permission_denied]"));
        assert!(failed_text.contains("cannot inspect service"));
    }

    fn collected_observation(value: f64, kind: MeasurementKind) -> Observation {
        Observation {
            collector_id: CollectorId::new("host").expect("valid fixture"),
            interval: CollectionInterval::from_secs(30).expect("valid fixture"),
            collected_at: fixture_time(),
            duration_ms: 4,
            result: ObservationResult::Collected {
                measurements: vec![
                    Measurement::scalar("memory", kind, Unit::Bytes, value).expect("valid fixture"),
                ],
                facts: vec![Fact {
                    name: FactName::new("updated").expect("valid fixture"),
                    value: FactValue::Timestamp(fixture_time()),
                }],
            },
        }
    }

    fn failed_observation() -> Observation {
        Observation {
            collector_id: CollectorId::new("host").expect("valid fixture"),
            interval: CollectionInterval::from_secs(30).expect("valid fixture"),
            collected_at: fixture_time(),
            duration_ms: 2,
            result: ObservationResult::Failed {
                code: "permission_denied".to_owned(),
                summary: "cannot inspect service".to_owned(),
            },
        }
    }

    fn fixture_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 16, 20, 33)
            .single()
            .expect("valid fixture")
    }

    fn lines_text(lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
