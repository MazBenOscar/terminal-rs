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
            let result = self.connection(&url, &key, &secret).await;
            if result.is_err() {
                // Never include server payloads or authentication material in logs/status.
                self.status("Alpaca minute stream unavailable; reconnecting. Check feed entitlement/connection limits and refresh history after a gap.");
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(60);
            } else {
                delay = 1;
            }
        }
    }
    async fn connection(&self, url: &str, key: &str, secret: &str) -> Result<(), ()> {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        let auth = serde_json::json!({"action":"auth","key":key,"secret":secret});
        socket.send(Message::Text(auth.to_string().into())).await.map_err(|_| ())?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(message) = socket.next().await {
                if let Message::Text(text) = message.map_err(|_| ())? {
                    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|_| ())?;
                    if rows.iter().any(|r| r["T"] == "error") {
                        return Err(());
                    }
                    if rows.iter().any(|r| r["T"] == "success" && r["msg"] == "authenticated") {
                        return Ok(());
                    }
                }
            }
            Err(())
        })
        .await
        .map_err(|_| ())??;
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
                    socket.send(Message::Text(request.to_string().into())).await.map_err(|_| ())?;
                }
            }
            subscribed = symbols;
            tokio::select! {
                _ = self.changed.notified() => {},
                _ = heartbeat.tick() => {
                    if received.elapsed() > Duration::from_secs(90) { return Err(()); }
                    socket.send(Message::Ping(Vec::new().into())).await.map_err(|_|())?;
                }
                message = socket.next() => {
                    received = std::time::Instant::now();
                    match message.ok_or(())?.map_err(|_|())? {
                        Message::Text(text) => {
                            let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|_|())?;
                            for row in rows {
                                if row["T"]=="error" { return Err(()); }
                                if let Some((symbol,bar)) = parse_bar(&row)
                                    && let Ok(channels) = self.channels.lock()
                                    && let Some(tx) = channels.get(&symbol) { let _ = tx.send(LiveUpdate::Snapshot(bar)); }
                            }
                        }
                        Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await.map_err(|_|())?; }
                        Message::Close(_) => return Err(()),
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
        let connection = tokio::spawn(async move { hub.connection(&url, "fixture-key", "fixture-secret").await });
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
