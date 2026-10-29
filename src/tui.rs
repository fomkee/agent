use std::{
    io::{self, Stdout},
    path::Path,
    time::Duration,
};

use chrono::Utc;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Frame, Terminal, backend::CrosstermBackend, style::Color};
use thiserror::Error;
use tokio::net::UnixStream;

use crate::{
    config::{Config, MAX_TUI_HISTORY_SNAPSHOTS, TuiDefaultView},
    local_socket::{self, LocalSocketError},
    model::{Observation, Snapshot},
};

mod dashboard;
mod format;
mod history;
mod render;

use dashboard::DashboardDefinition;
use history::SnapshotHistory;

const DEFAULT_HISTORY_CAPACITY: usize = 120;

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("failed to connect to local agent socket")]
    Connect(#[source] std::io::Error),
    #[error("local agent snapshot stream failed")]
    Snapshot(#[from] LocalSocketError),
    #[error("terminal operation failed")]
    Terminal(#[source] std::io::Error),
    #[error("terminal event stream ended")]
    EventStreamEnded,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TuiSettingsError {
    #[error("TUI history capacity must be between 1 and {MAX_TUI_HISTORY_SNAPSHOTS}")]
    InvalidHistoryCapacity,
}

/// Runtime settings and semantic layout for the local terminal dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiSettings {
    history_capacity: usize,
    initial_page: TuiPage,
    dashboard: DashboardDefinition,
}

impl TuiSettings {
    /// Builds TUI settings from the validated agent configuration.
    pub fn from_config(config: &Config) -> Result<Self, TuiSettingsError> {
        Self::default()
            .with_history_capacity(config.tui.history_snapshots)
            .map(|settings| Self {
                initial_page: TuiPage::from(config.tui.default_view),
                dashboard: DashboardDefinition::from_config(config),
                ..settings
            })
    }

    /// Returns settings with a validated snapshot-history capacity.
    #[must_use = "settings changes are lost unless the returned value is used"]
    pub fn with_history_capacity(self, history_capacity: usize) -> Result<Self, TuiSettingsError> {
        if (1..=MAX_TUI_HISTORY_SNAPSHOTS).contains(&history_capacity) {
            Ok(Self {
                history_capacity,
                ..self
            })
        } else {
            Err(TuiSettingsError::InvalidHistoryCapacity)
        }
    }

    fn history_capacity(&self) -> usize {
        self.history_capacity
    }
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            history_capacity: DEFAULT_HISTORY_CAPACITY,
            initial_page: TuiPage::Dashboard,
            dashboard: DashboardDefinition::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiPage {
    Dashboard,
    Collectors,
}

impl From<TuiDefaultView> for TuiPage {
    fn from(value: TuiDefaultView) -> Self {
        match value {
            TuiDefaultView::Dashboard => Self::Dashboard,
            TuiDefaultView::Collectors => Self::Collectors,
        }
    }
}

/// Runs the interactive dashboard over the daemon's local snapshot stream.
pub async fn run(
    path: &Path,
    max_snapshot_age: Duration,
    settings: TuiSettings,
) -> Result<(), TuiError> {
    let mut stream = UnixStream::connect(path).await.map_err(TuiError::Connect)?;
    let mut terminal = TerminalSession::new()?;
    let mut events = EventStream::new();
    let mut model = TuiModel::new(max_snapshot_age, settings);

    loop {
        terminal
            .draw(|frame| render::render(frame, &model))
            .map_err(TuiError::Terminal)?;
        tokio::select! {
            snapshot = local_socket::read_snapshot(&mut stream) => {
                model.set_snapshot(snapshot?);
            }
            event = events.next() => {
                let event = event.ok_or(TuiError::EventStreamEnded)?
                    .map_err(TuiError::Terminal)?;
                if model.handle_event(event) {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Debug)]
struct TuiModel {
    history: SnapshotHistory,
    dashboard: DashboardDefinition,
    page: TuiPage,
    dashboard_scroll: u16,
    selected_collector: usize,
    max_snapshot_age: Duration,
}

impl TuiModel {
    fn new(max_snapshot_age: Duration, settings: TuiSettings) -> Self {
        Self {
            history: SnapshotHistory::new(settings.history_capacity()),
            dashboard: settings.dashboard,
            page: settings.initial_page,
            dashboard_scroll: 0,
            selected_collector: 0,
            max_snapshot_age,
        }
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.history.push(snapshot);
        self.clamp_collector();
    }

    fn handle_event(&mut self, event: Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != KeyEventKind::Press {
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('1') | KeyCode::Char('d') => self.page = TuiPage::Dashboard,
            KeyCode::Char('2') | KeyCode::Char('c') => self.page = TuiPage::Collectors,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.toggle_page();
            }
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Home => self.dashboard_scroll = 0,
            KeyCode::PageUp => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(5);
            }
            KeyCode::PageDown => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(5);
            }
            KeyCode::Enter
            | KeyCode::End
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Insert
            | KeyCode::Char(_)
            | KeyCode::F(_)
            | KeyCode::Null
            | KeyCode::CapsLock
            | KeyCode::ScrollLock
            | KeyCode::NumLock
            | KeyCode::PrintScreen
            | KeyCode::Pause
            | KeyCode::Menu
            | KeyCode::KeypadBegin
            | KeyCode::Media(_)
            | KeyCode::Modifier(_) => {}
        }
        false
    }

    fn toggle_page(&mut self) {
        self.page = match self.page {
            TuiPage::Dashboard => TuiPage::Collectors,
            TuiPage::Collectors => TuiPage::Dashboard,
        };
    }

    fn select_previous(&mut self) {
        match self.page {
            TuiPage::Dashboard => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_sub(1);
            }
            TuiPage::Collectors => {
                self.selected_collector = self.selected_collector.saturating_sub(1);
            }
        }
    }

    fn select_next(&mut self) {
        match self.page {
            TuiPage::Dashboard => {
                self.dashboard_scroll = self.dashboard_scroll.saturating_add(1);
            }
            TuiPage::Collectors => {
                self.selected_collector = self.selected_collector.saturating_add(1);
                self.clamp_collector();
            }
        }
    }

    fn clamp_collector(&mut self) {
        let length = self
            .history
            .latest()
            .map_or(0, |snapshot| snapshot.observations.len());
        self.selected_collector = clamp_index(self.selected_collector, length);
    }

    fn latest_snapshot(&self) -> Option<&Snapshot> {
        self.history.latest()
    }

    fn selected_observation(&self) -> Option<&Observation> {
        self.latest_snapshot()
            .and_then(|snapshot| snapshot.observations.get(self.selected_collector))
    }

    fn freshness(&self) -> (&'static str, Color) {
        let Some(snapshot) = self.latest_snapshot() else {
            return ("connecting", Color::Yellow);
        };
        if snapshot.sequence == 0 {
            return ("starting", Color::Yellow);
        }
        let fresh = Utc::now()
            .signed_duration_since(snapshot.generated_at)
            .to_std()
            .is_ok_and(|age| age <= self.max_snapshot_age);
        if fresh {
            ("live", Color::Green)
        } else {
            ("stale", Color::Red)
        }
    }
}

fn clamp_index(index: usize, length: usize) -> usize {
    if length == 0 {
        0
    } else {
        index.min(length.saturating_sub(1))
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self, TuiError> {
        enable_raw_mode().map_err(TuiError::Terminal)?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(TuiError::Terminal(error));
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let mut cleanup = io::stdout();
                let _ = execute!(cleanup, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(TuiError::Terminal(error))
            }
        }
    }

    fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::{CollectorConfig, ScheduleConfig, ServerConfig, TuiConfig};

    #[test]
    fn test_tab_switches_between_dashboard_and_collectors() {
        // Arrange
        let mut model = a_model();

        // Act
        model.handle_event(key_event(KeyCode::Tab));
        let collectors = model.page;
        model.handle_event(key_event(KeyCode::Tab));

        // Assert
        assert_eq!(collectors, TuiPage::Collectors);
        assert_eq!(model.page, TuiPage::Dashboard);
    }

    #[test]
    fn test_dashboard_navigation_scrolls_without_selecting_collector() {
        // Arrange
        let mut model = a_model();

        // Act
        model.handle_event(key_event(KeyCode::Down));

        // Assert
        assert_eq!(model.dashboard_scroll, 1);
        assert_eq!(model.selected_collector, 0);
    }

    #[test]
    fn test_history_capacity_setting_rejects_unbounded_values() {
        // Arrange
        let excessive = MAX_TUI_HISTORY_SNAPSHOTS.saturating_add(1);

        // Act
        let zero = TuiSettings::default().with_history_capacity(0);
        let excessive = TuiSettings::default().with_history_capacity(excessive);

        // Assert
        assert_eq!(zero, Err(TuiSettingsError::InvalidHistoryCapacity));
        assert_eq!(excessive, Err(TuiSettingsError::InvalidHistoryCapacity));
    }

    #[test]
    fn test_settings_follow_configured_page_and_history() {
        // Arrange
        let config = a_config(TuiConfig {
            history_snapshots: 240,
            default_view: TuiDefaultView::Collectors,
            ..TuiConfig::default()
        });

        // Act
        let settings = TuiSettings::from_config(&config).expect("valid fixture");

        // Assert
        assert_eq!(settings.history_capacity, 240);
        assert_eq!(settings.initial_page, TuiPage::Collectors);
    }

    #[test]
    fn test_quit_keys_request_exit() {
        // Arrange
        let mut model = a_model();

        // Act
        let q_quits = model.handle_event(key_event(KeyCode::Char('q')));
        let escape_quits = model.handle_event(key_event(KeyCode::Esc));

        // Assert
        assert!(q_quits);
        assert!(escape_quits);
    }

    fn a_model() -> TuiModel {
        TuiModel::new(Duration::from_secs(90), TuiSettings::default())
    }

    fn a_config(tui: TuiConfig) -> Config {
        Config {
            server: ServerConfig::default(),
            schedule: ScheduleConfig::default(),
            tui,
            heartbeat: None,
            collectors: vec![CollectorConfig::System {
                id: crate::model::CollectorId::new("host").expect("valid fixture"),
                interval_secs: None,
            }],
        }
    }

    fn key_event(code: KeyCode) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        ))
    }
}
