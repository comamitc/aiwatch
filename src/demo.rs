use chrono::{Duration, Utc};

use crate::model::{
    AccountSnapshot, DashboardSnapshot, DetailMetric, FetchHealth, Provider, UsageWindow,
};

pub fn snapshot() -> DashboardSnapshot {
    let now = Utc::now();
    DashboardSnapshot {
        generated_at: now,
        accounts: vec![
            account(
                "claude:work",
                "work",
                Provider::Claude,
                Some("max 20x"),
                vec![
                    window("five_hour", "5H", 99.8, 108, &[10, 45, 31, 65, 72, 58, 79]),
                    window(
                        "weekly",
                        "WEEKLY",
                        53.2,
                        96 * 60,
                        &[18, 30, 25, 44, 48, 39, 53],
                    ),
                ],
                vec![DetailMetric::provider("balance", "$25.00")],
            ),
            account(
                "claude:personal",
                "personal",
                Provider::Claude,
                Some("pro"),
                vec![
                    window("five_hour", "5H", 19.0, 242, &[4, 8, 12, 7, 14, 16, 19]),
                    window(
                        "weekly",
                        "WEEKLY",
                        35.2,
                        96 * 60,
                        &[12, 10, 18, 22, 25, 31, 35],
                    ),
                ],
                Vec::new(),
            ),
            account(
                "codex:work",
                "work",
                Provider::Codex,
                Some("pro"),
                vec![
                    window("five_hour", "5H", 73.2, 192, &[20, 41, 38, 55, 61, 67, 73]),
                    window(
                        "weekly",
                        "WEEKLY",
                        94.7,
                        54 * 60,
                        &[31, 45, 52, 61, 70, 84, 95],
                    ),
                ],
                vec![DetailMetric::provider("reset credits", "1")],
            ),
            account(
                "codex:personal",
                "personal",
                Provider::Codex,
                Some("plus"),
                vec![
                    window("five_hour", "5H", 9.8, 300, &[2, 7, 4, 8, 6, 11, 10]),
                    window("weekly", "WEEKLY", 12.2, 54 * 60, &[3, 5, 7, 8, 10, 11, 12]),
                ],
                Vec::new(),
            ),
            account(
                "grok:work",
                "work",
                Provider::Grok,
                Some("SuperGrok"),
                vec![window(
                    "weekly",
                    "WEEKLY",
                    28.5,
                    102 * 60,
                    &[8, 12, 18, 15, 21, 24, 29],
                )],
                vec![
                    DetailMetric::provider("Grok Build", "24.0%"),
                    DetailMetric::provider("Chat", "4.5%"),
                    DetailMetric::provider("prepaid", "$10.00"),
                ],
            ),
        ],
    }
}

fn account(
    id: &str,
    name: &str,
    provider: Provider,
    plan: Option<&str>,
    windows: Vec<UsageWindow>,
    details: Vec<DetailMetric>,
) -> AccountSnapshot {
    let now = Utc::now();
    AccountSnapshot {
        id: id.into(),
        name: name.into(),
        provider,
        plan: plan.map(str::to_owned),
        windows,
        details,
        health: FetchHealth::ok(),
        fetched_at: now,
        last_success_at: Some(now),
    }
}

fn window(
    key: &str,
    label: &str,
    percent: f64,
    reset_minutes: i64,
    history: &[u64],
) -> UsageWindow {
    let mut window = UsageWindow::new(
        key,
        label,
        percent,
        Some(Utc::now() + Duration::minutes(reset_minutes)),
    );
    window.history = history.to_vec();
    window
}
