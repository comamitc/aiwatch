use std::{process::Command, sync::Arc};

use anyhow::Result;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, watch},
};

use crate::{
    dashboard::{self, AccountCard, MeterTone, ProfileFilter},
    model::{DashboardSnapshot, Provider},
};

const PAGE: &str = include_str!("panel.html");

#[derive(Serialize)]
struct PanelCard {
    title: String,
    account: Option<String>,
    plan: Option<String>,
    auth: &'static str,
    dot: &'static str,
    summary: Option<PanelSummary>,
    meters: Vec<PanelMeter>,
    notice: Option<String>,
}

#[derive(Serialize)]
struct PanelSummary {
    percent: String,
    label: String,
    used: f64,
    pace: Option<f64>,
    empty: String,
}

#[derive(Serialize)]
struct PanelMeter {
    label: String,
    used: f64,
    pace: Option<f64>,
    percent: String,
    time: String,
    tone: &'static str,
}

pub fn cards_json(snapshot: &DashboardSnapshot) -> String {
    let cards = dashboard::account_cards(
        snapshot,
        None,
        ProfileFilter::All,
        false,
        chrono::Utc::now(),
    )
    .iter()
    .map(panel_card)
    .collect::<Vec<_>>();
    serde_json::to_string(&cards).unwrap_or_else(|_| "[]".to_string())
}

fn panel_card(card: &AccountCard<'_>) -> PanelCard {
    PanelCard {
        title: card.title.clone(),
        account: card.account_name.clone(),
        plan: card.plan.clone(),
        auth: card.auth,
        dot: provider_dot(card.account.provider),
        summary: card.summary.as_ref().map(|summary| PanelSummary {
            percent: dashboard::percent_label(summary.percent),
            label: summary.label.clone(),
            used: summary.percent,
            pace: summary.pace,
            empty: match &summary.empty_in {
                Some(remaining) => format!("empty in {remaining}"),
                None => "empty in —".to_string(),
            },
        }),
        meters: card
            .meters
            .iter()
            .map(|meter| PanelMeter {
                label: meter.label.clone(),
                used: meter.used_percent,
                pace: meter.pace,
                percent: dashboard::percent_label(meter.used_percent),
                time: meter.reset.clone(),
                tone: tone_name(meter.tone),
            })
            .collect(),
        notice: card.notice.clone(),
    }
}

fn provider_dot(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "#e8a07c",
        Provider::Codex => "#5eead4",
        Provider::Grok => "#93c5fd",
    }
}

fn tone_name(tone: MeterTone) -> &'static str {
    match tone {
        MeterTone::Session => "session",
        MeterTone::Allowance => "allowance",
        MeterTone::Unknown => "allowance",
    }
}

pub async fn serve(snapshots: watch::Receiver<DashboardSnapshot>) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    println!("aiwatch panel {url}");
    let _ = Command::new("xdg-open").arg(&url).spawn();
    let snapshots = Arc::new(Mutex::new(snapshots));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            accepted = listener.accept() => {
                let (mut socket, _) = accepted?;
                let snapshots = Arc::clone(&snapshots);
                tokio::spawn(async move {
                    let _ = respond(&mut socket, &snapshots).await;
                });
            }
        }
    }
    Ok(())
}

async fn respond(
    socket: &mut tokio::net::TcpStream,
    snapshots: &Mutex<watch::Receiver<DashboardSnapshot>>,
) -> Result<()> {
    let mut buffer = vec![0; 2048];
    let n = socket.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (content_type, body) = if path.starts_with("/api/snapshot") {
        let snapshot = snapshots.lock().await.borrow().clone();
        ("application/json", cards_json(&snapshot).into_bytes())
    } else {
        ("text/html; charset=utf-8", PAGE.as_bytes().to_vec())
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(&body).await?;
    let _ = socket.flush().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccountSnapshot, FetchHealth, UsageWindow};
    use chrono::{Duration, Utc};

    #[test]
    fn claude_card_json_uses_screenshot_fields() {
        let now = Utc::now();
        let mut account = AccountSnapshot::empty(
            "claude:default",
            "default",
            Provider::Claude,
            FetchHealth::ok(),
        );
        account.plan = Some("max".into());
        account.windows = vec![
            UsageWindow::new("five_hour", "5H", 80.0, Some(now + Duration::minutes(162))),
            UsageWindow::new("weekly", "WEEKLY", 21.0, Some(now + Duration::hours(44))),
            UsageWindow::new(
                "weekly_fable",
                "Fable WEEKLY",
                27.0,
                Some(now + Duration::hours(44)),
            ),
        ];
        let snapshot = DashboardSnapshot {
            generated_at: now,
            accounts: vec![account],
        };
        let json = cards_json(&snapshot);
        assert!(json.contains("\"title\":\"claude\""));
        assert!(json.contains("\"plan\":\"max\""));
        assert!(json.contains("\"auth\":\"oauth\""));
        assert!(json.contains("\"tone\":\"session\""));
        assert!(json.contains("\"label\":\"fable\""));
        assert!(json.contains("empty in"));
        assert!(!json.contains("ACCOUNT"));
    }
}
