//! Estimated performance of a politician's disclosed trades.
//!
//! Disclosures give only a value band, so each trade is weighted by the band
//! midpoint. A purchase is scored by the price move from the transaction date
//! to `now`; a sale by the move avoided (the negated return). This is an
//! estimate of stock-picking skill, not a reconstruction of a portfolio.

use chrono::NaiveDate;

use crate::model::Trade;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradeReturn {
    pub trade_id: i64,
    /// Signed return in the trade's direction (buys long, sells short).
    pub ret: f64,
    pub weight: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Performance {
    pub scored: usize,
    pub unscored: usize,
    /// Amount-weighted mean directional return.
    pub weighted_return: f64,
    /// Fraction of scored trades with a positive directional return.
    pub hit_rate: f64,
    pub avg_lag_days: f64,
}

/// `price_at(ticker, date)` returns the close on or before `date`;
/// `latest(ticker)` the most recent close.
pub fn estimate(
    trades: &[Trade],
    mut price_at: impl FnMut(&str, NaiveDate) -> Option<f64>,
    mut latest: impl FnMut(&str) -> Option<f64>,
) -> (Performance, Vec<TradeReturn>) {
    let mut out = Vec::new();
    let mut unscored = 0;
    for t in trades {
        let dir = if t.tx_type.is_buy() {
            1.0
        } else if t.tx_type.is_sell() {
            -1.0
        } else {
            unscored += 1;
            continue;
        };
        let Some(tk) = t.ticker.as_deref() else {
            unscored += 1;
            continue;
        };
        match (price_at(tk, t.tx_date), latest(tk)) {
            (Some(p0), Some(p1)) if p0 > 0.0 => out.push(TradeReturn {
                trade_id: t.id,
                ret: dir * (p1 / p0 - 1.0),
                weight: t.amount.map(|a| a.midpoint()).unwrap_or(1.0),
            }),
            _ => unscored += 1,
        }
    }
    let wsum: f64 = out.iter().map(|r| r.weight).sum();
    let perf = Performance {
        scored: out.len(),
        unscored,
        weighted_return: if wsum > 0.0 { out.iter().map(|r| r.ret * r.weight).sum::<f64>() / wsum } else { 0.0 },
        hit_rate: if out.is_empty() {
            0.0
        } else {
            out.iter().filter(|r| r.ret > 0.0).count() as f64 / out.len() as f64
        },
        avg_lag_days: if trades.is_empty() {
            0.0
        } else {
            trades.iter().map(|t| t.disclosure_lag_days() as f64).sum::<f64>() / trades.len() as f64
        },
    };
    (perf, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn t(id: i64, tk: &str, tx: TxType, amt: &str) -> Trade {
        Trade {
            id,
            filing_id: "f".into(),
            politician_id: 1,
            politician: "X".into(),
            party: Party::Democrat,
            chamber: Chamber::House,
            state: None,
            ticker: Some(tk.into()),
            asset_name: tk.into(),
            asset_type: None,
            owner: None,
            tx_type: tx,
            amount: AmountRange::parse(amt),
            tx_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            filed_date: NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            source_url: String::new(),
            source: String::new(),
            amount_range: None,
        }
    }

    #[test]
    fn weighted_directional_returns() {
        let trades = [
            t(1, "UP", TxType::Purchase, "$1,001 - $15,000"),
            t(2, "DOWN", TxType::Sale, "$15,001 - $50,000"),
            t(3, "NOPE", TxType::Purchase, "$1,001 - $15,000"),
        ];
        let (p, rs) = estimate(
            &trades,
            |tk, _| match tk {
                "UP" | "DOWN" => Some(100.0),
                _ => None,
            },
            |tk| match tk {
                "UP" => Some(110.0),
                "DOWN" => Some(80.0),
                _ => None,
            },
        );
        assert_eq!(p.scored, 2);
        assert_eq!(p.unscored, 1);
        assert!((rs[0].ret - 0.10).abs() < 1e-9);
        assert!((rs[1].ret - 0.20).abs() < 1e-9);
        assert_eq!(p.hit_rate, 1.0);
        let w = (0.10 * 8000.5 + 0.20 * 32500.5) / (8000.5 + 32500.5);
        assert!((p.weighted_return - w).abs() < 1e-9);
        assert_eq!(p.avg_lag_days, 30.0);
    }
}
