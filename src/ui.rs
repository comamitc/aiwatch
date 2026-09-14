use std::{io, time::Duration};

use anyhow::Result;
use chrono::{Local, Utc};
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::{mpsc, watch};

use crate::{
    model::{AccountSnapshot, DashboardSnapshot, HealthState, Provider, UsageWindow},
    output::{format_reset, health_text},
};

const TERMINAL_BACKGROUND: Color = Color::Reset;
const TERMINAL_FOREGROUND: Color = Color::Reset;
const TERMINAL_MUTED: Color = Color::DarkGray;
const TERMINAL_INFO: Color = Color::LightCyan;
const TERMINAL_SUCCESS: Color = Color::LightGreen;
const TERMINAL_WARNING: Color = Color::Yellow;
const TERMINAL_ACCENT: Color = Color::LightMagenta;
const TERMINAL_SECONDARY: Color = Color::Magenta;
const TERMINAL_DANGER: Color = Color::LightRed;

struct AppState {
    provider: Option<Provider>,
    weekly_only: bool,
    selected_account: usize,
    scroll: u16,
}

impl AppState {
    fn new() -> Self {
        Self {
            provider: None,
            weekly_only: false,
            selected_account: 0,
            scroll: 0,
        }
    }

    fn select_provider(&mut self, provider: Option<Provider>) {
        self.provider = provider;
        self.selected_account = 0;
        self.scroll = 0;
    }

    fn next_account(&mut self, count: usize) {
        if count > 0 {
            let current = self.selected_account.min(count - 1);
            self.selected_account = (current + 1) % count;
            self.scroll = 0;
        }
    }

    fn previous_account(&mut self, count: usize) {
        if count > 0 {
            let current = self.selected_account.min(count - 1);
            self.selected_account = if current == 0 { count - 1 } else { current - 1 };
            self.scroll = 0;
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub async fn run(
    mut snapshots: watch::Receiver<DashboardSnapshot>,
    refresh: mpsc::Sender<()>,
    poll_interval: Duration,
) -> Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state = AppState::new();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut snapshots_open = true;

    loop {
        let snapshot = snapshots.borrow().clone();
        terminal.draw(|frame| render(frame, &snapshot, &state, poll_interval))?;

        tokio::select! {
            _ = tick.tick() => {}
            changed = snapshots.changed(), if snapshots_open => {
                snapshots_open = changed.is_ok();
            }
            event = events.next() => {
                let Some(event) = event else { break };
                let event = event?;
                if let Event::Key(key) = event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                        break;
                    }
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('r') => { let _ = refresh.try_send(()); }
                        KeyCode::Char('0') => state.select_provider(None),
                        KeyCode::Char('1') => state.select_provider(Some(Provider::Claude)),
                        KeyCode::Char('2') => state.select_provider(Some(Provider::Codex)),
                        KeyCode::Char('3') => state.select_provider(Some(Provider::Grok)),
                        KeyCode::Char('w') => { state.weekly_only = !state.weekly_only; state.scroll = 0; }
                        KeyCode::Char('j') | KeyCode::Right => {
                            state.next_account(matching_account_count(&snapshot, state.provider));
                        }
                        KeyCode::Char('k') | KeyCode::Left => {
                            state.previous_account(matching_account_count(&snapshot, state.provider));
                        }
                        KeyCode::Down | KeyCode::PageDown => {
                            state.scroll = state.scroll.saturating_add(1);
                        }
                        KeyCode::Up | KeyCode::PageUp => {
                            state.scroll = state.scroll.saturating_sub(1);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}

fn render(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, state: &AppState, poll: Duration) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(TERMINAL_FOREGROUND)
                .bg(TERMINAL_BACKGROUND),
        ),
        area,
    );
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, snapshot, poll, header);
    render_body(frame, snapshot, state, body);
    render_footer(frame, state, footer);
}

fn render_header(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, poll: Duration, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " aiwatch ",
            Style::default()
                .fg(TERMINAL_ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            env!("CARGO_PKG_VERSION"),
            Style::default().fg(TERMINAL_SECONDARY),
        ),
        Span::styled(
            format!(
                "  │  {} accounts · {} providers  │  provider poll {}s",
                snapshot.accounts.len(),
                snapshot.provider_count(),
                poll.as_secs()
            ),
            Style::default().fg(TERMINAL_MUTED),
        ),
    ]);
    let clock = Local::now().format("%H:%M:%S").to_string();
    let [left, right] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(12)]).areas(area);
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().fg(TERMINAL_MUTED).bg(TERMINAL_BACKGROUND))
            .block(Block::default().borders(Borders::BOTTOM)),
        left,
    );
    frame.render_widget(
        Paragraph::new(format!("{clock} ▌ "))
            .right_aligned()
            .style(
                Style::default()
                    .fg(TERMINAL_SUCCESS)
                    .bg(TERMINAL_BACKGROUND),
            )
            .block(Block::default().borders(Borders::BOTTOM)),
        right,
    );
}

fn render_body(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, state: &AppState, area: Rect) {
    let Some((index, count, account)) = selected_account(snapshot, state) else {
        frame.render_widget(
            Paragraph::new(
                "No matching accounts. Configure credentials or clear the provider filter.",
            )
            .style(
                Style::default()
                    .fg(TERMINAL_WARNING)
                    .bg(TERMINAL_BACKGROUND),
            )
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(TERMINAL_MUTED))
                    .title(" ACCOUNT "),
            ),
            area,
        );
        return;
    };

    let content_width = area.width.saturating_sub(6) as usize;
    let lines = account_card_lines(account, index, count, content_width, state.weekly_only);
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", account.provider.label()),
            Style::default()
                .fg(usage_color(
                    account
                        .windows
                        .iter()
                        .map(|window| window.used_percent)
                        .max_by(f64::total_cmp)
                        .unwrap_or_default(),
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("/ {} ", account.name),
            Style::default()
                .fg(TERMINAL_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .style(
                Style::default()
                    .fg(TERMINAL_FOREGROUND)
                    .bg(TERMINAL_BACKGROUND),
            )
            .scroll((state.scroll, 0))
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(TERMINAL_MUTED))
                    .title(title)
                    .padding(ratatui::widgets::Padding::horizontal(2)),
            ),
        area,
    );
}

fn matching_account_count(snapshot: &DashboardSnapshot, provider: Option<Provider>) -> usize {
    snapshot
        .accounts
        .iter()
        .filter(|account| provider.is_none_or(|filter| account.provider == filter))
        .count()
}

fn selected_account<'a>(
    snapshot: &'a DashboardSnapshot,
    state: &AppState,
) -> Option<(usize, usize, &'a AccountSnapshot)> {
    let count = matching_account_count(snapshot, state.provider);
    if count == 0 {
        return None;
    }
    let index = state.selected_account.min(count - 1);
    snapshot
        .accounts
        .iter()
        .filter(|account| {
            state
                .provider
                .is_none_or(|filter| account.provider == filter)
        })
        .nth(index)
        .map(|account| (index, count, account))
}

fn account_card_lines(
    account: &AccountSnapshot,
    index: usize,
    count: usize,
    width: usize,
    weekly_only: bool,
) -> Vec<Line<'static>> {
    let status_color = health_color(account.health.state);
    let meta = format!(
        "Updated {} · {}",
        age_text(Utc::now() - account.fetched_at),
        health_text(account.health.state, account.health.status_code)
    );
    let position = account.plan.as_deref().map_or_else(
        || format!("{} of {count}", index + 1),
        |plan| format!("{plan} · {} of {count}", index + 1),
    );
    let mut lines = vec![
        aligned_line(
            meta,
            position,
            width,
            Style::default().fg(status_color),
            Style::default()
                .fg(TERMINAL_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ),
        divider_line(width),
        Line::from(""),
    ];

    let mut rendered_window = false;
    for window in account.windows.iter().filter(|window| {
        !weekly_only || window.key.contains("weekly") || window.key.contains("monthly")
    }) {
        rendered_window = true;
        lines.extend(window_section(window, width));
        lines.push(Line::from(""));
    }
    if !rendered_window {
        let message = account.health.message.as_deref().unwrap_or(if weekly_only {
            "No weekly usage window reported"
        } else {
            "Quota unavailable"
        });
        lines.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(status_color),
        )));
    }

    if !account.details.is_empty() {
        lines.push(divider_line(width));
        lines.push(Line::from(Span::styled(
            "DETAILS",
            Style::default()
                .fg(TERMINAL_ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.extend(detail_grid_lines(account, width));
    }
    lines
}

fn window_section(window: &UsageWindow, width: usize) -> Vec<Line<'static>> {
    let color = usage_color(window.used_percent);
    let heading = format!("{} {:.1}% used", window.label, window.used_percent);
    let bar_width = width.clamp(4, 80);
    let filled = ((window.used_percent / 100.0) * bar_width as f64).round() as usize;
    let mut lines = vec![
        aligned_line(
            heading,
            format_reset(window.resets_at),
            width,
            Style::default()
                .fg(TERMINAL_FOREGROUND)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(TERMINAL_MUTED),
        ),
        Line::from(vec![
            Span::styled("█".repeat(filled), Style::default().fg(color)),
            Span::styled(
                pointillist_bar(bar_width - filled),
                Style::default().fg(TERMINAL_MUTED),
            ),
        ]),
    ];

    if window.history.iter().any(|value| *value > 0) {
        let peak = window.history.iter().copied().max().unwrap_or_default();
        lines.push(aligned_line(
            "7D PEAKS".to_string(),
            format!("peak {peak}%"),
            width,
            Style::default()
                .fg(TERMINAL_INFO)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(TERMINAL_MUTED),
        ));
        lines.extend(history_chart_lines(&window.history, width));
    }
    lines
}

fn history_chart_lines(history: &[u64], width: usize) -> Vec<Line<'static>> {
    const HEIGHT: usize = 4;
    const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let history = &history[history.len().saturating_sub(7)..];
    if history.is_empty() {
        return Vec::new();
    }

    let cell_width = (width / history.len()).clamp(1, 8);
    let bar_width = cell_width.min(3).saturating_sub(1).max(1);
    let chart_width = cell_width * history.len();
    let mut lines = Vec::with_capacity(HEIGHT + 2);
    for row in (0..HEIGHT).rev() {
        let mut chart = String::with_capacity(chart_width.saturating_mul(3));
        for value in history {
            let units = ((*value).min(100) as usize * HEIGHT * 8 + 50) / 100;
            let level = units.saturating_sub(row * 8).min(8);
            for _ in 0..bar_width {
                chart.push(LEVELS[level]);
            }
            for _ in bar_width..cell_width {
                chart.push(' ');
            }
        }
        lines.push(Line::from(Span::styled(
            chart,
            Style::default().fg(TERMINAL_INFO),
        )));
    }
    lines.push(Line::from(Span::styled(
        "─".repeat(chart_width),
        Style::default().fg(TERMINAL_MUTED),
    )));
    lines.push(aligned_line(
        "oldest".to_string(),
        "today".to_string(),
        chart_width,
        Style::default().fg(TERMINAL_MUTED),
        Style::default().fg(TERMINAL_MUTED),
    ));
    lines
}

fn detail_grid_lines(account: &AccountSnapshot, width: usize) -> Vec<Line<'static>> {
    let column_width = width.saturating_sub(3) / 2;
    let mut lines = Vec::new();
    for row in account.details.chunks(2) {
        let right_label = row.get(1).map_or("", |detail| detail.label.as_str());
        lines.push(two_column_line(
            &row[0].label,
            right_label,
            column_width,
            Style::default().fg(TERMINAL_MUTED),
        ));
        let right_value = row.get(1).map_or("", |detail| detail.value.as_str());
        lines.push(two_column_line(
            &row[0].value,
            right_value,
            column_width,
            Style::default()
                .fg(TERMINAL_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(""));
    }
    lines
}

fn two_column_line(left: &str, right: &str, column_width: usize, style: Style) -> Line<'static> {
    let gap = "   ";
    Line::from(vec![
        Span::styled(format!("{left:<column_width$}"), style),
        Span::raw(gap),
        Span::styled(right.to_string(), style),
    ])
}

fn aligned_line(
    left: String,
    right: String,
    width: usize,
    left_style: Style,
    right_style: Style,
) -> Line<'static> {
    let gap = width
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(2);
    Line::from(vec![
        Span::styled(left, left_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, right_style),
    ])
}

fn divider_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(TERMINAL_MUTED),
    ))
}

fn health_color(state: HealthState) -> Color {
    match state {
        HealthState::Ok => TERMINAL_SUCCESS,
        HealthState::Stale => TERMINAL_WARNING,
        HealthState::AuthenticationRequired | HealthState::RateLimited | HealthState::Error => {
            TERMINAL_DANGER
        }
    }
}

fn render_footer(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let filter = state.provider.map_or("all", Provider::key);
    let weekly = if state.weekly_only {
        "weekly"
    } else {
        "all windows"
    };
    frame.render_widget(
        Paragraph::new(format!(
            " q quit  r refresh  j/k or ←/→ account  ↑/↓ scroll  0-3 provider  w weekly  │  {filter} · {weekly}"
        ))
        .style(
            Style::default()
                .fg(TERMINAL_MUTED)
                .bg(TERMINAL_BACKGROUND),
        ),
        area,
    );
}

fn pointillist_bar(width: usize) -> String {
    let mut bar = "⠂⠄".repeat(width / 2);
    if width % 2 == 1 {
        bar.push('⠂');
    }
    bar
}

fn usage_color(percent: f64) -> Color {
    if percent >= 90.0 {
        TERMINAL_DANGER
    } else if percent >= 70.0 {
        TERMINAL_WARNING
    } else {
        TERMINAL_SUCCESS
    }
}

fn age_text(age: chrono::Duration) -> String {
    if age.num_seconds() < 2 {
        "now".to_string()
    } else if age.num_minutes() < 1 {
        format!("{}s ago", age.num_seconds())
    } else if age.num_hours() < 1 {
        format!("{}m ago", age.num_minutes())
    } else {
        format!("{}h ago", age.num_hours())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_navigation_wraps_and_resets_scroll() {
        let mut state = AppState::new();
        state.scroll = 4;
        state.next_account(3);
        assert_eq!(state.selected_account, 1);
        assert_eq!(state.scroll, 0);

        state.previous_account(3);
        assert_eq!(state.selected_account, 0);
        state.previous_account(3);
        assert_eq!(state.selected_account, 2);
        state.next_account(3);
        assert_eq!(state.selected_account, 0);
    }

    #[test]
    fn provider_selection_returns_to_first_account() {
        let mut state = AppState::new();
        state.selected_account = 3;
        state.scroll = 8;
        state.select_provider(Some(Provider::Codex));
        assert_eq!(state.provider, Some(Provider::Codex));
        assert_eq!(state.selected_account, 0);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn pointillist_bar_preserves_cell_width() {
        let bar = pointillist_bar(7);
        assert_eq!(bar.chars().count(), 7);
        assert_eq!(bar, "⠂⠄⠂⠄⠂⠄⠂");
    }
}
