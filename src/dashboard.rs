use std::{collections::BTreeSet, time::Duration as PollDuration};

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};

use crate::model::{
    AccountSnapshot, DashboardSnapshot, HealthState, Provider, UsageWindow, WindowClass,
};

pub const PACE_LEGEND: &str = "pace: green at or below this window's pace · yellow up to 10 over · orange 10-20 over · red >20 over or exhausted · gray unknown reset · │ on-pace marker";

pub const ACCOUNT_WIDTH: usize = 28;
pub const WINDOW_WIDTH: usize = 18;
pub const USAGE_WIDTH: usize = 42;
pub const USED_CAP_WIDTH: usize = 20;
pub const RESETS_WIDTH: usize = 14;
pub const SPARK_WIDTH: usize = 22;
pub const ACCENT_WIDTH: usize = 1;
pub const TARGET_WIDTH: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceBand {
    Green,
    Yellow,
    Orange,
    Red,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileFilter {
    All,
    Personal,
    Work,
}

impl ProfileFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Personal,
            Self::Personal => Self::Work,
            Self::Work => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Personal => "personal",
            Self::Work => "work",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnLayout {
    pub accent: usize,
    pub account: usize,
    pub window: usize,
    pub usage: usize,
    pub used_cap: usize,
    pub show_cap: bool,
    pub resets: usize,
    pub spark: usize,
    pub show_spark: bool,
}

impl ColumnLayout {
    pub fn for_width(width: usize) -> Self {
        let mut layout = Self {
            accent: ACCENT_WIDTH,
            account: ACCOUNT_WIDTH,
            window: WINDOW_WIDTH,
            usage: USAGE_WIDTH,
            used_cap: USED_CAP_WIDTH,
            show_cap: true,
            resets: RESETS_WIDTH,
            spark: SPARK_WIDTH,
            show_spark: true,
        };
        if width < layout.occupied_width() {
            layout.show_spark = false;
            layout.spark = 0;
        }
        if width < layout.occupied_width() {
            layout.show_cap = false;
            layout.used_cap = 8;
        }
        if width < layout.occupied_width() {
            let overflow = layout.occupied_width().saturating_sub(width);
            layout.usage = layout.usage.saturating_sub(overflow).max(4);
        }
        layout
    }

    pub fn occupied_width(self) -> usize {
        let mut width = self.accent
            + self.account
            + 1
            + self.window
            + 1
            + self.usage
            + 1
            + self.used_cap
            + 1
            + self.resets;
        if self.show_spark {
            width += 1 + self.spark;
        }
        width
    }

    pub fn used_cap_header(self) -> &'static str {
        if self.show_cap {
            "% USED / CAP"
        } else {
            "% USED"
        }
    }
}

#[derive(Debug, Clone)]
pub struct OverviewModel<'a> {
    pub header_line1: String,
    pub header_line2: String,
    pub layout: ColumnLayout,
    pub sections: Vec<ProviderSection<'a>>,
}

#[derive(Debug, Clone)]
pub struct ProviderSection<'a> {
    pub provider: Provider,
    pub account_count: usize,
    pub peak: Option<f64>,
    pub details: String,
    pub rows: Vec<WindowRow<'a>>,
}

#[derive(Debug, Clone)]
pub struct WindowRow<'a> {
    pub account: &'a AccountSnapshot,
    pub window: Option<&'a UsageWindow>,
    pub first_for_account: bool,
    pub account_cell: String,
    pub window_cell: String,
    pub used_percent: Option<f64>,
    pub pace: Option<f64>,
    pub used_cap_cell: String,
    pub resets_cell: String,
    pub spark_cell: String,
}

pub fn pace_band(used_percent: f64, pace: Option<f64>) -> PaceBand {
    if used_percent >= 100.0 {
        return PaceBand::Red;
    }
    let Some(pace) = pace else {
        return PaceBand::Gray;
    };
    let delta = used_percent - pace;
    if delta <= 0.0 {
        PaceBand::Green
    } else if delta <= 10.0 {
        PaceBand::Yellow
    } else if delta <= 20.0 {
        PaceBand::Orange
    } else {
        PaceBand::Red
    }
}

pub fn account_matches_profile(name: &str, filter: ProfileFilter) -> bool {
    match filter {
        ProfileFilter::All => true,
        ProfileFilter::Personal => name_has_token(name, "personal"),
        ProfileFilter::Work => name_has_token(name, "work"),
    }
}

pub fn name_has_token(name: &str, needle: &str) -> bool {
    name.split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| token.eq_ignore_ascii_case(needle))
}

pub fn visible_accounts(
    snapshot: &DashboardSnapshot,
    provider: Option<Provider>,
    profile: ProfileFilter,
) -> Vec<&AccountSnapshot> {
    snapshot
        .accounts
        .iter()
        .filter(|account| provider.is_none_or(|filter| account.provider == filter))
        .filter(|account| account_matches_profile(&account.name, profile))
        .collect()
}

pub fn window_is_weekly_view(window: &UsageWindow, weekly_only: bool) -> bool {
    !weekly_only || window.key.contains("weekly") || window.key.contains("monthly")
}

pub fn peak_weekly_percent(accounts: &[&AccountSnapshot]) -> Option<f64> {
    accounts
        .iter()
        .flat_map(|account| &account.windows)
        .filter(|window| window.class() == WindowClass::SevenDay)
        .map(|window| window.used_percent)
        .max_by(f64::total_cmp)
}

pub fn headroom_percent(peak: Option<f64>) -> Option<f64> {
    peak.map(|peak| 100.0 - peak)
}

pub fn soonest_future_reset<'a>(
    windows: impl IntoIterator<Item = &'a UsageWindow>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    windows
        .into_iter()
        .filter_map(|window| window.resets_at)
        .filter(|reset| *reset > now)
        .min()
}

pub fn sparkline(history: &[u64]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    history
        .iter()
        .map(|value| {
            let index = ((*value).min(100) as usize * (BLOCKS.len() - 1)) / 100;
            BLOCKS[index]
        })
        .collect()
}

pub fn usage_bar(used_percent: f64, width: usize, pace: Option<f64>) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = ((used_percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let mut cells: Vec<char> = (0..width)
        .map(|index| if index < filled { '█' } else { '░' })
        .collect();
    if let Some(column) = pace_column(pace, width) {
        cells[column] = '│';
    }
    cells.into_iter().collect()
}

pub fn pace_column(pace: Option<f64>, width: usize) -> Option<usize> {
    let pace = pace?;
    if width == 0 {
        return None;
    }
    let column = ((pace.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    Some(column.min(width - 1))
}

pub fn compact_countdown(reset: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = reset - now;
    if remaining.num_seconds() <= 0 {
        return "due".to_string();
    }
    format_duration(remaining)
}

pub fn format_duration(remaining: ChronoDuration) -> String {
    if remaining.num_seconds() <= 0 {
        return "now".to_string();
    }
    if remaining.num_days() > 0 {
        format!("{}d {}h", remaining.num_days(), remaining.num_hours() % 24)
    } else if remaining.num_hours() > 0 {
        format!(
            "{}h {}m",
            remaining.num_hours(),
            remaining.num_minutes() % 60
        )
    } else {
        format!("{}m", remaining.num_minutes().max(1))
    }
}

/// Time until this window hits 100% at its current burn, if that happens before reset.
pub fn empties_in(window: &UsageWindow, now: DateTime<Utc>) -> Option<ChronoDuration> {
    if window.used_percent >= 100.0 {
        return Some(ChronoDuration::zero());
    }
    let reset = window.resets_at?;
    let duration = window.class().duration()?;
    let remaining = reset - now;
    if remaining <= ChronoDuration::zero() || remaining > duration {
        return None;
    }
    let elapsed = duration - remaining;
    if elapsed < ChronoDuration::minutes(1) || window.used_percent <= 0.0 {
        return None;
    }
    let rate = window.used_percent / elapsed.num_milliseconds() as f64;
    if rate <= 0.0 {
        return None;
    }
    let projected = ChronoDuration::milliseconds(
        ((100.0 - window.used_percent) / rate).round().max(0.0) as i64,
    );
    if projected > remaining {
        return None;
    }
    Some(projected)
}

pub fn soonest_empty<'a>(
    windows: impl IntoIterator<Item = &'a UsageWindow>,
    now: DateTime<Utc>,
) -> Option<ChronoDuration> {
    windows
        .into_iter()
        .filter_map(|window| empties_in(window, now))
        .min()
}

pub fn percent_label(used: f64) -> String {
    format!("{:.0}%", used.clamp(0.0, 100.0).round())
}

pub fn meter_label(window: &UsageWindow) -> String {
    match window.class() {
        WindowClass::FiveHour => "session".to_string(),
        WindowClass::Monthly => "month".to_string(),
        WindowClass::SevenDay => scoped_week_label(&window.label),
        WindowClass::Unknown => {
            let label = window.label.trim().to_ascii_lowercase();
            if label.is_empty() {
                "usage".to_string()
            } else {
                label
            }
        }
    }
}

fn scoped_week_label(label: &str) -> String {
    let token = label
        .split_whitespace()
        .next()
        .unwrap_or("week")
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    if token.is_empty() || token == "weekly" || token == "week" || token == "7d" {
        "week".to_string()
    } else {
        token
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterTone {
    Session,
    Allowance,
    Unknown,
}

pub fn meter_tone(window: &UsageWindow) -> MeterTone {
    match window.class() {
        WindowClass::FiveHour => MeterTone::Session,
        WindowClass::SevenDay | WindowClass::Monthly => MeterTone::Allowance,
        WindowClass::Unknown => MeterTone::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterColumns {
    pub label: usize,
    pub bar: usize,
    pub percent: usize,
    pub time: usize,
}

pub fn meter_columns(width: usize) -> MeterColumns {
    if width >= 36 {
        MeterColumns {
            label: 8,
            bar: width - 22,
            percent: 4,
            time: 7,
        }
    } else if width >= 20 {
        MeterColumns {
            label: 8,
            bar: width.saturating_sub(14),
            percent: 4,
            time: 0,
        }
    } else {
        MeterColumns {
            label: width.min(8),
            bar: 0,
            percent: 0,
            time: 0,
        }
    }
}

pub const SUMMARY_BAR_WIDTH: usize = 10;

#[derive(Debug, Clone)]
pub struct CardSummary {
    pub percent: f64,
    pub label: String,
    pub pace: Option<f64>,
    pub empty_in: Option<String>,
    pub tone: MeterTone,
}

#[derive(Debug, Clone)]
pub struct CardMeter {
    pub label: String,
    pub used_percent: f64,
    pub pace: Option<f64>,
    pub reset: String,
    pub tone: MeterTone,
}

#[derive(Debug, Clone)]
pub struct AccountCard<'a> {
    pub account: &'a AccountSnapshot,
    pub title: String,
    pub account_name: Option<String>,
    pub plan: Option<String>,
    pub auth: &'static str,
    pub summary: Option<CardSummary>,
    pub meters: Vec<CardMeter>,
    pub notice: Option<String>,
}

pub fn card_auth_label(account: &AccountSnapshot) -> &'static str {
    match account.health.state {
        HealthState::Ok | HealthState::Stale => "oauth",
        HealthState::AuthenticationRequired => "login",
        HealthState::RateLimited => "limited",
        HealthState::Error => "error",
    }
}

pub fn card_account_name(account: &AccountSnapshot) -> Option<String> {
    let name = account.name.trim();
    if name.is_empty()
        || name.eq_ignore_ascii_case("default")
        || name.eq_ignore_ascii_case(account.provider.key())
    {
        None
    } else {
        Some(name.to_string())
    }
}

pub fn account_cards<'a>(
    snapshot: &'a DashboardSnapshot,
    provider: Option<Provider>,
    profile: ProfileFilter,
    weekly_only: bool,
    now: DateTime<Utc>,
) -> Vec<AccountCard<'a>> {
    visible_accounts(snapshot, provider, profile)
        .into_iter()
        .map(|account| account_card(account, weekly_only, now))
        .collect()
}

pub fn account_card<'a>(
    account: &'a AccountSnapshot,
    weekly_only: bool,
    now: DateTime<Utc>,
) -> AccountCard<'a> {
    let visible: Vec<&UsageWindow> = account
        .windows
        .iter()
        .filter(|window| window_is_weekly_view(window, weekly_only))
        .collect();
    let summary_window = visible
        .iter()
        .copied()
        .find(|window| meter_label(window) == "week")
        .or_else(|| {
            visible
                .iter()
                .copied()
                .find(|window| window.class() == WindowClass::SevenDay)
        })
        .or_else(|| visible.first().copied());
    let empty_in = soonest_empty(visible.iter().copied(), now).map(format_duration);
    let summary = summary_window.map(|window| CardSummary {
        percent: window.used_percent,
        label: meter_label(window),
        pace: window.pace_used_percent(now),
        empty_in,
        tone: meter_tone(window),
    });
    let meters = visible
        .iter()
        .copied()
        .map(|window| CardMeter {
            label: meter_label(window),
            used_percent: window.used_percent,
            pace: window.pace_used_percent(now),
            reset: reset_cell(window.resets_at, now),
            tone: meter_tone(window),
        })
        .collect::<Vec<_>>();
    let notice = if meters.is_empty() {
        Some(account.health.message.clone().unwrap_or_else(|| {
            if weekly_only {
                "No weekly usage window reported".to_string()
            } else {
                "Quota unavailable".to_string()
            }
        }))
    } else {
        None
    };
    AccountCard {
        account,
        title: account.provider.key().to_string(),
        account_name: card_account_name(account),
        plan: account
            .plan
            .as_deref()
            .map(|plan| plan.trim().to_ascii_lowercase())
            .filter(|plan| !plan.is_empty()),
        auth: card_auth_label(account),
        summary,
        meters,
        notice,
    }
}

pub fn status_line(account_count: usize, profile: ProfileFilter, poll: PollDuration) -> String {
    format!(
        "aiwatch {}  ·  {account_count} accounts  ·  profile {}  ·  poll {}s",
        env!("CARGO_PKG_VERSION"),
        profile.label(),
        poll.as_secs()
    )
}

pub fn render_card_text(card: &AccountCard<'_>, width: usize) -> String {
    let mut lines = vec![identity_line(card, width)];
    if let Some(summary) = &card.summary {
        lines.push(summary_line(summary, width));
    }
    if card.meters.is_empty() {
        if let Some(notice) = &card.notice {
            lines.push(pad_cell(notice, width));
        }
    } else {
        lines.push(String::new());
        for meter in &card.meters {
            lines.push(meter_line(meter, width));
        }
    }
    lines.join("\n")
}

fn identity_line(card: &AccountCard<'_>, width: usize) -> String {
    let mut left = format!("● {}", card.title);
    if let Some(name) = &card.account_name {
        left.push(' ');
        left.push_str(name);
    }
    let right = match &card.plan {
        Some(plan) => format!("{plan} ● {}", card.auth),
        None => format!("● {}", card.auth),
    };
    align_edges(&left, &right, width)
}

fn summary_line(summary: &CardSummary, width: usize) -> String {
    let bar = usage_bar(summary.percent, SUMMARY_BAR_WIDTH.min(width), summary.pace);
    let left = format!("{} {} {bar}", percent_label(summary.percent), summary.label);
    let empty = summary.empty_in.as_deref().unwrap_or("—");
    align_edges(&left, &format!("empty in {empty}"), width)
}

fn meter_line(meter: &CardMeter, width: usize) -> String {
    let columns = meter_columns(width);
    let mut line = pad_cell(&meter.label, columns.label);
    if columns.bar > 0 {
        line.push(' ');
        line.push_str(&usage_bar(meter.used_percent, columns.bar, meter.pace));
    }
    if columns.percent > 0 {
        line.push(' ');
        line.push_str(&pad_cell(
            &percent_label(meter.used_percent),
            columns.percent,
        ));
    }
    if columns.time > 0 {
        line.push(' ');
        line.push_str(&align_right(&meter.reset, columns.time));
    }
    pad_cell(&line, width)
}

fn align_edges(left: &str, right: &str, width: usize) -> String {
    let left_width = left.chars().count();
    let right_width = right.chars().count();
    if left_width + 1 + right_width >= width {
        return pad_cell(&format!("{left} {right}"), width);
    }
    let mut line = left.to_string();
    line.push_str(&" ".repeat(width - left_width - right_width));
    line.push_str(right);
    line
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

pub fn age_text(age: chrono::Duration) -> String {
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

pub fn pad_cell(text: &str, width: usize) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    if chars.len() > width {
        chars.truncate(width);
    }
    let mut output: String = chars.into_iter().collect();
    while output.chars().count() < width {
        output.push(' ');
    }
    output
}

pub fn build_overview<'a>(
    snapshot: &'a DashboardSnapshot,
    width: usize,
    poll: PollDuration,
    provider: Option<Provider>,
    profile: ProfileFilter,
    weekly_only: bool,
    now: DateTime<Utc>,
) -> OverviewModel<'a> {
    let visible = visible_accounts(snapshot, provider, profile);
    let layout = ColumnLayout::for_width(width);
    let header_line1 = header_line1(&visible, profile, now);
    let header_line2 = header_line2(&visible, poll, now);
    let sections = Provider::ALL
        .into_iter()
        .filter(|candidate| provider.is_none_or(|filter| filter == *candidate))
        .filter_map(|candidate| {
            let accounts = visible
                .iter()
                .copied()
                .filter(|account| account.provider == candidate)
                .collect::<Vec<_>>();
            if accounts.is_empty() {
                return None;
            }
            Some(provider_section(
                candidate,
                &accounts,
                &layout,
                weekly_only,
                now,
            ))
        })
        .collect();
    OverviewModel {
        header_line1,
        header_line2,
        layout,
        sections,
    }
}

pub fn render_text(
    snapshot: &DashboardSnapshot,
    width: usize,
    poll: PollDuration,
    now: DateTime<Utc>,
) -> String {
    let cards = account_cards(snapshot, None, ProfileFilter::All, false, now);
    let mut output = fit_line(&status_line(cards.len(), ProfileFilter::All, poll), width);
    output.push('\n');
    if cards.is_empty() {
        output.push_str(&fit_line("No matching accounts.", width));
        output.push('\n');
        return output;
    }
    for card in &cards {
        output.push('\n');
        output.push_str(&render_card_text(card, width));
        output.push('\n');
    }
    output
}

pub fn render_overview_text(model: &OverviewModel<'_>, width: usize) -> String {
    let mut output = String::new();
    output.push_str(&fit_line(&model.header_line1, width));
    output.push('\n');
    output.push_str(&fit_line(&model.header_line2, width));
    output.push('\n');
    output.push_str(&fit_line(&column_header_line(model.layout), width));
    output.push('\n');
    for section in &model.sections {
        output.push_str(&fit_line(
            &provider_header_line(section, model.layout, width),
            width,
        ));
        output.push('\n');
        for row in &section.rows {
            output.push_str(&fit_line(&window_row_line(row, model.layout), width));
            output.push('\n');
        }
    }
    output.push_str(&fit_line(PACE_LEGEND, width));
    output.push('\n');
    output
}

pub fn column_header_line(layout: ColumnLayout) -> String {
    let mut line = pad_cell("", layout.accent);
    line.push_str(&pad_cell("ACCOUNT", layout.account));
    line.push(' ');
    line.push_str(&pad_cell("WINDOW", layout.window));
    line.push(' ');
    line.push_str(&pad_cell("USAGE", layout.usage));
    line.push(' ');
    line.push_str(&pad_cell(layout.used_cap_header(), layout.used_cap));
    line.push(' ');
    line.push_str(&pad_cell("RESETS", layout.resets));
    if layout.show_spark {
        line.push(' ');
        line.push_str(&pad_cell("7D", layout.spark));
    }
    line
}

pub fn provider_header_text(section: &ProviderSection<'_>) -> String {
    let peak = section
        .peak
        .map(|peak| format!("{peak:.1}%"))
        .unwrap_or_else(|| "—".to_string());
    let headroom = headroom_percent(section.peak)
        .map(|headroom| format!("{headroom:.1}%"))
        .unwrap_or_else(|| "—".to_string());
    let accounts = if section.account_count == 1 {
        "1 account".to_string()
    } else {
        format!("{} accounts", section.account_count)
    };
    format!(
        "{}  {accounts}  ·  peak {peak}  ·  headroom {headroom}",
        section.provider.label()
    )
}

fn header_line1(
    visible: &[&AccountSnapshot],
    profile: ProfileFilter,
    now: DateTime<Utc>,
) -> String {
    let providers = visible
        .iter()
        .map(|account| account.provider)
        .collect::<BTreeSet<_>>()
        .len();
    format!(
        "aiwatch {}  ·  {} accounts · {} providers  ·  profile {}  ·  {}",
        env!("CARGO_PKG_VERSION"),
        visible.len(),
        providers,
        profile.label(),
        now.with_timezone(&Local).format("%H:%M:%S")
    )
}

fn header_line2(visible: &[&AccountSnapshot], poll: PollDuration, now: DateTime<Utc>) -> String {
    let nearest = soonest_future_reset(visible.iter().flat_map(|account| &account.windows), now)
        .map(|reset| compact_countdown(reset, now))
        .unwrap_or_else(|| "—".to_string());
    let freshness = visible
        .iter()
        .filter_map(|account| account.last_success_at)
        .max()
        .map(|time| age_text(now - time))
        .unwrap_or_else(|| "—".to_string());
    let ok = visible
        .iter()
        .filter(|account| account.health.state == HealthState::Ok)
        .count();
    let error = visible.len().saturating_sub(ok);
    format!(
        "poll {}s  ·  nearest cap {nearest}  ·  freshness {freshness}  ·  health {ok} ok / {error} error",
        poll.as_secs()
    )
}

fn provider_section<'a>(
    provider: Provider,
    accounts: &[&'a AccountSnapshot],
    layout: &ColumnLayout,
    weekly_only: bool,
    now: DateTime<Utc>,
) -> ProviderSection<'a> {
    let details = provider_details(accounts);
    let mut rows = Vec::new();
    for account in accounts {
        let windows = account
            .windows
            .iter()
            .filter(|window| window_is_weekly_view(window, weekly_only))
            .collect::<Vec<_>>();
        if windows.is_empty() {
            rows.push(empty_account_row(account, layout));
            continue;
        }
        for (index, window) in windows.iter().enumerate() {
            rows.push(window_row(
                account,
                window,
                index == 0,
                index + 1 == windows.len(),
                layout,
                now,
            ));
        }
    }
    ProviderSection {
        provider,
        account_count: accounts.len(),
        peak: peak_weekly_percent(accounts),
        details,
        rows,
    }
}

fn provider_details(accounts: &[&AccountSnapshot]) -> String {
    let mut parts = Vec::new();
    for account in accounts {
        for detail in &account.details {
            let part = format!("{} {}", detail.label, detail.value);
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }
    parts.join(" · ")
}

fn window_row<'a>(
    account: &'a AccountSnapshot,
    window: &'a UsageWindow,
    first: bool,
    last: bool,
    layout: &ColumnLayout,
    now: DateTime<Utc>,
) -> WindowRow<'a> {
    let pace = window.pace_used_percent(now);
    WindowRow {
        account,
        window: Some(window),
        first_for_account: first,
        account_cell: account_cell(account, first, last, layout.account),
        window_cell: pad_cell(&window.label, layout.window),
        used_percent: Some(window.used_percent),
        pace,
        used_cap_cell: used_cap_cell(window.used_percent, layout),
        resets_cell: pad_cell(&reset_cell(window.resets_at, now), layout.resets),
        spark_cell: spark_cell(&window.history, layout),
    }
}

fn empty_account_row<'a>(account: &'a AccountSnapshot, layout: &ColumnLayout) -> WindowRow<'a> {
    let message = account
        .health
        .message
        .as_deref()
        .unwrap_or("quota unavailable");
    WindowRow {
        account,
        window: None,
        first_for_account: true,
        account_cell: account_cell(account, true, true, layout.account),
        window_cell: pad_cell(message, layout.window),
        used_percent: None,
        pace: None,
        used_cap_cell: pad_cell("—", layout.used_cap),
        resets_cell: pad_cell("—", layout.resets),
        spark_cell: spark_cell(&[], layout),
    }
}

fn account_cell(account: &AccountSnapshot, first: bool, last: bool, width: usize) -> String {
    if !first {
        let connector = if last { "└─" } else { "├─" };
        return pad_cell(connector, width);
    }
    let plan = account.plan.as_deref().unwrap_or("");
    let text = if plan.is_empty() {
        account.name.clone()
    } else {
        format!("{}  {plan}", account.name)
    };
    pad_cell(&text, width)
}

fn used_cap_cell(used_percent: f64, layout: &ColumnLayout) -> String {
    let text = if layout.show_cap {
        format!("{used_percent:>5.1}% / —")
    } else {
        format!("{used_percent:>5.1}%")
    };
    pad_cell(&text, layout.used_cap)
}

fn reset_cell(reset: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    reset.map_or_else(|| "—".to_string(), |reset| compact_countdown(reset, now))
}

fn spark_cell(history: &[u64], layout: &ColumnLayout) -> String {
    if !layout.show_spark {
        return String::new();
    }
    if history.is_empty() {
        return pad_cell("—", layout.spark);
    }
    pad_cell(&sparkline(history), layout.spark)
}

fn provider_header_line(
    section: &ProviderSection<'_>,
    layout: ColumnLayout,
    width: usize,
) -> String {
    let left = provider_header_text(section);
    if section.details.is_empty() {
        return pad_cell(&format!("{}{left}", " ".repeat(layout.accent)), width);
    }
    let prefix = layout.accent;
    let available = width.saturating_sub(prefix);
    let details_width = section.details.chars().count();
    let left_width = left.chars().count();
    if left_width + 1 + details_width >= available {
        return pad_cell(
            &format!("{}{left}  {}", " ".repeat(prefix), section.details),
            width,
        );
    }
    let gap = available.saturating_sub(left_width + details_width).max(1);
    format!(
        "{}{left}{}{}",
        " ".repeat(prefix),
        " ".repeat(gap),
        section.details
    )
}

fn window_row_line(row: &WindowRow<'_>, layout: ColumnLayout) -> String {
    let bar = row.used_percent.map_or_else(
        || pad_cell("", layout.usage),
        |used| pad_cell(&usage_bar(used, layout.usage, row.pace), layout.usage),
    );
    let mut line = pad_cell("", layout.accent);
    line.push_str(&row.account_cell);
    line.push(' ');
    line.push_str(&row.window_cell);
    line.push(' ');
    line.push_str(&bar);
    line.push(' ');
    line.push_str(&row.used_cap_cell);
    line.push(' ');
    line.push_str(&row.resets_cell);
    if layout.show_spark {
        line.push(' ');
        line.push_str(&row.spark_cell);
    }
    line
}

fn fit_line(text: &str, width: usize) -> String {
    pad_cell(text, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{demo, model::FetchHealth};
    use chrono::Duration;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .expect("fixed now")
            .with_timezone(&Utc)
    }

    fn paced_window(key: &str, label: &str, used: f64, remaining: Duration) -> UsageWindow {
        UsageWindow::new(key, label, used, Some(fixed_now() + remaining))
    }

    fn band_for(used: f64, remaining: Duration) -> PaceBand {
        let window = paced_window("five_hour", "5H", used, remaining);
        pace_band(window.used_percent, window.pace_used_percent(fixed_now()))
    }

    #[test]
    fn pace_bands_cover_delta_boundaries() {
        let half = Duration::hours(2) + Duration::minutes(30);
        assert_eq!(band_for(50.0, half), PaceBand::Green);
        assert_eq!(band_for(50.01, half), PaceBand::Yellow);
        assert_eq!(band_for(60.0, half), PaceBand::Yellow);
        assert_eq!(band_for(60.01, half), PaceBand::Orange);
        assert_eq!(band_for(70.0, half), PaceBand::Orange);
        assert_eq!(band_for(70.01, half), PaceBand::Red);
        assert_eq!(band_for(100.0, half), PaceBand::Red);
    }

    #[test]
    fn unknown_expired_and_inconsistent_resets_are_gray_unless_exhausted() {
        let now = fixed_now();
        let unknown = UsageWindow::new("five_hour", "5H", 12.0, None);
        assert_eq!(
            pace_band(unknown.used_percent, unknown.pace_used_percent(now)),
            PaceBand::Gray
        );
        let expired = UsageWindow::new("weekly", "WEEKLY", 12.0, Some(now - Duration::minutes(1)));
        assert_eq!(
            pace_band(expired.used_percent, expired.pace_used_percent(now)),
            PaceBand::Gray
        );
        let too_far = paced_window("five_hour", "5H", 12.0, Duration::hours(6));
        assert_eq!(
            pace_band(too_far.used_percent, too_far.pace_used_percent(now)),
            PaceBand::Gray
        );
        let monthly = paced_window("monthly", "MONTHLY", 40.0, Duration::days(10));
        assert!(monthly.pace_used_percent(now).is_none());
        assert_eq!(
            pace_band(monthly.used_percent, monthly.pace_used_percent(now)),
            PaceBand::Gray
        );
        let exhausted_unknown = UsageWindow::new("monthly", "MONTHLY", 100.0, None);
        assert_eq!(
            pace_band(
                exhausted_unknown.used_percent,
                exhausted_unknown.pace_used_percent(now)
            ),
            PaceBand::Red
        );
    }

    #[test]
    fn usage_bar_keeps_vertical_on_pace_marker() {
        let bar = usage_bar(50.0, 10, Some(20.0));
        assert_eq!(bar.chars().count(), 10);
        assert_eq!(bar.chars().nth(2), Some('│'));
        assert!(usage_bar(50.0, 10, None).chars().all(|ch| ch != '│'));
    }

    #[test]
    fn provider_peak_is_max_weekly_and_ignores_monthly() {
        let mut account =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        account.windows = vec![
            UsageWindow::new("weekly", "WEEKLY", 40.0, None),
            UsageWindow::new("weekly_opus", "OPUS WEEKLY", 70.0, None),
            UsageWindow::new("monthly", "MONTHLY", 90.0, None),
        ];
        let peak = peak_weekly_percent(&[&account]);
        assert_eq!(peak, Some(70.0));
        assert_eq!(headroom_percent(peak), Some(30.0));

        let mut monthly_only =
            AccountSnapshot::empty("grok:work", "work", Provider::Grok, FetchHealth::ok());
        monthly_only
            .windows
            .push(UsageWindow::new("monthly", "MONTHLY", 55.0, None));
        assert!(peak_weekly_percent(&[&monthly_only]).is_none());
        assert!(headroom_percent(None).is_none());
    }

    #[test]
    fn profile_tokens_classify_exact_personal_and_work_names() {
        assert!(name_has_token("personal", "personal"));
        assert!(name_has_token("work", "work"));
        assert!(name_has_token("my-personal-acct", "personal"));
        assert!(!name_has_token("personality", "personal"));
        assert!(account_matches_profile("other", ProfileFilter::All));
        assert!(!account_matches_profile("other", ProfileFilter::Personal));
        assert!(!account_matches_profile("other", ProfileFilter::Work));
    }

    #[test]
    fn text_renders_one_card_per_account() {
        let snapshot = demo::snapshot();
        let now = snapshot.generated_at;
        let text = render_text(&snapshot, 160, PollDuration::from_secs(60), now);
        let first = text.lines().next().expect("status");
        assert!(first.contains("aiwatch"));
        assert!(first.contains(env!("CARGO_PKG_VERSION")));
        assert!(first.contains("profile all"));
        assert!(first.contains("poll 60s"));
        assert!(text.contains("● claude work"));
        assert!(text.contains("max 20x ● oauth"));
        assert!(text.contains("● codex personal"));
        assert!(text.contains("● grok work"));
        assert!(text.contains("session"));
        assert!(text.contains("week"));
        assert!(text.contains("empty in"));
        assert!(text.contains('│'));
        assert!(!text.contains("ACCOUNT"));
        assert!(!text.contains("7D PEAK"));
        assert!(!text.contains("recommend"));
        assert!(!text.contains("tokens /"));
    }

    #[test]
    fn card_uses_session_week_and_model_labels() {
        let mut snapshot = DashboardSnapshot::empty();
        let mut account =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        account.plan = Some("Pro".into());
        account.windows = vec![
            paced_window("five_hour", "5H", 10.0, Duration::hours(4)),
            paced_window("weekly", "WEEKLY", 20.0, Duration::days(6)),
            UsageWindow::new("weekly_fable", "Fable WEEKLY", 27.0, None),
        ];
        snapshot.accounts.push(account);
        let text = render_text(&snapshot, 80, PollDuration::from_secs(300), fixed_now());
        assert!(text.contains("pro ● oauth"));
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("session")
                    || line.starts_with("week")
                    || line.starts_with("fable"))
                .count(),
            3
        );
        assert!(text.contains("10%"));
        assert!(text.contains("20%"));
        assert!(text.contains("27%"));
    }

    #[test]
    fn empty_in_is_burn_before_reset_not_the_reset_itself() {
        let now = fixed_now();
        let mut fast =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        fast.windows.push(UsageWindow::new(
            "five_hour",
            "5H",
            80.0,
            Some(now + Duration::hours(1)),
        ));
        let mut slow =
            AccountSnapshot::empty("codex:work", "work", Provider::Codex, FetchHealth::ok());
        slow.windows.push(UsageWindow::new(
            "five_hour",
            "5H",
            10.0,
            Some(now + Duration::hours(1)),
        ));
        let fast_text = render_text(
            &DashboardSnapshot {
                generated_at: now,
                accounts: vec![fast],
            },
            80,
            PollDuration::from_secs(300),
            now,
        );
        assert!(fast_text.contains("empty in 1h 0m"), "{fast_text}");
        let slow_text = render_text(
            &DashboardSnapshot {
                generated_at: now,
                accounts: vec![slow],
            },
            80,
            PollDuration::from_secs(300),
            now,
        );
        assert!(slow_text.contains("empty in —"), "{slow_text}");
        assert!(!slow_text.contains("nearest cap"));
    }

    #[test]
    fn scoped_week_is_not_labeled_week_and_monthly_stays_month() {
        let mut claude =
            AccountSnapshot::empty("claude:work", "work", Provider::Claude, FetchHealth::ok());
        claude.windows = vec![
            UsageWindow::new("weekly", "WEEKLY", 40.0, None),
            UsageWindow::new("weekly_sonnet", "SONNET WEEKLY", 80.0, None),
            UsageWindow::new("monthly", "MONTHLY", 99.0, None),
        ];
        let text = render_text(
            &DashboardSnapshot {
                generated_at: fixed_now(),
                accounts: vec![claude],
            },
            80,
            PollDuration::from_secs(300),
            fixed_now(),
        );
        assert!(text.contains("40% week"));
        assert!(text.lines().any(|line| line.starts_with("sonnet")));
        assert!(text.lines().any(|line| line.starts_with("month")));
        assert!(!text.contains("73%"));
        assert!(!text.contains("peak"));
    }
}
