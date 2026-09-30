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
    widgets::{Block, Paragraph},
};
use tokio::sync::{mpsc, watch};

use crate::{
    dashboard::{self, MeterTone, ProfileFilter},
    model::{AccountSnapshot, DashboardSnapshot, HealthState, Provider},
};

const BG: Color = Color::Reset;
const FG: Color = Color::Rgb(0xc5, 0xcd, 0xd8);
const DIM: Color = Color::Rgb(0x5c, 0x65, 0x78);
const GOLD: Color = Color::Rgb(0xd4, 0xc0, 0x78);
const GOLD_FILL: Color = Color::Rgb(0xc9, 0xb1, 0x5c);
const MINT: Color = Color::Rgb(0x6f, 0xcb, 0x9f);
const CYAN: Color = Color::Rgb(0x4e, 0xcd, 0xc4);
const MUTED: Color = Color::Rgb(0x7a, 0x84, 0x96);
const META: Color = Color::Rgb(0x8b, 0x93, 0xa7);
const CLAUDE: Color = Color::Rgb(0xe8, 0xa0, 0x7c);
const CODEX: Color = Color::Rgb(0x5e, 0xea, 0xd4);
const GROK: Color = Color::Rgb(0x93, 0xc5, 0xfd);
const YELLOW: Color = Color::Rgb(0xea, 0xb3, 0x08);
const RED: Color = Color::Rgb(0xf8, 0x71, 0x71);

/// Terminals at least this wide show two account cards per row.
const TWO_COLUMN_MIN_WIDTH: usize = 100;
const COLUMN_GAP: usize = 3;
/// Blank columns between a card's side borders and its content.
const CARD_PAD_X: usize = 2;

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
    frame.render_widget(Block::default().style(Style::default().fg(FG)), area);
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(area);
    render_status(frame, snapshot, state, poll, header);
    match state.view {
        ViewMode::Summary => render_overview_body(frame, snapshot, state, now, body),
        ViewMode::Focused => render_focused_body(frame, snapshot, state, now, body),
    }
    render_footer(frame, state, footer);
}

fn render_status(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    poll: Duration,
    area: Rect,
) {
    let count = dashboard::visible_accounts(snapshot, state.provider, state.profile).len();
    frame.render_widget(
        Paragraph::new(dashboard::status_line(count, state.profile, poll))
            .style(Style::default().fg(MUTED).bg(BG)),
        area,
    );
}

fn render_overview_body(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    now: DateTime<Utc>,
    area: Rect,
) {
    let cards = dashboard::account_cards(
        snapshot,
        state.provider,
        state.profile,
        state.weekly_only,
        now,
    );
    let width = area.width as usize;
    let (columns, card_width) = if width >= TWO_COLUMN_MIN_WIDTH {
        (2, (width - COLUMN_GAP) / 2)
    } else {
        (1, width)
    };
    let mut lines = Vec::new();
    if cards.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching accounts. Configure credentials or clear the provider filter.",
            Style::default().fg(YELLOW).bg(BG),
        )));
    }
    let groups = dashboard::card_groups(&cards);
    if groups.is_empty() {
        push_card_rows(&mut lines, &cards, columns, card_width);
    }
    for (index, (group, run)) in groups.into_iter().enumerate() {
        if index > 0 {
            lines.push(blank_line());
            lines.push(blank_line());
        }
        let row_width = columns * card_width + (columns - 1) * COLUMN_GAP;
        lines.push(group_heading(group, run.len(), row_width));
        lines.push(blank_line());
        push_card_rows(&mut lines, run, columns, card_width);
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(FG).bg(BG))
            .scroll((state.scroll, 0)),
        area,
    );
}

fn push_card_rows(
    lines: &mut Vec<Line<'static>>,
    cards: &[dashboard::AccountCard<'_>],
    columns: usize,
    card_width: usize,
) {
    for (index, row) in cards.chunks(columns).enumerate() {
        if index > 0 {
            lines.push(blank_line());
        }
        lines.extend(card_row_lines(row, card_width));
    }
}

/// A full-width rule naming a personal/work group: `personal  ·  3 accounts ─────────`.
fn group_heading(group: dashboard::ProfileGroup, count: usize, width: usize) -> Line<'static> {
    let name = group.label().to_string();
    let detail = format!("  ·  {} ", dashboard::account_count_label(count));
    let used = name.chars().count() + detail.chars().count();
    let spans = vec![
        Span::styled(name, Style::default().fg(FG).add_modifier(Modifier::BOLD)),
        Span::styled(detail, Style::default().fg(MUTED)),
        Span::styled(
            "─".repeat(width.saturating_sub(used)),
            Style::default().fg(DIM),
        ),
    ];
    Line::from(fit_spans(spans, width))
}

/// Joins cards side by side, stretching shorter cards so every border in the row lines up.
fn card_row_lines(row: &[dashboard::AccountCard<'_>], card_width: usize) -> Vec<Line<'static>> {
    let mut cards = row
        .iter()
        .map(|card| card_lines(card, card_width))
        .collect::<Vec<_>>();
    let height = cards.iter().map(Vec::len).max().unwrap_or(0);
    for lines in &mut cards {
        while lines.len() < height {
            lines.insert(lines.len() - 1, side_frame(blank_line(), card_width));
        }
    }
    (0..height)
        .map(|y| {
            let mut spans = Vec::new();
            for (column, lines) in cards.iter_mut().enumerate() {
                if column > 0 {
                    spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                }
                spans.append(&mut lines[y].spans);
            }
            Line::from(spans)
        })
        .collect()
}

fn card_lines(card: &dashboard::AccountCard<'_>, width: usize) -> Vec<Line<'static>> {
    let inner = content_width(width);
    let mut body = vec![blank_line()];
    if card.meters.is_empty() {
        if let Some(notice) = &card.notice {
            body.push(Line::from(Span::styled(
                notice.clone(),
                Style::default().fg(health_color(card.account.health.state)),
            )));
            body.push(blank_line());
        }
    } else {
        for meter in &card.meters {
            body.push(meter_line(meter, inner));
            body.push(blank_line());
        }
    }
    let mut lines = vec![top_border(card, width)];
    for line in body {
        lines.push(side_frame(line, width));
    }
    lines.push(bottom_border(card.empty_in.as_deref(), width));
    lines
}

fn top_border(card: &dashboard::AccountCard<'_>, width: usize) -> Line<'static> {
    let accent = if card.account.health.state == HealthState::Ok {
        provider_accent(card.account.provider)
    } else {
        health_color(card.account.health.state)
    };
    // The leading dashes line the title's dot up with the padded content below it.
    let lead = "─".repeat(CARD_PAD_X - 1);
    let inner = width.saturating_sub(2 + lead.chars().count());
    let name = card
        .account_name
        .as_deref()
        .map(|name| format!(" {name}"))
        .unwrap_or_default();
    let left = format!(" ● {}{name} ", card.title);
    let right = match &card.plan {
        Some(plan) => format!(" {plan} · {} ", card.auth),
        None => format!(" {} ", card.auth),
    };
    let right_width = right.chars().count();
    let mut used = left.chars().count();
    let mut title = vec![Span::styled(left, Style::default().fg(accent))];
    // The email sits between the name and the plan, keeping at least one dash before the plan.
    let email_room = inner.saturating_sub(used + right_width + 1);
    if let Some(email) = card
        .email
        .as_deref()
        .and_then(|email| fit_email(email, email_room.saturating_sub(3)))
    {
        used += email.chars().count() + 3;
        title.push(Span::styled("· ", Style::default().fg(DIM)));
        title.push(Span::styled(email, Style::default().fg(FG)));
        title.push(Span::raw(" "));
    }
    title.push(Span::styled(
        "─".repeat(inner.saturating_sub(used + right_width)),
        Style::default().fg(DIM),
    ));
    title.push(Span::styled(right, Style::default().fg(META)));
    let mut spans = vec![Span::styled(format!("╭{lead}"), Style::default().fg(DIM))];
    spans.extend(fit_spans(title, inner));
    spans.push(Span::styled("╮", Style::default().fg(DIM)));
    Line::from(spans)
}

/// Shortens an email to `width` columns, or drops it when too little of it would remain to
/// recognise the account.
fn fit_email(email: &str, width: usize) -> Option<String> {
    const MIN_WIDTH: usize = 6;
    let length = email.chars().count();
    if length <= width {
        Some(email.to_string())
    } else if width >= MIN_WIDTH {
        let mut short = email.chars().take(width - 1).collect::<String>();
        short.push('…');
        Some(short)
    } else {
        None
    }
}

/// Cuts spans to `width` columns, ending in an ellipsis, so side-by-side cards never overlap.
fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    if total <= width {
        return spans;
    }
    let mut remaining = width.saturating_sub(1);
    let mut fitted = Vec::new();
    let mut style = Style::default();
    for span in spans {
        if remaining == 0 {
            break;
        }
        style = span.style;
        let length = span.content.chars().count();
        if length <= remaining {
            remaining -= length;
            fitted.push(span);
        } else {
            let content = span.content.chars().take(remaining).collect::<String>();
            fitted.push(Span::styled(content, style));
            remaining = 0;
        }
    }
    if width > 0 {
        fitted.push(Span::styled("…", style));
    }
    fitted
}

/// Closes a card, naming on its right edge how soon the account runs dry when it will.
fn bottom_border(empty_in: Option<&str>, width: usize) -> Line<'static> {
    let inner = width.saturating_sub(2);
    let tail = "─".repeat(CARD_PAD_X - 1);
    let label = empty_in
        .map(|empty_in| format!(" empty in {empty_in} "))
        .filter(|label| label.chars().count() + tail.chars().count() < inner)
        .unwrap_or_default();
    let dashes = inner.saturating_sub(label.chars().count() + tail.chars().count());
    let mut spans = vec![Span::styled(
        format!("╰{}", "─".repeat(dashes)),
        Style::default().fg(DIM),
    )];
    if !label.is_empty() {
        spans.push(Span::styled(label, Style::default().fg(GOLD)));
    }
    spans.push(Span::styled(format!("{tail}╯"), Style::default().fg(DIM)));
    Line::from(spans)
}

/// Columns left for content once a card of `width` gives up its borders and padding.
fn content_width(width: usize) -> usize {
    width.saturating_sub(2 + 2 * CARD_PAD_X)
}

fn side_frame(line: Line<'static>, width: usize) -> Line<'static> {
    let inner = content_width(width);
    let content = fit_spans(line.spans, inner);
    let used = content
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let pad = " ".repeat(CARD_PAD_X);
    let mut spans = vec![
        Span::styled("│", Style::default().fg(DIM)),
        Span::raw(pad.clone()),
    ];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
    spans.push(Span::raw(pad));
    spans.push(Span::styled("│", Style::default().fg(DIM)));
    Line::from(spans)
}

fn meter_line(meter: &dashboard::CardMeter, width: usize) -> Line<'static> {
    let columns = dashboard::meter_columns(width);
    let fill = tone_color(meter.tone);
    let mut spans = vec![Span::styled(
        dashboard::pad_cell(&meter.label, columns.label),
        Style::default().fg(MUTED),
    )];
    if columns.bar > 0 {
        spans.push(Span::raw(" "));
        spans.extend(rail_spans(
            meter.used_percent,
            meter.pace,
            columns.bar,
            fill,
        ));
    }
    if columns.percent > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            dashboard::pad_cell(
                &dashboard::percent_label(meter.used_percent),
                columns.percent,
            ),
            Style::default().fg(fill),
        ));
    }
    if columns.time > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            align_right(&meter.reset, columns.time),
            Style::default().fg(GOLD),
        ));
    }
    Line::from(spans)
}

fn blank_line() -> Line<'static> {
    Line::from("")
}

fn align_right(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() >= width {
        return chars.into_iter().take(width).collect();
    }
    let mut line = " ".repeat(width - chars.len());
    line.extend(chars);
    line
}

fn tone_color(tone: MeterTone) -> Color {
    match tone {
        MeterTone::Session => MINT,
        MeterTone::Allowance => GOLD_FILL,
        MeterTone::Unknown => DIM,
    }
}

fn rail_spans(
    used_percent: f64,
    pace: Option<f64>,
    width: usize,
    fill: Color,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let used_halves =
        ((used_percent.clamp(0.0, 100.0) / 100.0) * (width as f64 * 2.0)).round() as usize;
    let marker = pace.map(|pace| {
        let column = ((pace.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
        column.min(width - 1)
    });
    let gap_left = marker.and_then(|column| column.checked_sub(1));
    let gap_right = marker
        .filter(|column| column + 1 < width)
        .map(|column| column + 1);
    let mut spans = Vec::new();
    let mut index = 0;
    while index < width {
        if marker == Some(index) {
            spans.push(Span::styled("┃", Style::default().fg(CYAN)));
            index += 1;
            continue;
        }
        if gap_left == Some(index) || gap_right == Some(index) {
            spans.push(Span::raw(" "));
            index += 1;
            continue;
        }
        let start = index;
        while index < width
            && marker != Some(index)
            && gap_left != Some(index)
            && gap_right != Some(index)
        {
            index += 1;
        }
        let mut run = String::new();
        for cell in start..index {
            let left = cell * 2 < used_halves;
            let right = cell * 2 + 1 < used_halves;
            run.push(match (left, right) {
                (true, true) => '━',
                (true, false) => '╸',
                (false, true) => '╺',
                (false, false) => '─',
            });
        }
        spans.extend(split_rail(&run, fill));
    }
    spans
}

fn split_rail(run: &str, fill: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_fill = None;
    for ch in run.chars() {
        let is_fill = ch != '─';
        if current_fill == Some(is_fill) {
            current.push(ch);
            continue;
        }
        if !current.is_empty() {
            let color = if current_fill == Some(true) {
                fill
            } else {
                DIM
            };
            spans.push(Span::styled(current, Style::default().fg(color)));
        }
        current = ch.to_string();
        current_fill = Some(is_fill);
    }
    if !current.is_empty() {
        let color = if current_fill == Some(true) {
            fill
        } else {
            DIM
        };
        spans.push(Span::styled(current, Style::default().fg(color)));
    }
    spans
}

fn provider_accent(provider: Provider) -> Color {
    match provider {
        Provider::Claude => CLAUDE,
        Provider::Codex => CODEX,
        Provider::Grok => GROK,
    }
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

fn render_focused_body(
    frame: &mut Frame<'_>,
    snapshot: &DashboardSnapshot,
    state: &AppState,
    now: DateTime<Utc>,
    area: Rect,
) {
    let Some((_, _, account)) = selected_account(snapshot, state) else {
        frame.render_widget(
            Paragraph::new(
                "No matching accounts. Configure credentials or clear the provider filter.",
            )
            .style(Style::default().fg(YELLOW).bg(BG)),
            area,
        );
        return;
    };

    let width = area.width as usize;
    let card = dashboard::account_card(account, state.weekly_only, now);
    let mut lines = card_lines(&card, width);
    if !account.details.is_empty() {
        lines.push(blank_line());
        lines.push(Line::from(Span::styled(
            "details",
            Style::default().fg(MUTED).bg(BG),
        )));
        lines.extend(detail_grid_lines(account, width));
    }
    for window in account
        .windows
        .iter()
        .filter(|window| dashboard::window_is_weekly_view(window, state.weekly_only))
    {
        if window.history.iter().any(|value| *value > 0) {
            lines.push(blank_line());
            lines.push(Line::from(Span::styled(
                format!("{} history", dashboard::meter_label(window)),
                Style::default().fg(MUTED).bg(BG),
            )));
            lines.extend(history_chart_lines(&window.history, width));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(FG).bg(BG))
            .scroll((state.scroll, 0)),
        area,
    );
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

fn health_color(state: HealthState) -> Color {
    match state {
        HealthState::Ok => MINT,
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

    fn usage_marker_cells(buffer: &Buffer) -> Vec<(u16, u16)> {
        (0..buffer.area.height)
            .flat_map(|y| {
                (0..buffer.area.width)
                    .filter(|&x| buffer[(x, y)].symbol() == "┃" && buffer[(x, y)].fg == CYAN)
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
    fn overview_is_one_rounded_card_per_account() {
        let buffer = draw_overview(160, 40, &AppState::new());
        let text = buffer_text(&buffer);
        assert!(text.contains("╭"));
        assert!(text.contains("● claude work"));
        assert!(text.contains("max 20x · oauth"));
        assert!(text.contains("session"));
        assert!(text.contains("empty in"));
        assert!(text.contains('━'));
        assert!(text.contains('┃'));
        assert!(!text.contains("ACCOUNT"));
        let footer = buffer_line(&buffer, 39);
        assert!(footer.contains("p profile"));
        assert_eq!(buffer[(0, 0)].bg, BG);
    }

    #[test]
    fn session_meter_is_mint_and_week_meter_is_gold() {
        let buffer = draw_overview(120, 24, &AppState::new());
        let session_y = (0..buffer.area.height)
            .find(|y| buffer_line(&buffer, *y).contains("session"))
            .expect("session row");
        let week_y = (0..buffer.area.height)
            .find(|y| {
                let line = buffer_line(&buffer, *y);
                line.contains("week") && line.contains('━')
            })
            .expect("week row");
        assert!(row_has_fg(&buffer, session_y, MINT));
        assert!(row_has_fg(&buffer, week_y, GOLD_FILL));
        assert!(row_has_fg(&buffer, session_y, CYAN));
        assert!(row_has_fg(&buffer, week_y, GOLD));
    }

    #[test]
    fn narrow_card_keeps_meter_label_and_percent() {
        let text = buffer_text(&draw_overview(48, 24, &AppState::new()));
        assert!(text.contains("session"));
        assert!(text.contains('%'));
    }

    #[test]
    fn pace_marker_breaks_the_rail_and_stays_teal() {
        let ahead = rail_spans(80.0, Some(20.0), 12, MINT);
        let text: String = ahead.iter().map(|span| span.content.as_ref()).collect();
        assert!(text.contains(" ┃ "), "{text}");
        let marker = ahead
            .iter()
            .find(|span| span.content.as_ref() == "┃")
            .expect("marker");
        assert_eq!(marker.style.fg, Some(CYAN));
        assert_eq!(marker.style.bg, None);
        assert!(text.contains('━'));
        assert!(text.contains('─'));
    }

    #[test]
    fn overview_and_focused_pace_ticks_are_teal_without_a_fill() {
        let now = Utc::now();
        let remaining = ChronoDuration::hours(2) + ChronoDuration::minutes(30);
        let mut focused = AppState::new();
        focused.view = ViewMode::Focused;
        for state in [&AppState::new(), &focused] {
            let buffer = draw_snapshot(
                &paced_account_snapshot(65.0, remaining, now),
                state,
                160,
                16,
                now,
            );
            let cells = usage_marker_cells(&buffer);
            assert!(!cells.is_empty(), "expected pace marker");
            for (x, y) in &cells {
                assert_eq!(buffer[(*x, *y)].fg, CYAN);
                assert_eq!(buffer[(*x, *y)].bg, BG);
                if *x > 0 {
                    assert_eq!(buffer[(*x - 1, *y)].symbol(), " ");
                }
            }
        }
    }

    #[test]
    fn cards_keep_rounded_titles_and_scroll() {
        let buffer = draw_overview(160, 48, &AppState::new());
        let text = buffer_text(&buffer);
        assert!(text.contains("╭"));
        assert!(text.contains("● claude work"));
        assert!(text.contains("● claude personal"));
        assert!(text.contains("session"));

        let mut state = AppState::new();
        state.scroll = 7;
        let scrolled = draw_overview(160, 12, &state);
        let scrolled_text = buffer_text(&scrolled);
        assert!(buffer_line(&scrolled, 0).contains("aiwatch"));
        assert!(scrolled_text.contains("codex") || scrolled_text.contains("grok"));
    }

    #[test]
    fn wide_overview_places_two_cards_per_row() {
        let buffer = draw_overview(160, 40, &AppState::new());
        let y = (0..buffer.area.height)
            .find(|y| buffer_line(&buffer, *y).contains("● claude work"))
            .expect("claude work title");
        let line = buffer_line(&buffer, y);
        assert!(line.contains("● codex work"), "{line}");
        assert_eq!(line.matches('╭').count(), 2, "{line}");
    }

    #[test]
    fn overview_groups_personal_then_work_under_headings() {
        let text = buffer_text(&draw_overview(160, 60, &AppState::new()));
        let position = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}\n{text}"))
        };
        let personal = position("personal  ·  2 accounts");
        let work = position("work  ·  3 accounts");
        assert!(personal < work);
        assert!(personal < position("● claude personal"));
        assert!(position("● codex personal") < work);
        for card in ["● claude work", "● codex work", "● grok work"] {
            assert!(work < position(card), "{card}");
        }
    }

    #[test]
    fn ungrouped_accounts_render_without_headings() {
        let mut snapshot = DashboardSnapshot::empty();
        snapshot.accounts.push(AccountSnapshot::empty(
            "claude:default",
            "default",
            Provider::Claude,
            FetchHealth::ok(),
        ));
        let buffer = draw_snapshot(&snapshot, &AppState::new(), 160, 16, Utc::now());
        let text = buffer_text(&buffer);
        assert!(text.contains("● claude"));
        assert!(!text.contains("other  ·"), "{text}");
    }

    #[test]
    fn narrow_overview_keeps_one_card_per_row() {
        let buffer = draw_overview(90, 60, &AppState::new());
        assert!((0..buffer.area.height).all(|y| buffer_line(&buffer, y).matches('╭').count() <= 1));
        assert!(buffer_text(&buffer).contains("● claude personal"));
    }

    #[test]
    fn card_titles_show_the_account_email() {
        let text = buffer_text(&draw_overview(160, 40, &AppState::new()));
        assert!(text.contains("● claude work · work@example.com"), "{text}");
        assert!(text.contains("personal@example.com"));
    }

    #[test]
    fn side_by_side_cards_stay_aligned_when_content_overflows() {
        let now = Utc::now();
        let mut snapshot = DashboardSnapshot {
            generated_at: now,
            accounts: Vec::new(),
        };
        let mut failing = AccountSnapshot::empty(
            "claude:work",
            "work",
            Provider::Claude,
            FetchHealth::failure(
                HealthState::AuthenticationRequired,
                "authentication required: run `aiwatch account login claude work` to sign in again",
                Some(401),
            ),
        );
        failing.email = Some("someone.with.a.very.long.address@subdomain.example.com".into());
        failing.plan = Some("team".into());
        snapshot.accounts.push(failing);
        snapshot.accounts.push(AccountSnapshot::empty(
            "codex:work",
            "work",
            Provider::Codex,
            FetchHealth::ok(),
        ));

        let buffer = draw_snapshot(&snapshot, &AppState::new(), 100, 12, now);
        let card_width = (100 - COLUMN_GAP as u16) / 2;
        let right_start = card_width + COLUMN_GAP as u16;
        let mut card_rows = 0;
        for y in 1..buffer.area.height - 1 {
            let left_edge = buffer[(0, y)].symbol();
            if !matches!(left_edge, "╭" | "│" | "╰") {
                continue;
            }
            card_rows += 1;
            let line = buffer_line(&buffer, y);
            assert!(
                matches!(buffer[(card_width - 1, y)].symbol(), "╮" | "│" | "╯"),
                "{line}"
            );
            assert!(
                matches!(buffer[(right_start, y)].symbol(), "╭" | "│" | "╰"),
                "{line}"
            );
        }
        assert!(card_rows >= 3);
        assert!(buffer_text(&buffer).contains('…'));
    }

    #[test]
    fn cards_pad_their_content_and_space_meter_rows() {
        let buffer = draw_overview(160, 48, &AppState::new());
        let title_y = (0..buffer.area.height)
            .find(|y| buffer_line(&buffer, *y).contains("● claude work"))
            .expect("claude work title");
        let rows = (title_y..title_y + 7)
            .map(|y| buffer_line(&buffer, y))
            .collect::<Vec<_>>();
        let card = |row: &str| row.chars().take(10).collect::<String>();

        assert!(rows[0].starts_with("╭─ ●"), "{}", rows[0]);
        assert_eq!(card(&rows[1]).trim_end(), "│");
        assert!(card(&rows[2]).starts_with("│  session"), "{}", rows[2]);
        assert_eq!(card(&rows[3]).trim_end(), "│");
        assert!(card(&rows[4]).starts_with("│  week"), "{}", rows[4]);
        assert_eq!(card(&rows[5]).trim_end(), "│");
        assert!(rows[6].starts_with('╰'), "{}", rows[6]);
    }

    #[test]
    fn each_card_reports_weekly_usage_once_with_bars_of_one_length() {
        let buffer = draw_overview(160, 48, &AppState::new());
        let text = buffer_text(&buffer);
        assert!(!text.contains("% week"), "{text}");
        // The demo's claude work card burns its session before reset.
        assert!(
            text.lines()
                .any(|line| line.starts_with('╰') && line.contains("empty in")),
            "{text}"
        );

        // Every meter row's rail spans the same columns within its card, in both card columns.
        let card_width = (160 - COLUMN_GAP) / 2;
        let is_rail = |cell: &str| matches!(cell, "━" | "╸" | "╺" | "─" | "┃");
        let mut rails = Vec::new();
        for y in 0..buffer.area.height {
            for left in [0, card_width + COLUMN_GAP] {
                let cells = (left..left + card_width)
                    .map(|x| buffer[(x as u16, y)].symbol().to_string())
                    .collect::<Vec<_>>();
                let label = cells[3..10].concat();
                if cells[0] != "│" || label.trim().is_empty() {
                    continue;
                }
                let first = cells.iter().position(|cell| is_rail(cell)).unwrap();
                let last = cells.iter().rposition(|cell| is_rail(cell)).unwrap();
                rails.push((first, last));
            }
        }
        assert!(rails.len() >= 9, "{rails:?}");
        assert!(rails.windows(2).all(|pair| pair[0] == pair[1]), "{rails:?}");
    }

    #[test]
    fn long_emails_shorten_and_tiny_ones_drop() {
        assert_eq!(
            fit_email("a@example.com", 20).as_deref(),
            Some("a@example.com")
        );
        assert_eq!(
            fit_email("someone@example.com", 10).as_deref(),
            Some("someone@e…")
        );
        assert_eq!(fit_email("someone@example.com", 5), None);
    }

    fn row_has_fg(buffer: &Buffer, y: u16, color: Color) -> bool {
        (0..buffer.area.width).any(|x| buffer[(x, y)].fg == color)
    }
}
