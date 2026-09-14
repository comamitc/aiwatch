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
    output::{format_reset, health_text, trend},
};

struct AppState {
    provider: Option<Provider>,
    weekly_only: bool,
    scroll: u16,
}

impl AppState {
    fn new() -> Self {
        Self {
            provider: None,
            weekly_only: false,
            scroll: 0,
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
                        KeyCode::Char('0') => { state.provider = None; state.scroll = 0; }
                        KeyCode::Char('1') => { state.provider = Some(Provider::Claude); state.scroll = 0; }
                        KeyCode::Char('2') => { state.provider = Some(Provider::Codex); state.scroll = 0; }
                        KeyCode::Char('3') => { state.provider = Some(Provider::Grok); state.scroll = 0; }
                        KeyCode::Char('w') => { state.weekly_only = !state.weekly_only; state.scroll = 0; }
                        KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
                            state.scroll = state.scroll.saturating_add(1);
                        }
                        KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
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
    let [header, summary, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(5),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, snapshot, poll, header);
    render_summary(frame, snapshot, summary);
    render_body(frame, snapshot, state, body);
    render_footer(frame, state, footer);
}

fn render_header(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, poll: Duration, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " limitwatch ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            env!("CARGO_PKG_VERSION"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(
                "  │  {} accounts · {} providers  │  provider poll {}s",
                snapshot.accounts.len(),
                snapshot.provider_count(),
                poll.as_secs()
            ),
            Style::default().fg(Color::Gray),
        ),
    ]);
    let clock = Local::now().format("%H:%M:%S").to_string();
    let [left, right] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(12)]).areas(area);
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().bg(Color::Rgb(18, 28, 25)))
            .block(Block::default().borders(Borders::BOTTOM)),
        left,
    );
    frame.render_widget(
        Paragraph::new(format!("{clock} ▌ "))
            .right_aligned()
            .style(
                Style::default()
                    .fg(Color::LightGreen)
                    .bg(Color::Rgb(18, 28, 25)),
            )
            .block(Block::default().borders(Borders::BOTTOM)),
        right,
    );
}

fn render_summary(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, area: Rect) {
    let [accounts, providers, nearest, poll] = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(20),
        Constraint::Percentage(35),
        Constraint::Percentage(25),
    ])
    .areas(area);

    summary_box(
        frame,
        accounts,
        "ACCOUNTS",
        snapshot.accounts.len().to_string(),
        Color::White,
    );
    summary_box(
        frame,
        providers,
        "PROVIDERS",
        snapshot.provider_count().to_string(),
        Color::White,
    );

    let (nearest_value, nearest_color) = snapshot.nearest_limit().map_or_else(
        || ("waiting for quota data".to_string(), Color::DarkGray),
        |(account, window)| {
            (
                format!(
                    "{}/{} {} · {:.1}% left",
                    account.provider,
                    account.name,
                    window.label,
                    window.remaining_percent()
                ),
                usage_color(window.used_percent),
            )
        },
    );
    summary_box(frame, nearest, "NEAREST CAP", nearest_value, nearest_color);

    let ok = snapshot
        .accounts
        .iter()
        .filter(|account| account.health.state == HealthState::Ok)
        .count();
    let age = snapshot
        .accounts
        .iter()
        .map(|account| account.fetched_at)
        .max()
        .map(|time| {
            format!(
                "{} · {ok}/{} ok",
                age_text(Utc::now() - time),
                snapshot.accounts.len()
            )
        })
        .unwrap_or_else(|| "not polled".to_string());
    summary_box(
        frame,
        poll,
        "LAST POLL",
        age,
        if ok == snapshot.accounts.len() {
            Color::LightGreen
        } else {
            Color::Yellow
        },
    );
}

fn summary_box(frame: &mut Frame<'_>, area: Rect, title: &str, value: String, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
        .block(Block::bordered().title(Span::styled(
            format!(" {title} "),
            Style::default().fg(Color::DarkGray),
        )))
        .style(Style::default().bg(Color::Rgb(8, 12, 11))),
        area,
    );
}

fn render_body(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot, state: &AppState, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    for provider in snapshot
        .providers()
        .filter(|provider| state.provider.is_none_or(|filter| filter == *provider))
    {
        let accounts = snapshot
            .accounts
            .iter()
            .filter(|account| account.provider == provider)
            .collect::<Vec<_>>();
        lines.push(provider_line(provider, &accounts));
        for account in accounts {
            lines.extend(account_lines(account, width, state.weekly_only));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching accounts. Configure credential paths or clear the provider filter.",
            Style::default().fg(Color::Yellow),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((state.scroll, 0))
            .block(Block::default().borders(Borders::LEFT | Borders::RIGHT)),
        area,
    );
}

fn provider_line(provider: Provider, accounts: &[&AccountSnapshot]) -> Line<'static> {
    let highest_weekly = accounts
        .iter()
        .flat_map(|account| &account.windows)
        .filter(|window| window.key.contains("weekly") || window.key.contains("monthly"))
        .map(|window| window.used_percent)
        .max_by(f64::total_cmp);
    let nearest = accounts
        .iter()
        .flat_map(|account| account.windows.iter().map(move |window| (*account, window)))
        .max_by(|(_, left), (_, right)| left.used_percent.total_cmp(&right.used_percent));
    let color = nearest.map_or(Color::LightGreen, |(_, window)| {
        usage_color(window.used_percent)
    });
    let mut right = String::new();
    if let Some(weekly) = highest_weekly {
        right.push_str(&format!("highest weekly {weekly:.1}%"));
    }
    if let Some((account, window)) = nearest {
        if !right.is_empty() {
            right.push_str(" · ");
        }
        right.push_str(&format!(
            "nearest {}/{} {:.1}% left",
            account.name,
            window.label,
            window.remaining_percent()
        ));
    }
    Line::from(vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::styled(
            format!(
                "{}  {} account{}",
                provider.label(),
                accounts.len(),
                if accounts.len() == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if right.is_empty() {
                String::new()
            } else {
                format!("  │  {right}")
            },
            Style::default().fg(color),
        ),
    ])
}

fn account_lines(account: &AccountSnapshot, width: usize, weekly_only: bool) -> Vec<Line<'static>> {
    let status_color = match account.health.state {
        HealthState::Ok => Color::LightGreen,
        HealthState::Stale => Color::Yellow,
        HealthState::AuthenticationRequired | HealthState::RateLimited | HealthState::Error => {
            Color::LightRed
        }
    };
    let plan = account
        .plan
        .as_deref()
        .map(|plan| format!("  {plan}"))
        .unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled("  ● ", Style::default().fg(status_color)),
        Span::styled(
            account.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(plan, Style::default().fg(Color::Gray)),
        Span::styled(
            format!(
                "  ·  {}",
                health_text(account.health.state, account.health.status_code)
            ),
            Style::default().fg(status_color),
        ),
    ])];

    let windows = account.windows.iter().filter(|window| {
        !weekly_only || window.key.contains("weekly") || window.key.contains("monthly")
    });
    for window in windows {
        lines.push(window_line(window, width));
        if window.history.iter().any(|value| *value > 0) {
            lines.push(Line::from(vec![
                Span::raw("      7D        "),
                Span::styled(trend(window), Style::default().fg(Color::Green)),
                Span::styled("  local daily peaks", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }
    if account.windows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "      {}",
                account
                    .health
                    .message
                    .as_deref()
                    .unwrap_or("quota unavailable")
            ),
            Style::default().fg(status_color),
        )));
    }
    if !account.details.is_empty() {
        let details = account
            .details
            .iter()
            .map(|detail| format!("{} {}", detail.label, detail.value))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(Span::styled(
            format!("      {details}"),
            Style::default().fg(Color::Gray),
        )));
    }
    lines.push(Line::from(""));
    lines
}

fn window_line(window: &UsageWindow, width: usize) -> Line<'static> {
    let color = usage_color(window.used_percent);
    if width < 68 {
        return Line::from(vec![
            Span::styled(
                format!("      {:<10}", window.label),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                format!("{:>6.1}% used", window.used_percent),
                Style::default().fg(color),
            ),
            Span::styled(
                format!("  {}", format_reset(window.resets_at)),
                Style::default().fg(Color::Gray),
            ),
        ]);
    }

    let bar_width = width.saturating_sub(52).clamp(12, 52);
    let filled = ((window.used_percent / 100.0) * bar_width as f64).round() as usize;
    Line::from(vec![
        Span::styled(
            format!("      {:<10}", window.label),
            Style::default().fg(Color::Gray),
        ),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(bar_width - filled),
            Style::default().fg(Color::Rgb(30, 38, 36)),
        ),
        Span::styled(
            format!("  {:>6.1}% used", window.used_percent),
            Style::default().fg(color),
        ),
        Span::styled(
            format!("  {}", format_reset(window.resets_at)),
            Style::default().fg(Color::Gray),
        ),
    ])
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
            " q quit  r refresh when due  0-3 provider  j/k scroll  w weekly only  │  filter {filter} · {weekly}"
        ))
        .style(Style::default().fg(Color::Gray).bg(Color::Rgb(18, 28, 25))),
        area,
    );
}

fn usage_color(percent: f64) -> Color {
    if percent >= 90.0 {
        Color::LightRed
    } else if percent >= 70.0 {
        Color::Yellow
    } else {
        Color::LightGreen
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
