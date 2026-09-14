use std::fmt::Write;

use chrono::{DateTime, Local, Utc};

use crate::model::{DashboardSnapshot, HealthState, UsageWindow};

pub fn render_text(snapshot: &DashboardSnapshot, width: usize) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "aiwatch {} accounts · {} providers · {}",
        snapshot.accounts.len(),
        snapshot.provider_count(),
        snapshot
            .generated_at
            .with_timezone(&Local)
            .format("%H:%M:%S")
    );

    if let Some((account, window)) = snapshot.nearest_limit() {
        let _ = writeln!(
            output,
            "nearest cap: {}/{} · {:.1}% left",
            account.provider,
            account.name,
            window.remaining_percent()
        );
    }

    for provider in snapshot.providers() {
        let _ = writeln!(output, "\n{}", provider.label());
        for account in snapshot.accounts.iter().filter(|a| a.provider == provider) {
            let plan = account
                .plan
                .as_deref()
                .map(|plan| format!(" · {plan}"))
                .unwrap_or_default();
            let _ = writeln!(
                output,
                "  {}{} · {}",
                account.name,
                plan,
                health_text(account.health.state, account.health.status_code)
            );
            if account.windows.is_empty() {
                if let Some(message) = account.health.message.as_deref() {
                    let _ = writeln!(output, "    {message}");
                }
            }
            for window in &account.windows {
                let bar_width = width.saturating_sub(46).clamp(10, 40);
                let _ = writeln!(
                    output,
                    "    {:<12} {} {:>6.1}% used  {}",
                    window.label,
                    text_bar(window.used_percent, bar_width),
                    window.used_percent,
                    format_reset(window.resets_at)
                );
            }
            if !account.details.is_empty() {
                let details = account
                    .details
                    .iter()
                    .map(|detail| format!("{} {}", detail.label, detail.value))
                    .collect::<Vec<_>>()
                    .join(" · ");
                let _ = writeln!(output, "    {details}");
            }
        }
    }
    output
}

pub fn text_bar(used_percent: f64, width: usize) -> String {
    let filled = ((used_percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn format_reset(reset: Option<DateTime<Utc>>) -> String {
    let Some(reset) = reset else {
        return "reset unknown".to_string();
    };
    let remaining = reset - Utc::now();
    if remaining.num_seconds() <= 0 {
        return "reset due".to_string();
    }
    if remaining.num_days() > 0 {
        return format!(
            "resets in {}d {}h",
            remaining.num_days(),
            remaining.num_hours() % 24
        );
    }
    if remaining.num_hours() > 0 {
        return format!(
            "resets in {}h {}m",
            remaining.num_hours(),
            remaining.num_minutes() % 60
        );
    }
    format!("resets in {}m", remaining.num_minutes().max(1))
}

pub fn trend(window: &UsageWindow) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    window
        .history
        .iter()
        .map(|value| {
            let index = ((*value).min(100) as usize * (BLOCKS.len() - 1)) / 100;
            BLOCKS[index]
        })
        .collect()
}

pub fn health_text(state: HealthState, status: Option<u16>) -> String {
    let label = match state {
        HealthState::Ok => "ok",
        HealthState::Stale => "stale",
        HealthState::AuthenticationRequired => "login required",
        HealthState::RateLimited => "rate limited",
        HealthState::Error => "error",
    };
    status.map_or_else(|| label.to_string(), |status| format!("{status} {label}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_bar_has_stable_width() {
        assert_eq!(text_bar(50.0, 10).chars().count(), 10);
        assert_eq!(text_bar(150.0, 10), "██████████");
    }
}
