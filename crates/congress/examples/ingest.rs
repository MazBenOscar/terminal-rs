//! Run one live refresh against the official sources into a SQLite file.
//! `cargo run -p tr-congress --example ingest -- --i-know congress.sqlite3 [max_new_per_chamber]`
//! Without --i-know this exits without opening a database or contacting any source.
use tokio::sync::Mutex;
use tr_congress::Db;
use tr_congress::refresh::{HttpSource, RefreshConfig, refresh};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info,tr_congress=debug").init();
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--i-know") {
        eprintln!(
            "WARNING: ingest contacts live Congress sites. Dry run: no requests or database changes.\n\
            To opt in: ingest --i-know <app-congress-db> [max_new_per_chamber].\n\
            Use the app's database to share its persisted source cooldowns."
        );
        return Ok(());
    }
    let minutes = tr_congress::refresh::refresh_minutes(
        std::env::var("TR_PROVIDER").ok().as_deref(),
        std::env::var("TR_CONGRESS_REFRESH_MIN").ok().as_deref(),
    );
    if minutes == 0 {
        println!("Congress refresh disabled");
        return Ok(());
    }
    let net = tr_net::shared()?;
    net.check("house-disclosures")?;
    if !net.claim_slot("congress:refresh", std::time::Duration::from_secs(minutes * 60))? {
        println!("Congress refresh slot already claimed; no requests");
        return Ok(());
    }
    let path = args.next().unwrap_or_else(|| "congress.sqlite3".into());
    let db = Db::open(&path)?;
    let mut cfg = RefreshConfig::incremental(&db);
    if let Some(n) = args.next() {
        cfg.max_new_per_chamber = n.parse()?;
    }
    let db = Mutex::new(db);
    let report = refresh(&HttpSource::new()?, &db, &cfg).await?;
    println!("{}", report.summary());
    for e in report.errors.iter().take(10) {
        println!("  error: {e}");
    }
    let db = db.lock().await;
    println!("{:?}", db.stats()?);
    for t in db.latest_trades(15)? {
        println!(
            "{} {:<24} {} {:<6} {:<5} {:<12} tx {} filed {} lag {}d",
            t.chamber.as_str(),
            t.politician,
            t.party.code(),
            t.ticker.as_deref().unwrap_or("-"),
            t.tx_type.marker(),
            t.amount.map(|a| a.to_string()).unwrap_or_default(),
            t.tx_date,
            t.filed_date,
            t.disclosure_lag_days()
        );
    }
    Ok(())
}
