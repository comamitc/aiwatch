use std::time::Duration as PollDuration;

use chrono::{DateTime, Utc};

use crate::{
    dashboard,
    model::{DashboardSnapshot, HealthState, UsageWindow},
};

pub fn render_text(snapshot: &DashboardSnapshot, width: usize, poll: PollDuration) -> String {
    dashboard::render_text(snapshot, width, poll, Utc::now())
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
    dashboard::sparkline(&window.history)
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
    use crate::demo;

    #[test]
    fn text_bar_has_stable_width_and_pace_marker() {
        assert_eq!(dashboard::usage_bar(50.0, 10, None).chars().count(), 10);
        assert_eq!(dashboard::usage_bar(150.0, 10, None), "██████████");
        let with_pace = dashboard::usage_bar(50.0, 10, Some(20.0));
        assert_eq!(with_pace.chars().count(), 10);
        assert_eq!(with_pace.chars().nth(2), Some('│'));
    }

    #[test]
    fn once_text_renders_compact_dashboard() {
        let text = render_text(&demo::snapshot(), 160, PollDuration::from_secs(60));
        assert!(text.contains("ACCOUNT"));
        assert!(text.contains("pace:"));
        assert!(text.contains("CLAUDE"));
    }
}
