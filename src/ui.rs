use std::{io, time::Duration};

use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
    dashboard::{
        self, ColumnLayout, OverviewModel, PACE_LEGEND, PaceBand, ProfileFilter, ProviderSection,
        WindowRow,
    },
    model::{AccountSnapshot, DashboardSnapshot, HealthState, Provider, UsageWindow},
    output::{format_reset, health_text},
};

const BG: Color = Color::Rgb(0x15, 0x15, 0x1e);
const FG: Color = Color::Rgb(0xc6, 0xc0, 0xd8);
const SHADE: Color = Color::Rgb(0x1c, 0x1c, 0x28);
const MUTED: Color = Color::Rgb(0x9c, 0xa3, 0xaf);
const CLAUDE: Color = Color::Rgb(0xe8, 0xa0, 0x7c);
const CODEX: Color = Color::Rgb(0x5e, 0xea, 0xd4);
const GROK: Color = Color::Rgb(0x93, 0xc5, 0xfd);
const GREEN: Color = Color::Rgb(0x4a, 0xde, 0x80);
const YELLOW: Color = Color::Rgb(0xea, 0xb3, 0x08);
const ORANGE: Color = Color::Rgb(0xe0, 0x9a, 0x3e);
const RED: Color = Color::Rgb(0xf8, 0x71, 0x71);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMode {
    Summary,
    Focused,
}

struct AppState {
    provider: Option<Provider>,
    profile: ProfileFilter,
    weekly_only: bool,
    view: ViewMode,
    selected_account: usize,
    scroll: u16,
}

impl AppState {
    fn new() -> Self {
        Self {
            provider: None,
            profile: ProfileFilter::All,
            weekly_only: false,
            view: ViewMode::Summary,
            selected_account: 0,
            scroll: 0,
        }
    }

    fn select_provider(&mut self, provider: Option<Provider>) {
        self.provider = provider;
        self.selected_account = 0;
        self.scroll = 0;
    }

    fn cycle_profile(&mut self) {
        self.profile = self.profile.next();
        self.selected_account = 0;
        self.scroll = 0;
    }

    fn toggle_view(&mut self) {
        self.view = match self.view {
            ViewMode::Summary => ViewMode::Focused,
            ViewMode::Focused => ViewMode::Summary,
        };
        self.scroll = 0;
    }

    fn show_summary(&mut self) {
        self.view = ViewMode::Summary;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyAction {
    Continue,
    Quit,
    Refresh,
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
        terminal.draw(|frame| render(frame, &snapshot, &state, poll_interval, Utc::now()))?;

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
                    match handle_key(&mut state, key, &snapshot) {
                        KeyAction::Quit => break,
                        KeyAction::Refresh => { let _ = refresh.try_send(()); }
                        KeyAction::Continue => {}
                    }
                }
            }
        }
    }

    Ok(())
}

fn handle_key(state: &mut AppState, key: KeyEvent, snapshot: &DashboardSnapshot) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::Quit;
    }
    match key.code {
        KeyCode::Char('q') => KeyAction::Quit,
        KeyCode::Char('r') => KeyAction::Refresh,
        KeyCode::Char('0') => {
            state.select_provider(None);
            KeyAction::Continue
        }
        KeyCode::Char('1') => {
            state.select_provider(Some(Provider::Claude));
            KeyAction::Continue
        }
        KeyCode::Char('2') => {
            state.select_provider(Some(Provider::Codex));
            KeyAction::Continue
        }
        KeyCode::Char('3') => {
            state.select_provider(Some(Provider::Grok));
            KeyAction::Continue
        }
        KeyCode::Char('p') => {
            state.cycle_profile();
            KeyAction::Continue
        }
        KeyCode::Char('w') => {
            state.weekly_only = !state.weekly_only;
            state.scroll = 0;
            KeyAction::Continue
        }
        KeyCode::Tab | KeyCode::Char('v') => {
            state.toggle_view();
            KeyAction::Continue
        }
        KeyCode::Esc => {
            state.show_summary();
            KeyAction::Continue
        }
        KeyCode::Char('j') if state.view == ViewMode::Focused => {
            state.next_account(matching_account_count(snapshot, state));
            KeyAction::Continue
        }
        KeyCode::Char('k') if state.view == ViewMode::Focused => {
            state.previous_account(matching_account_count(snapshot, state));
            KeyAction::Continue
        }
        KeyCode::Right if state.view == ViewMode::Focused => {
            state.next_account(matching_account_count(snapshot, state));
            KeyAction::Continue
        }
        KeyCode::Left if state.view == ViewMode::Focused => {
            state.previous_account(matching_account_count(snapshot, state));
            KeyAction::Continue
        }
        KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
            state.scroll = state.scroll.saturating_add(1);
            KeyAction::Continue
        }
        KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
            state.scroll = state.scroll.saturating_sub(1);
            KeyAction::Continue
        }
        _ => KeyAction::Continue,
    }
}

fn render(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    poll: Duration,
    now: DateTime<Utc>,
) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().fg(FG).bg(BG)), area);
    match state.view {
        ViewMode::Summary => render_overview(frame, snapshot, state, poll, now, area),
        ViewMode::Focused => {
            let [header, body, footer] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(6),
                Constraint::Length(1),
            ])
            .areas(area);
            let model = overview_model(snapshot, state, poll, now, area.width as usize);
            render_header(frame, &model, header);
            render_focused_body(frame, snapshot, state, now, body);
            render_footer(frame, state, footer);
        }
    }
}

fn overview_model<'a>(
    snapshot: &'a DashboardSnapshot,
    state: &AppState,
    poll: Duration,
    now: DateTime<Utc>,
    width: usize,
) -> OverviewModel<'a> {
    dashboard::build_overview(
        snapshot,
        width,
        poll,
        state.provider,
        state.profile,
        state.weekly_only,
        now,
    )
}

fn render_overview(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    poll: Duration,
    now: DateTime<Utc>,
    area: Rect,
) {
    let [header, columns, body, legend, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let model = overview_model(snapshot, state, poll, now, area.width as usize);
    render_header(frame, &model, header);
    render_column_headers(frame, model.layout, columns);
    render_overview_body(frame, &model, state.scroll, body);
    frame.render_widget(
        Paragraph::new(PACE_LEGEND).style(Style::default().fg(MUTED).bg(BG)),
        legend,
    );
    render_footer(frame, state, footer);
}

fn render_header(frame: &mut Frame<'_>, model: &OverviewModel<'_>, area: Rect) {
    let [top, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(model.header_line1.clone()).style(Style::default().fg(FG).bg(BG)),
        top,
    );
    frame.render_widget(
        Paragraph::new(model.header_line2.clone()).style(Style::default().fg(MUTED).bg(BG)),
        bottom,
    );
}

fn render_column_headers(frame: &mut Frame<'_>, layout: ColumnLayout, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(column_header_spans(layout)))
            .style(Style::default().fg(MUTED).bg(SHADE)),
        area,
    );
}

fn column_header_spans(layout: ColumnLayout) -> Vec<Span<'static>> {
    let mut spans = vec![
        cell_span("", layout.accent, MUTED, SHADE),
        cell_span("ACCOUNT", layout.account, MUTED, SHADE),
        Span::styled(" ", Style::default().bg(SHADE)),
        cell_span("WINDOW", layout.window, MUTED, SHADE),
        Span::styled(" ", Style::default().bg(SHADE)),
        cell_span("USAGE", layout.usage, MUTED, SHADE),
        Span::styled(" ", Style::default().bg(SHADE)),
        cell_span(layout.used_cap_header(), layout.used_cap, MUTED, SHADE),
        Span::styled(" ", Style::default().bg(SHADE)),
        cell_span("RESETS", layout.resets, MUTED, SHADE),
    ];
    if layout.show_spark {
        spans.push(Span::styled(" ", Style::default().bg(SHADE)));
        spans.push(cell_span("7D", layout.spark, MUTED, SHADE));
    }
    spans
}

fn render_overview_body(frame: &mut Frame<'_>, model: &OverviewModel<'_>, scroll: u16, area: Rect) {
    let mut lines = Vec::new();
    if model.sections.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching accounts. Configure credentials or clear the provider filter.",
            Style::default().fg(YELLOW),
        )));
    }
    for (section_index, section) in model.sections.iter().enumerate() {
        if section_index > 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(""));
        }
        lines.push(provider_header_line(
            section,
            model.layout,
            area.width as usize,
        ));
        for (row_index, row) in section.rows.iter().enumerate() {
            let blanks = if row_index > 0 && row.first_for_account {
                2
            } else {
                1
            };
            for _ in 0..blanks {
                lines.push(provider_accent_blank(section.provider));
            }
            lines.push(window_row_line(row, model.layout, section.provider));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(FG).bg(BG))
            .scroll((scroll, 0)),
        area,
    );
}

fn provider_accent_blank(provider: Provider) -> Line<'static> {
    Line::from(Span::styled(
        "▎",
        Style::default().fg(provider_accent(provider)).bg(BG),
    ))
}

fn provider_header_line(
    section: &ProviderSection<'_>,
    layout: ColumnLayout,
    width: usize,
) -> Line<'static> {
    let accent = provider_accent(section.provider);
    let left = dashboard::provider_header_text(section);
    let details = section.details.clone();
    let mut spans = vec![Span::styled("▎", Style::default().fg(accent).bg(SHADE))];
    let remaining = width.saturating_sub(layout.accent);
    if details.is_empty() {
        spans.push(Span::styled(
            dashboard::pad_cell(&left, remaining),
            Style::default()
                .fg(FG)
                .bg(SHADE)
                .add_modifier(Modifier::BOLD),
        ));
        return Line::from(spans);
    }
    let left_width = left.chars().count();
    let details_width = details.chars().count();
    let gap = remaining.saturating_sub(left_width + details_width).max(1);
    spans.push(Span::styled(
        left,
        Style::default()
            .fg(FG)
            .bg(SHADE)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(SHADE)));
    spans.push(Span::styled(details, Style::default().fg(MUTED).bg(SHADE)));
    Line::from(spans)
}

fn window_row_line(row: &WindowRow<'_>, layout: ColumnLayout, provider: Provider) -> Line<'static> {
    let band = row
        .used_percent
        .map(|used| dashboard::pace_band(used, row.pace))
        .unwrap_or(PaceBand::Gray);
    let color = band_color(band);
    let bar = usage_bar_spans(
        row.used_percent.unwrap_or(0.0),
        layout.usage,
        row.pace,
        color,
    );
    let mut spans = vec![
        Span::styled("▎", Style::default().fg(provider_accent(provider)).bg(BG)),
        Span::styled(row.account_cell.clone(), Style::default().fg(FG).bg(BG)),
        Span::raw(" "),
        Span::styled(row.window_cell.clone(), Style::default().fg(MUTED).bg(BG)),
        Span::raw(" "),
    ];
    spans.extend(bar);
    spans.extend([
        Span::raw(" "),
        Span::styled(row.used_cap_cell.clone(), Style::default().fg(color).bg(BG)),
        Span::raw(" "),
        Span::styled(row.resets_cell.clone(), Style::default().fg(MUTED).bg(BG)),
    ]);
    if layout.show_spark {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            row.spark_cell.clone(),
            Style::default().fg(MUTED).bg(BG),
        ));
    }
    Line::from(spans)
}

fn usage_bar_spans(
    used_percent: f64,
    width: usize,
    pace: Option<f64>,
    fill: Color,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let filled = ((used_percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let marker = dashboard::pace_column(pace, width);
    let mut spans = Vec::new();
    let mut index = 0;
    while index < width {
        if marker == Some(index) {
            let marker_bg = if index < filled { fill } else { MUTED };
            spans.push(Span::styled(
                "│",
                Style::default()
                    .fg(FG)
                    .bg(marker_bg)
                    .add_modifier(Modifier::BOLD),
            ));
            index += 1;
            continue;
        }
        let filled_run = index < filled;
        let start = index;
        while index < width && marker != Some(index) && (index < filled) == filled_run {
            index += 1;
        }
        let run = index - start;
        if filled_run {
            spans.push(Span::styled(
                "█".repeat(run),
                Style::default().fg(fill).bg(BG),
            ));
        } else {
            spans.push(Span::styled(
                "░".repeat(run),
                Style::default().fg(MUTED).bg(BG),
            ));
        }
    }
    spans
}

fn cell_span(text: &str, width: usize, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(
        dashboard::pad_cell(text, width),
        Style::default().fg(fg).bg(bg),
    )
}

fn provider_accent(provider: Provider) -> Color {
    match provider {
        Provider::Claude => CLAUDE,
        Provider::Codex => CODEX,
        Provider::Grok => GROK,
    }
}

fn band_color(band: PaceBand) -> Color {
    match band {
        PaceBand::Green => GREEN,
        PaceBand::Yellow => YELLOW,
        PaceBand::Orange => ORANGE,
        PaceBand::Red => RED,
        PaceBand::Gray => MUTED,
    }
}

fn render_focused_body(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    now: DateTime<Utc>,
    area: Rect,
) {
    let Some((index, count, account)) = selected_account(snapshot, state) else {
        frame.render_widget(
            Paragraph::new(
                "No matching accounts. Configure credentials or clear the provider filter.",
            )
            .style(Style::default().fg(YELLOW).bg(BG)),
            area,
        );
        return;
    };

    let content_width = area.width.saturating_sub(4) as usize;
    let lines = account_card_lines(account, index, count, content_width, state.weekly_only, now);
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", account.provider.label()),
            Style::default()
                .fg(provider_accent(account.provider))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("/ {} ", account.name),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(FG).bg(BG))
            .scroll((state.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(MUTED))
                    .title(title)
                    .padding(ratatui::widgets::Padding::horizontal(2)),
            ),
        area,
    );
}

fn matching_account_count(snapshot: &DashboardSnapshot, state: &AppState) -> usize {
    dashboard::visible_accounts(snapshot, state.provider, state.profile).len()
}

fn selected_account<'a>(
    snapshot: &'a DashboardSnapshot,
    state: &AppState,
) -> Option<(usize, usize, &'a AccountSnapshot)> {
    let accounts = dashboard::visible_accounts(snapshot, state.provider, state.profile);
    if accounts.is_empty() {
        return None;
    }
    let index = state.selected_account.min(accounts.len() - 1);
    accounts
        .get(index)
        .copied()
        .map(|account| (index, accounts.len(), account))
}

fn account_card_lines(
    account: &AccountSnapshot,
    index: usize,
    count: usize,
    width: usize,
    weekly_only: bool,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let status_color = health_color(account.health.state);
    let meta = format!(
        "Updated {} · {}",
        dashboard::age_text(now - account.fetched_at),
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
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        divider_line(width),
        Line::from(""),
    ];

    let mut rendered_window = false;
    for window in account
        .windows
        .iter()
        .filter(|window| dashboard::window_is_weekly_view(window, weekly_only))
    {
        rendered_window = true;
        lines.extend(window_section(window, width, now));
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
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.extend(detail_grid_lines(account, width));
    }
    lines
}

fn window_section(window: &UsageWindow, width: usize, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let pace = window.pace_used_percent(now);
    let color = band_color(dashboard::pace_band(window.used_percent, pace));
    let bar_width = width.saturating_sub(20).clamp(4, 80);
    let mut bar = vec![Span::styled(
        "usage ",
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    )];
    bar.extend(usage_bar_spans(window.used_percent, bar_width, pace, color));
    bar.push(Span::styled(
        format!(" {:>5.1}% used", window.used_percent),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    let mut lines = vec![
        aligned_line(
            window.label.clone(),
            format_reset(window.resets_at),
            width,
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
            Style::default().fg(MUTED),
        ),
        Line::from(bar),
    ];

    if window.history.iter().any(|value| *value > 0) {
        let peak = window.history.iter().copied().max().unwrap_or_default();
        lines.push(aligned_line(
            "7D PEAKS".to_string(),
            format!("peak {peak}%"),
            width,
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
            Style::default().fg(MUTED),
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
        lines.push(Line::from(Span::styled(chart, Style::default().fg(MUTED))));
    }
    lines.push(Line::from(Span::styled(
        "─".repeat(chart_width),
        Style::default().fg(MUTED),
    )));
    lines.push(aligned_line(
        "oldest".to_string(),
        "today".to_string(),
        chart_width,
        Style::default().fg(MUTED),
        Style::default().fg(MUTED),
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
            Style::default().fg(MUTED),
        ));
        let right_value = row.get(1).map_or("", |detail| detail.value.as_str());
        lines.push(two_column_line(
            &row[0].value,
            right_value,
            column_width,
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
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
    Line::from(Span::styled("─".repeat(width), Style::default().fg(MUTED)))
}

fn health_color(state: HealthState) -> Color {
    match state {
        HealthState::Ok => GREEN,
        HealthState::Stale => YELLOW,
        HealthState::AuthenticationRequired | HealthState::RateLimited | HealthState::Error => RED,
    }
}

fn render_footer(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let filter = state.provider.map_or("all", Provider::key);
    let weekly = if state.weekly_only {
        "weekly"
    } else {
        "all windows"
    };
    let controls = match state.view {
        ViewMode::Summary => {
            " q quit  r refresh  j/k or ↑/↓ scroll  tab focused  0-3 provider  w weekly  p profile "
        }
        ViewMode::Focused => {
            " q quit  r refresh  j/k or ←/→ account  ↑/↓ scroll  tab summary  0-3 provider  w weekly  p profile "
        }
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{controls} │  {filter} · {} · {weekly}",
            state.profile.label()
        ))
        .style(Style::default().fg(MUTED).bg(BG)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        demo,
        model::{FetchHealth, UsageWindow},
    };
    use chrono::Duration as ChronoDuration;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    fn buffer_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| buffer_line(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw_overview(width: u16, height: u16, state: &AppState) -> Buffer {
        let snapshot = demo::snapshot();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &snapshot, state, Duration::from_secs(60), Utc::now()))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    fn draw_snapshot(
        snapshot: &DashboardSnapshot,
        state: &AppState,
        width: u16,
        height: u16,
        now: DateTime<Utc>,
    ) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, snapshot, state, Duration::from_secs(300), now))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    fn is_accent_gap(line: &str) -> bool {
        let trimmed = line.trim_end();
        !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '▎' || ch == ' ')
    }

    fn usage_marker_cells(buffer: &Buffer) -> Vec<(u16, u16)> {
        (0..buffer.area.height)
            .flat_map(|y| {
                (0..buffer.area.width)
                    .filter(|&x| buffer[(x, y)].symbol() == "│" && buffer[(x, y)].bg != BG)
                    .map(|x| (x, y))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn paced_account_snapshot(
        used: f64,
        remaining: ChronoDuration,
        now: DateTime<Utc>,
    ) -> DashboardSnapshot {
        let mut snapshot = DashboardSnapshot {
            generated_at: now,
            accounts: Vec::new(),
        };
        let mut account =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        account.windows.push(UsageWindow::new(
            "five_hour",
            "5H",
            used,
            Some(now + remaining),
        ));
        snapshot.accounts.push(account);
        snapshot
    }


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
    fn existing_keys_keep_working_and_z_is_ignored() {
        let snapshot = demo::snapshot();
        let mut state = AppState::new();
        assert_eq!(
            handle_key(&mut state, press(KeyCode::Char('q')), &snapshot),
            KeyAction::Quit
        );
        assert_eq!(
            handle_key(&mut state, press(KeyCode::Char('r')), &snapshot),
            KeyAction::Refresh
        );
        handle_key(&mut state, press(KeyCode::Char('1')), &snapshot);
        assert_eq!(state.provider, Some(Provider::Claude));
        handle_key(&mut state, press(KeyCode::Char('2')), &snapshot);
        assert_eq!(state.provider, Some(Provider::Codex));
        handle_key(&mut state, press(KeyCode::Char('3')), &snapshot);
        assert_eq!(state.provider, Some(Provider::Grok));
        handle_key(&mut state, press(KeyCode::Char('0')), &snapshot);
        assert_eq!(state.provider, None);
        handle_key(&mut state, press(KeyCode::Char('w')), &snapshot);
        assert!(state.weekly_only);
        handle_key(&mut state, press(KeyCode::Tab), &snapshot);
        assert_eq!(state.view, ViewMode::Focused);
        handle_key(&mut state, press(KeyCode::Char('v')), &snapshot);
        assert_eq!(state.view, ViewMode::Summary);
        handle_key(&mut state, press(KeyCode::Char('j')), &snapshot);
        assert_eq!(state.scroll, 1);
        handle_key(&mut state, press(KeyCode::Char('k')), &snapshot);
        assert_eq!(state.scroll, 0);
        handle_key(&mut state, press(KeyCode::Down), &snapshot);
        handle_key(&mut state, press(KeyCode::Up), &snapshot);
        handle_key(&mut state, press(KeyCode::Char('z')), &snapshot);
        assert_eq!(state.view, ViewMode::Summary);
        assert_eq!(state.profile, ProfileFilter::All);
    }

    #[test]
    fn profile_key_cycles_all_personal_work_and_keeps_unclassifiable_on_all() {
        let mut snapshot = demo::snapshot();
        snapshot.accounts.push(AccountSnapshot::empty(
            "claude:other",
            "other",
            Provider::Claude,
            FetchHealth::ok(),
        ));
        let mut state = AppState::new();
        assert_eq!(
            dashboard::visible_accounts(&snapshot, None, ProfileFilter::All)
                .iter()
                .filter(|account| account.name == "other")
                .count(),
            1
        );
        handle_key(&mut state, press(KeyCode::Char('p')), &snapshot);
        assert_eq!(state.profile, ProfileFilter::Personal);
        assert!(
            dashboard::visible_accounts(&snapshot, None, state.profile)
                .iter()
                .all(|account| dashboard::name_has_token(&account.name, "personal"))
        );
        handle_key(&mut state, press(KeyCode::Char('p')), &snapshot);
        assert_eq!(state.profile, ProfileFilter::Work);
        handle_key(&mut state, press(KeyCode::Char('p')), &snapshot);
        assert_eq!(state.profile, ProfileFilter::All);
        assert!(
            dashboard::visible_accounts(&snapshot, None, ProfileFilter::All)
                .iter()
                .any(|account| account.name == "other")
        );
    }

    #[test]
    fn overview_160x30_is_compact_table_with_theme_and_legend() {
        let buffer = draw_overview(160, 30, &AppState::new());
        let text = buffer_text(&buffer);
        let line0 = buffer_line(&buffer, 0);
        let line1 = buffer_line(&buffer, 1);
        let columns = buffer_line(&buffer, 2);
        assert!(line0.contains("aiwatch"));
        assert!(line0.contains(env!("CARGO_PKG_VERSION")));
        assert!(line0.contains("profile all"));
        assert!(line1.contains("poll 60s"));
        assert!(line1.contains("nearest cap"));
        assert!(line1.contains("freshness"));
        assert!(line1.contains("health"));
        assert!(columns.contains("ACCOUNT"));
        assert!(columns.contains("WINDOW"));
        assert!(columns.contains("USAGE"));
        assert!(columns.contains("% USED / CAP"));
        assert!(columns.contains("RESETS"));
        assert!(columns.contains("7D"));
        assert!(text.contains("CLAUDE"));
        assert!(text.contains("CODEX"));
        assert!(text.contains("GROK"));
        assert!(text.contains("pace:"));
        assert!(text.contains("on-pace marker"));
        assert!(!text.contains("7D PEAK"));
        assert!(!text.contains("local daily peaks"));
        assert!(!text.contains("NEAREST CAP"));
        let footer = buffer_line(&buffer, 29);
        assert!(footer.contains("p profile"));
        assert!(!footer.contains("z "));
        assert!(!footer.contains("zoom"));
        assert_eq!(buffer[(0, 0)].bg, BG);
        assert_eq!(buffer[(8, 0)].fg, FG);
        let claude_y = (0..buffer.area.height)
            .find(|y| buffer_line(&buffer, *y).contains("CLAUDE"))
            .expect("claude section");
        assert_eq!(buffer[(0, claude_y)].fg, CLAUDE);
        assert_eq!(buffer[(0, claude_y)].bg, SHADE);
    }

    #[test]
    fn overview_narrow_width_drops_spark_then_cap() {
        let mid = buffer_text(&draw_overview(140, 24, &AppState::new()));
        let mid_header = mid.lines().nth(2).expect("columns");
        assert!(mid_header.contains("ACCOUNT"));
        assert!(mid_header.contains("WINDOW"));
        assert!(mid_header.contains("% USED / CAP"));
        assert!(mid_header.contains("RESETS"));
        assert!(!mid_header.contains("7D"));

        let narrow = buffer_text(&draw_overview(110, 24, &AppState::new()));
        let narrow_header = narrow.lines().nth(2).expect("columns");
        assert!(narrow_header.contains("% USED"));
        assert!(!narrow_header.contains("CAP"));
        assert!(!narrow_header.contains("7D"));
        assert!(narrow_header.contains("RESETS"));
    }

    #[test]
    fn orange_band_uses_distinct_rgb_and_marker_is_present() {
        let now = Utc::now();
        let mut snapshot = DashboardSnapshot {
            generated_at: now,
            accounts: Vec::new(),
        };
        let mut account =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        let remaining = ChronoDuration::hours(2) + ChronoDuration::minutes(30);
        account.windows.push(UsageWindow::new(
            "five_hour",
            "5H",
            65.0,
            Some(now + remaining),
        ));
        snapshot.accounts.push(account);
        let backend = TestBackend::new(160, 16);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &snapshot,
                    &AppState::new(),
                    Duration::from_secs(300),
                    now,
                )
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains('│'));
        let mut saw_orange = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].fg == ORANGE {
                    saw_orange = true;
                }
                assert_ne!(buffer[(x, y)].fg, YELLOW);
            }
        }
        assert!(saw_orange);
        let window = &snapshot.accounts[0].windows[0];
        assert_eq!(
            dashboard::pace_band(window.used_percent, window.pace_used_percent(now)),
            PaceBand::Orange
        );
    }

    #[test]
    fn footer_and_header_show_profile_filter() {
        let mut state = AppState::new();
        state.profile = ProfileFilter::Work;
        let buffer = draw_overview(160, 30, &state);
        assert!(buffer_line(&buffer, 0).contains("profile work"));
        assert!(buffer_line(&buffer, 29).contains("work"));
        assert!(!buffer_text(&buffer).contains("zoom"));
    }

    #[test]
    fn pace_marker_keeps_filled_and_empty_bar_background() {
        let fill = GREEN;
        let filled = usage_bar_spans(80.0, 10, Some(20.0), fill);
        let marker = filled
            .iter()
            .find(|span| span.content.as_ref() == "│")
            .expect("filled marker");
        assert_eq!(marker.style.fg, Some(FG));
        assert_eq!(marker.style.bg, Some(fill));
        assert_ne!(marker.style.bg, Some(BG));

        let empty = usage_bar_spans(20.0, 10, Some(80.0), fill);
        let marker = empty
            .iter()
            .find(|span| span.content.as_ref() == "│")
            .expect("empty marker");
        assert_eq!(marker.style.fg, Some(FG));
        assert_eq!(marker.style.bg, Some(MUTED));
        assert_ne!(marker.style.bg, Some(BG));
    }

    #[test]
    fn overview_and_focused_markers_keep_bar_background() {
        let now = Utc::now();
        let remaining = ChronoDuration::hours(2) + ChronoDuration::minutes(30);
        let mut focused = AppState::new();
        focused.view = ViewMode::Focused;

        let filled_snapshot = paced_account_snapshot(65.0, remaining, now);
        for state in [&AppState::new(), &focused] {
            let buffer = draw_snapshot(&filled_snapshot, state, 160, 24, now);
            let cells = usage_marker_cells(&buffer);
            assert!(!cells.is_empty(), "expected pace marker");
            for (x, y) in cells {
                assert_eq!(buffer[(x, y)].fg, FG);
                assert_eq!(buffer[(x, y)].bg, ORANGE);
                assert_ne!(buffer[(x, y)].bg, BG);
            }
        }

        let empty_snapshot = paced_account_snapshot(20.0, remaining, now);
        for state in [&AppState::new(), &focused] {
            let buffer = draw_snapshot(&empty_snapshot, state, 160, 24, now);
            let cells = usage_marker_cells(&buffer);
            assert!(!cells.is_empty(), "expected pace marker");
            for (x, y) in cells {
                assert_eq!(buffer[(x, y)].fg, FG);
                assert_eq!(buffer[(x, y)].bg, MUTED);
                assert_ne!(buffer[(x, y)].bg, BG);
            }
        }
    }

    #[test]
    fn overview_spaces_rows_accounts_and_providers() {
        let buffer = draw_overview(160, 48, &AppState::new());
        let lines = (0..buffer.area.height)
            .map(|y| buffer_line(&buffer, y).trim_end().to_string())
            .collect::<Vec<_>>();
        let claude = lines
            .iter()
            .position(|line| line.contains("CLAUDE"))
            .expect("claude");
        let codex = lines
            .iter()
            .position(|line| line.contains("CODEX"))
            .expect("codex");
        let grok = lines
            .iter()
            .position(|line| line.contains("GROK"))
            .expect("grok");
        assert!(claude < codex && codex < grok);
        assert!(lines[codex - 1].is_empty());
        assert!(lines[codex - 2].is_empty());
        assert!(lines[grok - 1].is_empty());
        assert!(lines[grok - 2].is_empty());
        assert!(is_accent_gap(&lines[claude + 1]));

        let claude_rows = ((claude + 1)..codex)
            .filter(|&index| lines[index].contains("5H") || lines[index].contains("WEEKLY"))
            .collect::<Vec<_>>();
        assert_eq!(claude_rows, [claude + 2, claude + 4, claude + 7, claude + 9]);
        assert!(is_accent_gap(&lines[claude_rows[0] + 1]));
        assert!(is_accent_gap(&lines[claude_rows[1] + 1]));
        assert!(is_accent_gap(&lines[claude_rows[1] + 2]));
    }

    #[test]
    fn overview_scrolls_expanded_layout() {
        let mut state = AppState::new();
        let top = draw_overview(160, 16, &state);
        let top_text = buffer_text(&top);
        assert!(top_text.contains("CLAUDE"));
        assert!(!top_text.contains("GROK"));

        state.scroll = 18;
        let scrolled = draw_overview(160, 16, &state);
        let scrolled_text = buffer_text(&scrolled);
        assert!(buffer_line(&scrolled, 0).contains("aiwatch"));
        assert!(scrolled_text.contains("GROK") || scrolled_text.contains("CODEX"));
        assert!(!scrolled_text.contains("CLAUDE") || scrolled_text.contains("CODEX"));
    }

}
