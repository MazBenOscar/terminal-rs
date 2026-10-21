//! Read-only portfolio annotations. Derivatives are never plotted on their underlying's
//! price axis, and accounts/currencies are kept separate rather than netted together.
use crate::theme::{self, mono, muted};
use egui::{Align2, Rect, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use tr_broker::{InstrumentKind, Portfolio};
use tr_chart::viewport::PriceScale;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub positions: bool,
    pub orders: bool,
    pub account: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self { positions: true, orders: true, account: None }
    }
}
#[derive(Debug, Clone)]
pub struct Level {
    pub price: f64,
    pub label: String,
    pub position: Option<(f64, f64, String)>,
}
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub accounts: Vec<(String, String)>,
    pub levels: Vec<Level>,
    pub status: String,
    pub chart_mark: bool,
}
impl Snapshot {
    pub fn build(
        pf: Option<&Portfolio>,
        symbol: &str,
        settings: &Settings,
        demo: bool,
        error: Option<&str>,
        sample_chart: bool,
    ) -> Self {
        let Some(pf) = pf else {
            return Self {
                status: if demo {
                    "DEMO · no portfolio".into()
                } else {
                    format!("Account overlays · {}", error.unwrap_or("connect a broker in PORT"))
                },
                ..Default::default()
            };
        };
        let mode = if pf.demo {
            "DEMO"
        } else if pf.simulated {
            "SAMPLE"
        } else {
            "BROKER"
        };
        let mut out =
            Self { accounts: pf.accounts.iter().map(|a| (a.id.clone(), a.label())).collect(), ..Default::default() };
        let age = pf.fetched.map(|t| (chrono::Utc::now() - t).num_seconds());
        out.status = format!(
            "{mode} overlays{}{}",
            if age.is_none_or(|age| age > 300) { " · snapshot stale/undated" } else { "" },
            if error.is_some() { " · refresh failed" } else { "" }
        );
        let account = |id: &str| settings.account.as_deref().is_none_or(|wanted| wanted == id);
        out.chart_mark = pf.demo || pf.simulated || !sample_chart;
        if !out.chart_mark {
            out.status.push_str(" · P/L hidden on sample charts");
        }
        let label = |id: &str| {
            pf.accounts.iter().find(|a| a.id == id).map(|a| a.label()).unwrap_or_else(|| "Unknown account".into())
        };
        if settings.positions {
            for p in &pf.positions {
                if !account(&p.account_id)
                    || !matches!(
                        p.kind,
                        InstrumentKind::Stock | InstrumentKind::Etf | InstrumentKind::Fund | InstrumentKind::Crypto
                    )
                    || p.market_key().as_deref() != Some(symbol)
                    || p.units == 0.
                    || !p.units.is_finite()
                    || !p.multiplier.is_finite()
                {
                    continue;
                }
                // US share charts use USD, never an account's converted cost basis.
                if p.kind != InstrumentKind::Crypto && p.currency != "USD" {
                    continue;
                }
                if let Some(cost) = p.cost_basis.filter(|x| x.is_finite()) {
                    out.levels.push(Level {
                        price: cost,
                        label: format!("{mode} {} · {} {} @ {}", label(&p.account_id), p.units, p.symbol, cost),
                        position: Some((p.units, p.multiplier, p.currency.clone())),
                    });
                }
            }
        }
        if settings.orders {
            for o in &pf.orders {
                if !account(&o.account_id)
                    || !o.is_open()
                    || !matches!(
                        o.kind,
                        InstrumentKind::Stock | InstrumentKind::Etf | InstrumentKind::Fund | InstrumentKind::Crypto
                    )
                    || o.market_key().as_deref() != Some(symbol)
                {
                    continue;
                }
                let remaining = o.quantity.zip(o.filled.or(Some(0.))).map(|(qty, filled)| (qty - filled).max(0.));
                if remaining.is_some_and(|n| n <= 0.) {
                    continue;
                }
                for (kind, price) in [("Limit", o.limit_price), ("Stop", o.stop_price)] {
                    if let Some(price) = price.filter(|p| p.is_finite()) {
                        out.levels.push(Level {
                            price,
                            label: format!(
                                "{mode} {} · {} {} {kind} {} · {}",
                                label(&o.account_id),
                                o.action,
                                remaining.map_or("?".into(), |q| q.to_string()),
                                price,
                                o.status
                            ),
                            position: None,
                        });
                    }
                }
            }
        }
        out
    }
    pub fn menu(&self, ui: &mut Ui, settings: &mut Settings) {
        ui.checkbox(&mut settings.positions, "Position average cost and P/L");
        ui.checkbox(&mut settings.orders, "Working limit and stop orders");
        egui::ComboBox::from_id_salt("chart-overlay-account")
            .selected_text(
                settings
                    .account
                    .as_ref()
                    .and_then(|id| self.accounts.iter().find(|(a, _)| a == id).map(|(_, name)| name.as_str()))
                    .unwrap_or(if settings.account.is_some() { "Unavailable account" } else { "All accounts" }),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut settings.account, None, "All accounts");
                for (id, name) in &self.accounts {
                    ui.selectable_value(&mut settings.account, Some(id.clone()), name);
                }
            });
        ui.label(&self.status);
        ui.label("Read-only. P/L uses the chart's latest loaded price; data can be delayed.");
    }
    pub fn paint(&self, ui: &Ui, plot: Rect, scale: &PriceScale, mark: Option<f64>) {
        let p = ui.painter_at(plot);
        let mut last_label = f32::NEG_INFINITY;
        let mut levels: Vec<_> = self
            .levels
            .iter()
            .filter_map(|l| {
                if scale.log && l.price <= 0. {
                    return None;
                }
                let y = plot.top() + scale.y_of(l.price) as f32;
                (y.is_finite() && plot.y_range().contains(y)).then_some((y, l))
            })
            .collect();
        levels.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (y, l) in levels {
            let pnl = l.position.as_ref().and_then(|(q, m, c)| {
                mark.filter(|v| v.is_finite() && self.chart_mark).map(|v| ((v - l.price) * q * m, c))
            });
            let color = pnl.map_or(theme::series(4), |(p, _)| if p >= 0. { theme::up() } else { theme::down() });
            let mut x = plot.left();
            while x < plot.right() {
                p.hline(x..=(x + 8.).min(plot.right()), y, Stroke::new(1., color.gamma_multiply(0.65)));
                x += 14.;
            }
            let msg = match pnl {
                Some((p, c)) => format!("{} · chart P/L {p:+.2} {c}", l.label),
                None => l.label.clone(),
            };
            let galley = p.layout(msg, mono(10.), color, plot.width().min(480.));
            let top = (y - galley.size().y - 2.).max(last_label).max(plot.top());
            let r = Rect::from_min_size(pos2(plot.left() + 6., top), galley.size() + vec2(6., 2.));
            p.rect_filled(r, 1., theme::card());
            p.galley(r.min + vec2(3., 0.), galley, color);
            last_label = r.bottom() + 2.;
        }
        crate::chart_view::label_text(
            &p,
            plot.left_bottom() + vec2(6., -3.),
            Align2::LEFT_BOTTOM,
            &self.status,
            mono(9.5),
            muted(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_accounts_skips_derivatives_and_closed_orders_and_preserves_short_sign() {
        let mut pf = tr_broker::sample::portfolio();
        let mut p = pf.positions.iter().find(|p| p.kind == InstrumentKind::Stock).unwrap().clone();
        p.symbol = "AAPL".into();
        p.underlying = None;
        p.units = -10.;
        p.cost_basis = Some(150.);
        p.currency = "USD".into();
        let symbol = p.market_key().unwrap();
        pf.positions = vec![p.clone()];
        let mut option = p.clone();
        option.kind = InstrumentKind::Option;
        option.underlying = Some("AAPL".into());
        pf.positions.push(option);
        let mut foreign = p.clone();
        foreign.currency = "CAD".into();
        pf.positions.push(foreign);
        let snap =
            Snapshot::build(Some(&pf), &symbol, &Settings { orders: false, ..Default::default() }, false, None, false);
        assert_eq!(snap.levels.len(), 1);
        assert_eq!(snap.levels[0].position.as_ref().unwrap().0, -10.);
        let settings = Settings { account: Some("absent".into()), ..Default::default() };
        assert!(Snapshot::build(Some(&pf), &symbol, &settings, false, None, false).levels.is_empty());
    }
    #[test]
    fn stop_limit_remaining_quantities_and_sample_chart_marks() {
        let mut pf = tr_broker::sample::portfolio();
        let mut order = pf.orders[0].clone();
        order.symbol = "AAPL".into();
        order.kind = InstrumentKind::Stock;
        order.underlying = None;
        order.quantity = Some(50.);
        order.filled = Some(10.);
        order.limit_price = Some(101.);
        order.stop_price = Some(99.);
        order.status = "PARTIAL".into();
        pf.orders = vec![order];
        pf.simulated = false;
        pf.demo = false;
        let settings = Settings { positions: false, ..Default::default() };
        let snap = Snapshot::build(Some(&pf), "AAPL US Equity", &settings, false, None, true);
        assert_eq!(snap.levels.len(), 2);
        assert!(!snap.chart_mark);
        assert!(snap.levels.iter().all(|l| l.label.contains("40")));
        pf.orders[0].status = "CANCELED".into();
        assert!(Snapshot::build(Some(&pf), "AAPL US Equity", &settings, false, None, false).levels.is_empty());
    }
}
