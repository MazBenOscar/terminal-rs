use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use tr_core::{Bar, Timeframe, crypto::CryptoInstrument, session::Session};
use tr_data::{MarketData, Quote, SearchHit};

use crate::{ToolResult, args::LiveArgs};

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceBasis {
    Adjusted,
    Unadjusted,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct History {
    pub bars: Vec<Bar>,
    pub price_basis: PriceBasis,
    pub session: Session,
    /// Authoritative source identity; do not infer a venue from an equity ticker.
    pub instrument: Option<CryptoInstrument>,
}

pub trait DataHost: Send + Sync {
    fn history<'a>(&'a self, symbol: &'a str, tf: Timeframe, end: Option<i64>, limit: usize)
    -> HostFuture<'a, History>;
    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>>;
    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>>;
}

/// `chart_id = None` means the host's pinned chart, not whichever pane currently has focus.
/// Hosts must check loaded/future drawing anchors, stale ids and confirmation/consent at
/// execution time, and report linked panes moved by set_view. Queue deadlines belong to the host.
pub trait LiveHost: Send + Sync {
    fn call_live<'a>(&'a self, name: &'a str, args: LiveArgs) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
}

/// Bridge for workers using the existing market-data interface. Price basis is explicit:
/// MarketData::history itself does not expose it. Unknown is safer than assuming adjusted.
pub struct MarketDataHost<M> {
    pub market: M,
    pub price_basis: PriceBasis,
    pub session: Session,
}

impl<M: MarketData> DataHost for MarketDataHost<M> {
    fn history<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
    ) -> HostFuture<'a, History> {
        Box::pin(async move {
            let bars = self.market.history(symbol, tf, end, limit).await.map_err(|e| e.to_string())?;
            Ok(History {
                bars,
                price_basis: self.price_basis,
                session: self.session,
                instrument: self.market.crypto_instrument(symbol),
            })
        })
    }

    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>> {
        Box::pin(async move { self.market.search(query).await.map_err(|e| e.to_string()) })
    }

    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>> {
        Box::pin(async move { self.market.quotes(symbols).await.map_err(|e| e.to_string()) })
    }
}
