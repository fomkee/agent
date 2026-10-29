use chrono::Utc;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::{
    config::CapacityThresholds,
    model::{CollectorId, FactValue, Observation, ObservationResult, Unit},
};

use super::{DashboardCollector, values};
use crate::tui::{format, history::SnapshotHistory};

pub(super) fn append(
    lines: &mut Vec<Line<'static>>,
    collector: &DashboardCollector,
    history: &SnapshotHistory,
    thresholds: CapacityThresholds,
) {
    match collector {
        DashboardCollector::System { id } => append_system(lines, history, id, thresholds),
        DashboardCollector::Filesystem { .. } => {}
        DashboardCollector::Systemd { id, unit } => append_systemd(lines, history, id, unit),
        DashboardCollector::Container { id, name } => {
            append_container(lines, history, id, name);
        }
        DashboardCollector::Postgres { id } => append_postgres(lines, history, id),
        DashboardCollector::Backup { id, directory } => {
            append_backup(lines, history, id, directory);
        }
    }
}

fn append_system(
    lines: &mut Vec<Line<'static>>,
    history: &SnapshotHistory,
    id: &CollectorId,
    thresholds: CapacityThresholds,
) {
    heading(lines, format!("Host · {id}"));
    let Some(observation) = latest_observation(lines, history, id) else {
        return;
    };
    lines.push(field(
        "Uptime",
        values::formatted(observation, "uptime", Unit::Seconds),
    ));
    let load = values::series(history, id, "load_1m");
    lines.push(field(
        "Load 1m",
        format!(
            "{} · {}  {}",
            values::formatted(observation, "load_1m", Unit::None),
            values::range(&load, Unit::None),
            values::sparkline(&load)
        ),
    ));
    append_memory_capacity(lines, observation, thresholds);
}

pub(super) fn append_filesystems(
    lines: &mut Vec<Line<'static>>,
    collectors: &[DashboardCollector],
    history: &SnapshotHistory,
    thresholds: CapacityThresholds,
) {
    heading(lines, "Storage".to_owned());
    lines.push(storage_header());
    for collector in collectors {
        if let DashboardCollector::Filesystem { id, path } = collector {
            lines.push(storage_row(history, id, path, thresholds));
        }
    }
}

fn storage_header() -> Line<'static> {
    Line::styled(
        format!(
            "  {:<6} {:<18} {:>7} {:<14} {:>12} {:>12}",
            "STATUS", "MOUNT", "USED", "CAPACITY", "AVAILABLE", "UPDATED"
        ),
        Style::default().fg(Color::DarkGray),
    )
}

fn storage_row(
    history: &SnapshotHistory,
    id: &CollectorId,
    path: &str,
    thresholds: CapacityThresholds,
) -> Line<'static> {
    let target = format::truncate(path, 18);
    let Some(observation) = history
        .latest()
        .and_then(|snapshot| values::observation(snapshot, id))
    else {
        return storage_line("WAIT", Color::Yellow, &target, "--", None, "--", "--");
    };
    let age = format::age(observation.collected_at, Utc::now());
    let (_, overdue) =
        format::freshness(observation.collected_at, observation.interval, Utc::now());
    if let ObservationResult::Failed { code, .. } = &observation.result {
        return storage_line("FAIL", Color::Red, &target, "--", None, code, &age);
    }
    let used = values::measurement(observation, "used");
    let total = values::measurement(observation, "total");
    let available = values::formatted(observation, "available", Unit::Bytes);
    let Some(ratio) = used
        .zip(total)
        .and_then(|(used, total)| values::capacity_ratio(used, total))
    else {
        return storage_line("WARN", Color::Yellow, &target, "--", None, &available, &age);
    };
    let state = values::capacity_state(ratio, thresholds);
    let read_only = fact_boolean(observation, "read_only") == Some(true);
    let (status, color) = if overdue {
        ("STALE", Color::Yellow)
    } else if read_only && state == values::CapacityState::Normal {
        ("WARN", Color::Yellow)
    } else {
        capacity_status(state)
    };
    let used = format::measurement(ratio, Unit::Ratio);
    storage_line(status, color, &target, &used, Some(ratio), &available, &age)
}

fn storage_line(
    status: &str,
    color: Color,
    target: &str,
    used: &str,
    ratio: Option<f64>,
    available: &str,
    age: &str,
) -> Line<'static> {
    let target = format::truncate(target, 18);
    let used = format::truncate(used, 7);
    let available = format::truncate(available, 12);
    let age = format::truncate(age, 12);
    let (filled, empty) = ratio.map_or_else(
        || (String::new(), "──────────────".to_owned()),
        |ratio| values::capacity_bar(ratio, 14),
    );
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{status:<6}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {target:<18} {used:>7} ")),
        Span::styled(filled, Style::default().fg(color)),
        Span::styled(empty, Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {available:>12} {age:>12}")),
    ])
}

fn capacity_status(state: values::CapacityState) -> (&'static str, Color) {
    match state {
        values::CapacityState::Normal => ("OK", Color::Green),
        values::CapacityState::Warning => ("WARN", Color::Yellow),
        values::CapacityState::Critical => ("CRIT", Color::Red),
    }
}

fn append_systemd(
    lines: &mut Vec<Line<'static>>,
    history: &SnapshotHistory,
    id: &CollectorId,
    unit: &str,
) {
    heading(lines, format!("Service · {unit} · {id}"));
    let Some(observation) = latest_observation(lines, history, id) else {
        return;
    };
    lines.push(state_field(
        "State",
        values::fact_text(observation, "active_state"),
        fact_boolean(observation, "active"),
    ));
    lines.push(field(
        "Sub-state",
        values::fact_text(observation, "sub_state"),
    ));
    append_activity(
        lines,
        history,
        id,
        observation,
        "restarts_total",
        "Restarts",
    );
}

fn append_container(
    lines: &mut Vec<Line<'static>>,
    history: &SnapshotHistory,
    id: &CollectorId,
    name: &str,
) {
    heading(lines, format!("Container · {name} · {id}"));
    let Some(observation) = latest_observation(lines, history, id) else {
        return;
    };
    lines.push(state_field(
        "State",
        values::fact_text(observation, "status"),
        fact_boolean(observation, "running"),
    ));
    lines.push(field(
        "Exit code",
        values::formatted(observation, "exit_code", Unit::None),
    ));
    append_activity(
        lines,
        history,
        id,
        observation,
        "restarts_total",
        "Restarts",
    );
}

fn append_postgres(lines: &mut Vec<Line<'static>>, history: &SnapshotHistory, id: &CollectorId) {
    heading(lines, format!("PostgreSQL · {id}"));
    let Some(observation) = latest_observation(lines, history, id) else {
        return;
    };
    lines.push(field(
        "Database size",
        format!(
            "{} · session {}",
            values::formatted(observation, "database_size", Unit::Bytes),
            values::session_change(history, id, "database_size", Unit::Bytes)
        ),
    ));
    let connections = values::series(history, id, "connections");
    lines.push(field(
        "Connections",
        format!(
            "{} · {}  {}",
            values::formatted(observation, "connections", Unit::Count),
            values::range(&connections, Unit::Count),
            values::sparkline(&connections)
        ),
    ));
    lines.push(field(
        "Temp I/O session",
        format!(
            "{} across {} files",
            values::session_activity(history, id, "temp_bytes_total_delta", Unit::Bytes),
            values::session_activity(history, id, "temp_files_total_delta", Unit::Count)
        ),
    ));
    lines.push(field(
        "Deadlocks session",
        values::session_activity(history, id, "deadlocks_total_delta", Unit::Count),
    ));
}

fn append_backup(
    lines: &mut Vec<Line<'static>>,
    history: &SnapshotHistory,
    id: &CollectorId,
    directory: &str,
) {
    heading(lines, format!("Backups · {id} · {directory}"));
    let Some(observation) = latest_observation(lines, history, id) else {
        return;
    };
    lines.push(field(
        "Latest",
        format!(
            "{} ago · {}",
            values::formatted(observation, "latest_age", Unit::Seconds),
            values::fact_text(observation, "latest_modified_at")
        ),
    ));
    lines.push(field(
        "Latest size",
        values::formatted(observation, "latest_size", Unit::Bytes),
    ));
    lines.push(field(
        "Files",
        values::formatted(observation, "files", Unit::Count),
    ));
}

fn append_memory_capacity(
    lines: &mut Vec<Line<'static>>,
    observation: &Observation,
    thresholds: CapacityThresholds,
) {
    let used = values::measurement(observation, "memory_used");
    let total = values::measurement(observation, "memory_total");
    match (used, total) {
        (Some(used), Some(total)) => {
            let color = values::capacity_ratio(used, total)
                .map(|ratio| capacity_status(values::capacity_state(ratio, thresholds)).1)
                .unwrap_or(Color::Yellow);
            lines.push(styled_field(
                "Memory",
                format!(
                    "{} / {}  {}",
                    format::measurement(used, Unit::Bytes),
                    format::measurement(total, Unit::Bytes),
                    values::capacity(used, total)
                ),
                color,
            ));
            lines.push(field(
                "Available",
                values::formatted(observation, "memory_available", Unit::Bytes),
            ));
        }
        _ => lines.push(field("Memory", "unavailable".to_owned())),
    }
}

fn append_activity(
    lines: &mut Vec<Line<'static>>,
    history: &SnapshotHistory,
    id: &CollectorId,
    observation: &Observation,
    total_name: &str,
    label: &str,
) {
    lines.push(field(
        label,
        format!(
            "{} during session · {} total",
            values::session_activity(history, id, &format!("{total_name}_delta"), Unit::Count),
            values::formatted(observation, total_name, Unit::Count)
        ),
    ));
}

fn latest_observation<'a>(
    lines: &mut Vec<Line<'static>>,
    history: &'a SnapshotHistory,
    id: &CollectorId,
) -> Option<&'a Observation> {
    let observation = history
        .latest()
        .and_then(|snapshot| values::observation(snapshot, id));
    match observation {
        Some(
            observation @ Observation {
                result: ObservationResult::Collected { .. },
                ..
            },
        ) => {
            append_freshness(lines, observation);
            Some(observation)
        }
        Some(
            observation @ Observation {
                result: ObservationResult::Failed { code, summary },
                ..
            },
        ) => {
            append_freshness(lines, observation);
            lines.push(Line::styled(
                format!("  FAILED [{code}] {summary}"),
                Style::default().fg(Color::Red),
            ));
            None
        }
        None => {
            lines.push(Line::from("  Waiting for the first observation…"));
            None
        }
    }
}

fn append_freshness(lines: &mut Vec<Line<'static>>, observation: &Observation) {
    let (freshness, overdue) =
        format::freshness(observation.collected_at, observation.interval, Utc::now());
    lines.push(state_field("Updated", freshness, Some(!overdue)));
}

fn fact_boolean(observation: &Observation, name: &str) -> Option<bool> {
    match values::fact(observation, name) {
        Some(FactValue::Boolean(value)) => Some(*value),
        Some(FactValue::Text(_) | FactValue::Timestamp(_)) | None => None,
    }
}

fn heading(lines: &mut Vec<Line<'static>>, title: String) {
    lines.push(Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
}

fn styled_field(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<18}"), Style::default().fg(Color::Gray)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<18}"), Style::default().fg(Color::Gray)),
        Span::raw(value),
    ])
}

fn state_field(label: &str, value: String, healthy: Option<bool>) -> Line<'static> {
    let color = match healthy {
        Some(true) => Color::Green,
        Some(false) => Color::Red,
        None => Color::Yellow,
    };
    Line::from(vec![
        Span::styled(format!("  {label:<18}"), Style::default().fg(Color::Gray)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_update_ages_share_one_column() {
        // Arrange
        let rows = [
            storage_line(
                "OK",
                Color::Green,
                "/",
                "12%",
                Some(0.12),
                "8 GiB",
                "2s ago",
            ),
            storage_line(
                "WARN",
                Color::Yellow,
                "/srv/data",
                "82%",
                Some(0.82),
                "38 GiB",
                "38s ago",
            ),
            storage_line("FAIL", Color::Red, "/backup", "--", None, "--", "4m ago"),
        ];

        // Act
        let rendered = rows.iter().map(line_text).collect::<Vec<_>>();

        // Assert
        let mut lines = rendered.iter();
        let first = lines.next().expect("first row");
        let second = lines.next().expect("second row");
        let third = lines.next().expect("third row");
        assert!(rendered.iter().all(|line| line.len() == first.len()));
        assert!(first.ends_with("      2s ago"));
        assert!(second.ends_with("     38s ago"));
        assert!(third.ends_with("      4m ago"));
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
