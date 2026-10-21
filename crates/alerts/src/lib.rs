//! Alert monitoring shared by the desktop application and its background agent.
pub mod delivery;
pub mod launch;
pub mod storage;
#[cfg(test)]
mod tests;

use futures::{StreamExt, stream};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tr_core::{
    Bar, Series, Timeframe,
    price_alerts::{Alert, Book, Event, Observation, Status},
};
use tr_data::Provider;

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TR_DATA_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/terminal-rs")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("terminal-rs")
    }
}

pub fn questrade_handle() -> Option<tr_data::questrade::Questrade> {
    let store = tr_broker::secrets::SecretStore::platform_for(&data_dir(), tr_broker::secrets::QUESTRADE_ACCOUNT);
    let token = store.load_text().unwrap_or(None).or_else(|| std::env::var("QUESTRADE_REFRESH_TOKEN").ok());
    let sink: tr_data::questrade::TokenSink = Box::new(move |token| match token {
        Some(t) => store.save_text(t),
        None => store.clear(),
    });
    tr_data::questrade::Questrade::new(token, sink).ok()
}

pub fn history_is_stale(tf: Timeframe, last: i64, quote: i64) -> bool {
    let boundary = if matches!(tf, Timeframe::MN1 | Timeframe::MN3) {
        tr_core::session::next_bucket(tf, last)
    } else {
        last.saturating_add(tf.seconds())
    };
    last > quote.saturating_add(30) || quote > boundary.saturating_add(120)
}

pub async fn condition_observations(provider: Provider, alerts: Vec<Alert>) -> Vec<(u64, Result<Observation, String>)> {
    let mut groups = BTreeMap::<(String, Timeframe), Vec<Alert>>::new();
    for alert in alerts {
        let tf = match &alert.rule {
            Some(
                tr_core::price_alerts::Rule::Formula { timeframe, .. }
                | tr_core::price_alerts::Rule::Condition { timeframe, .. },
            ) => *timeframe,
            _ => Timeframe::M1,
        };
        groups.entry((alert.symbol.clone(), tf)).or_default().push(alert);
    }
    let batches: Vec<_> = stream::iter(groups)
        .map(|((symbol, tf), alerts)| {
            let provider = provider.clone();
            async move {
                let input: Result<(Observation, Vec<Bar>), String> =
                    tokio::time::timeout(Duration::from_secs(20), async {
                        let q = provider.alert_observation(&symbol).await.map_err(|e| e.to_string())?;
                        if !q.valid_at(chrono::Utc::now().timestamp()) {
                            return Err("Stale price observation".into());
                        }
                        let history = provider.alert_history(&symbol, tf, 1500).await.map_err(|e| e.to_string())?;
                        if history.simulated != q.simulated {
                            return Err("Condition and price data modes differ".into());
                        }
                        let last = history.bars.last().ok_or("No condition history")?;
                        if history_is_stale(tf, last.ts, q.ts) {
                            return Err("Condition history is stale relative to price".into());
                        }
                        Ok((q, history.bars))
                    })
                    .await
                    .unwrap_or_else(|_| Err("Condition request timed out".into()));
                match input {
                    Err(e) => alerts.into_iter().map(|a| (a.id, Err(e.clone()))).collect(),
                    Ok((q, bars)) => {
                        let mut set = tr_core::scanner::SeriesSet::from([(tf, Series::new(symbol.clone(), tf, bars))]);
                        let extras: std::collections::BTreeSet<_> = alerts
                            .iter()
                            .filter_map(|a| a.rule.as_ref())
                            .flat_map(|r| r.timeframes())
                            .filter(|extra| *extra != tf)
                            .collect();
                        let mut errors = std::collections::HashMap::new();
                        for extra in extras {
                            let result = tokio::time::timeout(
                                Duration::from_secs(20),
                                provider.alert_history(&symbol, extra, 1500),
                            )
                            .await;
                            match result {
                                Ok(Ok(history)) if history.simulated == q.simulated => {
                                    set.insert(extra, Series::new(symbol.clone(), extra, history.bars));
                                }
                                result => {
                                    let error = match result {
                                        Ok(Err(e)) => e.to_string(),
                                        Err(_) => "Condition request timed out".into(),
                                        _ => "Condition and price data modes differ".into(),
                                    };
                                    errors.insert(extra, error.to_string());
                                }
                            }
                        }
                        let ids: Vec<_> = alerts.iter().map(|a| a.id).collect();
                        tokio::task::spawn_blocking(move || {
                            alerts
                                .into_iter()
                                .map(|a| {
                                    let result = (|| {
                                        let rule = a.rule.as_ref().ok_or("Missing condition")?;
                                        let mut observation = q.clone();
                                        for required in rule.timeframes() {
                                            if let Some(error) = errors.get(&required) {
                                                return Err(error.clone());
                                            }
                                        }
                                        observation.price = match rule {
                                            tr_core::price_alerts::Rule::Condition { .. } => rule.value_set(&set)?,
                                            _ => rule.value(&set[&tf].bars)?,
                                        };
                                        observation.source = format!(
                                            "{} · {}",
                                            q.source,
                                            rule.label().chars().take(200).collect::<String>()
                                        );
                                        Ok(observation)
                                    })();
                                    (a.id, result)
                                })
                                .collect::<Vec<_>>()
                        })
                        .await
                        .unwrap_or_else(|_| {
                            ids.into_iter().map(|id| (id, Err("Condition evaluation failed".into()))).collect()
                        })
                    }
                }
            }
        })
        .buffer_unordered(4)
        .collect()
        .await;
    batches.into_iter().flatten().collect()
}

pub async fn price_observations(
    provider: Provider,
    symbols: Vec<String>,
) -> Vec<(String, Result<Observation, String>)> {
    stream::iter(symbols)
        .map(|symbol| {
            let provider = provider.clone();
            async move {
                let result =
                    match tokio::time::timeout(Duration::from_secs(20), provider.alert_observation(&symbol)).await {
                        Ok(result) => result.map_err(|e| format!("{e:#}")),
                        Err(_) => Err("Price request timed out; will retry".into()),
                    };
                (symbol, result)
            }
        })
        .buffer_unordered(4)
        .collect()
        .await
}

pub fn merge_price(
    book: &mut Book,
    symbol: &str,
    result: Result<Observation, String>,
    simulated: bool,
    now: i64,
) -> Vec<Event> {
    match result {
        Ok(q) if q.symbol == symbol && q.simulated == simulated && q.valid_at(now) => book.observe(q, now),
        _ => {
            book.reset_symbol(symbol);
            vec![]
        }
    }
}
pub fn merge_rule(
    book: &mut Book,
    id: u64,
    result: Result<Observation, String>,
    simulated: bool,
    now: i64,
) -> Vec<Event> {
    match result {
        Ok(q) if q.simulated == simulated && q.valid_at(now) => book.observe_rule(id, q, now),
        _ => {
            book.reset_rule(id);
            vec![]
        }
    }
}

/// Runtime baselines survive cycles, but never survive a restart or an app handoff.
#[derive(Default)]
pub struct Monitor {
    cached: Option<Book>,
}
impl Monitor {
    pub async fn cycle(&mut self, dir: &std::path::Path, provider: Provider) -> Result<Vec<Event>, String> {
        self.cycle_with(dir, || provider).await
    }

    /// Holds the app gate across provider setup and the entire transaction.
    /// No credentials are loaded or provider clients constructed while the app owns the gate.
    pub async fn cycle_with(
        &mut self,
        dir: &std::path::Path,
        provider: impl FnOnce() -> Provider,
    ) -> Result<Vec<Event>, String> {
        let Some(_gate) = storage::Lock::try_acquire(&dir.join("app.lock")).map_err(|e| e.to_string())? else {
            self.cached = None;
            return Ok(vec![]);
        };
        let path = dir.join("price-alerts.json");
        let _book_lock = storage::Lock::acquire(&dir.join("price-alerts.lock")).map_err(|e| e.to_string())?;
        let loaded = storage::load(&path)?;
        let mut book = match self.cached.take() {
            Some(cached) if serde_json::to_value(&cached).ok() == serde_json::to_value(&loaded).ok() => cached,
            _ => loaded,
        };
        let provider = provider();
        let mut events = Vec::new();
        let armed: Vec<_> = book.alerts.iter().filter(|a| a.status == Status::Armed).cloned().collect();
        let symbols = armed
            .iter()
            .filter(|a| a.rule.is_none())
            .map(|a| a.symbol.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for (symbol, result) in price_observations(provider.clone(), symbols).await {
            // Book::observe matches each alert's data mode. Fetch once per symbol so a
            // waiting sample alert cannot reset a live alert's baseline (or vice versa).
            let simulated = result.as_ref().is_ok_and(|q| q.simulated);
            events.extend(merge_price(&mut book, &symbol, result, simulated, chrono::Utc::now().timestamp()));
        }
        for (id, result) in
            condition_observations(provider, armed.into_iter().filter(|a| a.rule.is_some()).collect()).await
        {
            if let Some(simulated) = book.alerts.iter().find(|a| a.id == id).map(|a| a.simulated) {
                events.extend(merge_rule(&mut book, id, result, simulated, chrono::Utc::now().timestamp()));
            }
        }
        storage::save_locked(&path, &book)?;
        self.cached = Some(book);
        Ok(events)
    }
}
