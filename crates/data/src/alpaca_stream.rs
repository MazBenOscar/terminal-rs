//! One authenticated stock-minute connection shared by all chart subscribers.
//! Opt-in until credentials/feed entitlement have been verified by the user.
use crate::{LiveFeed, LiveUpdate};
use futures::{SinkExt, StreamExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tr_core::Bar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Retry,
    Fatal(u16),
}
fn classify(code: u16) -> Failure {
    if matches!(code, 401 | 402 | 403 | 406 | 409) { Failure::Fatal(code) } else { Failure::Retry }
}
fn next_delay(delay: u64) -> u64 {
    (delay * 2).min(60)
}
fn server_error(row: &serde_json::Value, authenticating: bool) -> Failure {
    let code = row["code"].as_u64().and_then(|code| u16::try_from(code).ok()).unwrap_or(0);
    if authenticating { Failure::Fatal(code) } else { classify(code) }
}

#[derive(Default)]
pub struct Hub {
    channels: Mutex<BTreeMap<String, broadcast::Sender<LiveUpdate>>>,
    changed: Notify,
    started: AtomicBool,
}
impl Hub {
    pub fn subscribe(self: &Arc<Self>, symbol: &str, key: String, secret: String, feed: String) -> Option<LiveFeed> {
        if !matches!(feed.as_str(), "iex" | "sip" | "delayed_sip") {
            return None;
        }
        let mut channels = self.channels.lock().ok()?;
        if channels.len() >= 64 && !channels.contains_key(symbol) {
            return None;
        }
        let mut rx = channels.entry(symbol.into()).or_insert_with(|| broadcast::channel(512).0).subscribe();
        drop(channels);
        let (tx, out) = mpsc::channel(256);
        let hub = self.clone();
        let symbol = symbol.to_string();
        tokio::spawn(async move {
            let _ =
                tx.send(LiveUpdate::Status("Waiting for Alpaca minute bars; feed entitlement required".into())).await;
            loop {
                tokio::select! {
                    _ = tx.closed() => break,
                    result = rx.recv() => match result {
                        Ok(update) => { if tx.send(update).await.is_err() { break; } }
                        Err(broadcast::error::RecvError::Lagged(_)) => { let _ = tx.send(LiveUpdate::Status("Stream consumer lagged; refresh history to repair missed bars".into())).await; }
                        Err(_) => break,
                    }
                }
            }
            drop(rx);
            if let Ok(mut channels) = hub.channels.lock()
                && channels.get(&symbol).is_some_and(|tx| tx.receiver_count() == 0)
            {
                channels.remove(&symbol);
            }
            hub.changed.notify_one();
        });
        self.changed.notify_one();
        if !self.started.swap(true, Ordering::SeqCst) {
            let hub = self.clone();
            tokio::spawn(async move {
                hub.run(key, secret, feed).await;
            });
        }
        Some(out)
    }
    fn symbols(&self) -> BTreeSet<String> {
        self.channels.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }
    fn status(&self, message: &str) {
        if let Ok(channels) = self.channels.lock() {
            for tx in channels.values() {
                let _ = tx.send(LiveUpdate::Status(message.into()));
            }
        }
    }
    async fn run(self: Arc<Self>, key: String, secret: String, feed: String) {
        let url = format!("wss://stream.data.alpaca.markets/v2/{feed}");
        let mut delay = 1;
        loop {
            if self.symbols().is_empty() {
                self.changed.notified().await;
                continue;
            }
            match self.connection(&url, &key, &secret, &mut delay).await {
                Err(Failure::Fatal(code)) => {
                    self.status(&format!("Alpaca minute stream permanent error (code {code}); retries stopped. Check authentication, subscription and connection limits."));
                    return;
                }
                Err(Failure::Retry) => {
                    // Never include server payloads or authentication material in logs/status.
                    self.status("Alpaca minute stream unavailable; reconnecting. Refresh history after a gap.");
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay = next_delay(delay);
                }
                Ok(()) => delay = 1,
            }
        }
    }
    async fn connection(&self, url: &str, key: &str, secret: &str, delay: &mut u64) -> Result<(), Failure> {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| Failure::Retry)?
            .map_err(|error| match error {
                tokio_tungstenite::tungstenite::Error::Http(response) => classify(response.status().as_u16()),
                _ => Failure::Retry,
            })?;
        let auth = serde_json::json!({"action":"auth","key":key,"secret":secret});
        socket.send(Message::Text(auth.to_string().into())).await.map_err(|_| Failure::Retry)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(message) = socket.next().await {
                if let Message::Text(text) = message.map_err(|_| Failure::Retry)? {
                    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|_| Failure::Retry)?;
                    if let Some(row) = rows.iter().find(|r| r["T"] == "error") {
                        return Err(server_error(row, true));
                    }
                    if rows.iter().any(|r| r["T"] == "success" && r["msg"] == "authenticated") {
                        return Ok(());
                    }
                }
            }
            Err(Failure::Retry)
        })
        .await
        .map_err(|_| Failure::Retry)??;
        *delay = 1;
        self.status("Alpaca minute stream authenticated; waiting for minute bars");
        let mut subscribed = BTreeSet::<String>::new();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        let mut received = std::time::Instant::now();
        loop {
            let symbols = self.symbols();
            if symbols.is_empty() {
                let _ = socket.close(None).await;
                return Ok(());
            }
            let added: Vec<_> = symbols.difference(&subscribed).cloned().collect();
            let removed: Vec<_> = subscribed.difference(&symbols).cloned().collect();
            for (action, list) in [("subscribe", added), ("unsubscribe", removed)] {
                if !list.is_empty() {
                    let request = serde_json::json!({"action":action,"bars":list,"updatedBars":list});
                    socket.send(Message::Text(request.to_string().into())).await.map_err(|_| Failure::Retry)?;
                }
            }
            subscribed = symbols;
            tokio::select! {
                _ = self.changed.notified() => {},
                _ = heartbeat.tick() => {
                    if received.elapsed() > Duration::from_secs(90) { return Err(Failure::Retry); }
                    socket.send(Message::Ping(Vec::new().into())).await.map_err(|_| Failure::Retry)?;
                }
                message = socket.next() => {
                    received = std::time::Instant::now();
                    match message.ok_or(Failure::Retry)?.map_err(|_| Failure::Retry)? {
                        Message::Text(text) => {
                            let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|_| Failure::Retry)?;
                            for row in rows {
                                if row["T"]=="error" { return Err(server_error(&row, false)); }
                                if let Some((symbol,bar)) = parse_bar(&row)
                                    && let Ok(channels) = self.channels.lock()
                                    && let Some(tx) = channels.get(&symbol) { let _ = tx.send(LiveUpdate::Snapshot(bar)); }
                            }
                        }
                        Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await.map_err(|_| Failure::Retry)?; }
                        Message::Close(_) => return Err(Failure::Retry),
                        _ => {},
                    }
                }
            }
        }
    }
}
fn parse_bar(value: &serde_json::Value) -> Option<(String, Bar)> {
    if !matches!(value["T"].as_str()?, "b" | "u") {
        return None;
    }
    let symbol = value["S"].as_str()?.to_string();
    if symbol.len() > 32 {
        return None;
    }
    let ts = chrono::DateTime::parse_from_rfc3339(value["t"].as_str()?).ok()?.timestamp();
    let bar = Bar::new(
        ts,
        value["o"].as_f64()?,
        value["h"].as_f64()?,
        value["l"].as_f64()?,
        value["c"].as_f64()?,
        value["v"].as_f64()?,
    );
    if ts <= 0
        || ts % 60 != 0
        || ![bar.open, bar.high, bar.low, bar.close, bar.volume].iter().all(|v| v.is_finite())
        || bar.volume < 0.
        || bar.low > bar.high
        || bar.open < bar.low
        || bar.open > bar.high
        || bar.close < bar.low
        || bar.close > bar.high
    {
        return None;
    }
    Some((symbol, bar))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permanent_failures_stop_retries_and_transient_backoff_is_bounded() {
        for code in [401, 402, 403, 406, 409] {
            assert_eq!(classify(code), Failure::Fatal(code));
            assert_eq!(server_error(&serde_json::json!({"code":code}), false), Failure::Fatal(code));
        }
        assert_eq!(classify(500), Failure::Retry);
        assert_eq!(server_error(&serde_json::json!({"code":400}), true), Failure::Fatal(400));
        assert_eq!(next_delay(1), 2);
        assert_eq!(next_delay(32), 60);
        assert_eq!(next_delay(60), 60);
    }
    #[tokio::test]
    async fn one_connection_authenticates_and_routes_multiple_symbols_without_cross_talk() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let hub = Arc::new(Hub::default());
        let (a, mut a_rx) = broadcast::channel(16);
        let (b, mut b_rx) = broadcast::channel(16);
        hub.channels.lock().unwrap().extend([("AAA".into(), a), ("BBB".into(), b)]);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(r#"[{"T":"success","msg":"connected"}]"#.into())).await.unwrap();
            let Message::Text(auth) = ws.next().await.unwrap().unwrap() else { panic!("auth required") };
            let auth: serde_json::Value = serde_json::from_str(&auth).unwrap();
            assert_eq!(auth["action"], "auth");
            assert_eq!(auth["key"], "fixture-key");
            ws.send(Message::Text(r#"[{"T":"success","msg":"authenticated"}]"#.into())).await.unwrap();
            let Message::Text(sub) = ws.next().await.unwrap().unwrap() else { panic!("subscription required") };
            let sub: serde_json::Value = serde_json::from_str(&sub).unwrap();
            assert_eq!(sub["bars"], serde_json::json!(["AAA", "BBB"]));
            assert_eq!(sub["updatedBars"], sub["bars"]);
            ws.send(Message::Text(
                r#"[{"T":"b","S":"AAA","t":"2026-10-20T14:30:00Z","o":100,"h":102,"l":99,"c":101,"v":300}]"#.into(),
            ))
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = ws.close(None).await;
        });
        let connection = tokio::spawn(async move {
            let mut delay = 60;
            let result = hub.connection(&url, "fixture-key", "fixture-secret", &mut delay).await;
            assert_eq!(delay, 1);
            result
        });
        let status = tokio::time::timeout(Duration::from_secs(3), a_rx.recv()).await.unwrap().unwrap();
        assert!(
            matches!(status, LiveUpdate::Status(message) if message.contains("authenticated") && !message.contains("reconnecting"))
        );
        assert!(matches!(b_rx.recv().await.unwrap(), LiveUpdate::Status(_)));
        let update = tokio::time::timeout(Duration::from_secs(3), a_rx.recv()).await.unwrap().unwrap();
        assert!(matches!(update,LiveUpdate::Snapshot(bar) if bar.close==101.));
        assert!(b_rx.try_recv().is_err());
        server.await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(3), connection).await.unwrap().unwrap().is_err());
    }
    #[test]
    fn parses_only_complete_minute_snapshots_and_corrections() {
        let mut row = serde_json::json!({"T":"b","S":"AAPL","t":"2026-10-20T14:30:00Z","o":100.,"h":102.,"l":99.,"c":101.,"v":300});
        let (symbol, bar) = parse_bar(&row).unwrap();
        assert_eq!(symbol, "AAPL");
        assert_eq!(bar.volume, 300.);
        row["T"] = "u".into();
        assert!(parse_bar(&row).is_some());
        row["T"] = "t".into();
        assert!(parse_bar(&row).is_none());
        row["T"] = "b".into();
        row["c"] = 200.into();
        assert!(parse_bar(&row).is_none());
    }
}
