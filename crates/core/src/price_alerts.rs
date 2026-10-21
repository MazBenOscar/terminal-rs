//! Sampled price crossings with optional cooldown rearming. Runtime baselines are deliberately not persisted:
//! reopening, editing or rearming an alert starts with a fresh observation.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::scanner::{ConditionGroup, MatchMode, ScanDefinition, ScanOutcome, SeriesSet};

pub const MAX_ALERTS: usize = 200;
pub const MAX_EVENTS: usize = 500;
pub const MAX_SYMBOLS: usize = 32;
/// Allow delayed feeds, but never silently treat yesterday's close as a live price.
pub const MAX_QUOTE_AGE: i64 = 20 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Above,
    Below,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Above => "Crosses above",
            Self::Below => "Crosses below",
        }
    }
    fn crosses(self, before: f64, after: f64, level: f64) -> bool {
        match self {
            Self::Above => before < level && after >= level,
            Self::Below => before > level && after <= level,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Armed,
    Disabled,
    Triggered,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Once,
    Rearm {
        cooldown_secs: u32,
    },
}

impl Repeat {
    pub fn validate(self) -> Result<(), String> {
        if matches!(self, Self::Rearm { cooldown_secs } if !(60..=86_400).contains(&cooldown_secs)) {
            return Err("Cooldown must be between 1 and 1440 minutes".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: u64,
    pub symbol: String,
    pub level: f64,
    pub direction: Direction,
    pub status: Status,
    /// Sample alerts can only be evaluated against explicitly simulated data.
    pub simulated: bool,
    #[serde(default)]
    pub rule: Option<Rule>,
    #[serde(default)]
    pub repeat: Repeat,
    #[serde(default)]
    pub last_fired: Option<i64>,
}

/// Formula values and line-distance values have their own units, not market prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Rule {
    Formula { expression: String, timeframe: crate::Timeframe },
    Condition { root: ConditionGroup, timeframe: crate::Timeframe },
    Line { start: i64, end: i64, first: f64, last: f64, logarithmic: bool },
}
impl Rule {
    fn scan(root: &ConditionGroup, timeframe: crate::Timeframe) -> ScanDefinition {
        ScanDefinition { name: "Alert".into(), timeframe, root: root.clone() }
    }

    pub fn timeframes(&self) -> Vec<crate::Timeframe> {
        match self {
            Self::Formula { timeframe, .. } => vec![*timeframe],
            Self::Line { .. } => vec![crate::Timeframe::M1],
            Self::Condition { root, timeframe } => Self::scan(root, *timeframe).timeframes(),
        }
    }

    pub fn value_set(&self, set: &SeriesSet) -> Result<f64, String> {
        match self {
            Self::Condition { root, timeframe } => {
                let evaluation = Self::scan(root, *timeframe).evaluate_set(set);
                match evaluation.outcome {
                    ScanOutcome::Match => Ok(1.0),
                    ScanOutcome::NoMatch => Ok(0.0),
                    ScanOutcome::Unavailable => Err(evaluation.reason.unwrap_or("Condition unavailable".into())),
                }
            }
            _ => {
                let timeframe = self.timeframes()[0];
                self.value(&set.get(&timeframe).ok_or("No condition history")?.bars)
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Condition { root, timeframe } => Self::scan(root, *timeframe).validate(),
            Self::Formula { expression, .. } => crate::indicators::pcf::evaluate(
                expression,
                &crate::indicators::registry::Input::new(&[], crate::Timeframe::D1),
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
            Self::Line { start, end, first, last, logarithmic } => {
                if *start > 0
                    && end > start
                    && first.is_finite()
                    && last.is_finite()
                    && (!logarithmic || (*first > 0. && *last > 0.))
                {
                    Ok(())
                } else {
                    Err("Line requires two increasing timestamps and valid prices".into())
                }
            }
        }
    }
    pub fn value(&self, bars: &[crate::Bar]) -> Result<f64, String> {
        let value = match self {
            Self::Condition { timeframe, .. } => {
                return self.value_set(&SeriesSet::from([(
                    *timeframe,
                    crate::Series::new("Alert", *timeframe, bars.to_vec()),
                )]));
            }
            Self::Formula { expression, timeframe } => *crate::indicators::pcf::evaluate(
                expression,
                &crate::indicators::registry::Input::new(bars, *timeframe),
            )
            .map_err(|e| e.to_string())?
            .last()
            .ok_or("No formula value")?,
            Self::Line { start, end, first, last, logarithmic } => {
                let bar = bars.last().ok_or("No condition history")?;
                let fraction = (bar.ts - start) as f64 / (end - start) as f64;
                let line = if *logarithmic {
                    (first.ln() + fraction * (last.ln() - first.ln())).exp()
                } else {
                    first + fraction * (last - first)
                };
                bar.close - line
            }
        };
        if value.is_finite() { Ok(value) } else { Err("Condition needs more valid history".into()) }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Formula { expression, timeframe } => format!("{expression} · {}", timeframe.label()),
            Self::Line { .. } => "Price minus saved line (time extrapolation)".into(),
            Self::Condition { root, timeframe } => format!(
                "Condition: {} conditions (match {}) · {}",
                root.leaves().len(),
                if root.mode == MatchMode::All { "all" } else { "any" },
                timeframe.label()
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub alert: Alert,
    pub price: f64,
    pub quote_ts: i64,
    pub observed_at: i64,
    pub source: String,
    pub seen: bool,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub symbol: String,
    pub price: f64,
    pub ts: i64,
    pub simulated: bool,
    pub source: String,
}

impl Observation {
    pub fn valid_at(&self, now: i64) -> bool {
        self.price.is_finite() && self.ts > 0 && self.ts <= now + 30 && now.saturating_sub(self.ts) <= MAX_QUOTE_AGE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    #[serde(serialize_with = "serialize_version")]
    pub version: u32,
    pub alerts: Vec<Alert>,
    pub events: Vec<Event>,
    next_id: u64,
    /// Keep duplicate protection even after bounded history has been pruned.
    #[serde(default)]
    fired_through: HashMap<u64, i64>,
    #[serde(skip)]
    previous: HashMap<u64, Observation>,
}

fn serialize_version<S: serde::Serializer>(_: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(3)
}

impl Default for Book {
    fn default() -> Self {
        Self {
            version: 3,
            alerts: vec![],
            events: vec![],
            next_id: 1,
            fired_through: HashMap::new(),
            previous: HashMap::new(),
        }
    }
}

impl Book {
    /// Drop corrupted duplicate-protection pins without discarding the user's alerts.
    pub fn sanitize_fired_through(&mut self, now: i64) {
        self.fired_through
            .retain(|id, ts| self.alerts.iter().any(|a| a.id == *id) && *ts > 0 && *ts <= now.saturating_add(30));
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_at(chrono::Utc::now().timestamp())
    }

    pub fn validate_at(&self, now: i64) -> Result<(), String> {
        if !(1..=3).contains(&self.version)
            || self.next_id == 0
            || self.alerts.len() > MAX_ALERTS
            || self.events.len() > MAX_EVENTS
        {
            return Err("Unsupported or oversized price-alert file".into());
        }
        let mut ids = HashSet::new();
        for a in &self.alerts {
            validate_alert(&a.symbol, a.level)?;
            a.repeat.validate()?;
            if let Some(rule) = &a.rule {
                rule.validate()?;
                if matches!(rule, Rule::Condition { .. }) && !(a.level > 0.0 && a.level < 1.0) {
                    return Err("Condition alert level must be strictly between 0 and 1".into());
                }
            }
            if a.id == 0 || a.id >= self.next_id || !ids.insert(a.id) {
                return Err("Invalid or duplicate price-alert ID".into());
            }
        }
        if self.fired_through.iter().any(|(id, ts)| !ids.contains(id) || *ts <= 0 || *ts > now.saturating_add(30)) {
            return Err("Invalid price-alert duplicate-protection timestamp or ID".into());
        }
        if self.alerts.iter().map(|a| &a.symbol).collect::<HashSet<_>>().len() > MAX_SYMBOLS {
            return Err(format!("At most {MAX_SYMBOLS} distinct price-alert symbols"));
        }
        for e in &self.events {
            validate_alert(&e.alert.symbol, e.alert.level)?;
            e.alert.repeat.validate()?;
            if !e.price.is_finite() || e.quote_ts <= 0 || e.observed_at <= 0 || e.source.len() > 500 {
                return Err("Invalid price-alert history".into());
            }
        }
        Ok(())
    }

    pub fn add(&mut self, symbol: String, level: f64, direction: Direction, simulated: bool) -> Result<u64, String> {
        validate_alert(&symbol, level)?;
        if self.alerts.len() >= MAX_ALERTS {
            return Err(format!("At most {MAX_ALERTS} price alerts; delete an old alert first"));
        }
        if !self.alerts.iter().any(|a| a.symbol == symbol)
            && self.alerts.iter().map(|a| &a.symbol).collect::<HashSet<_>>().len() >= MAX_SYMBOLS
        {
            return Err(format!("At most {MAX_SYMBOLS} distinct price-alert symbols"));
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or("No free alert IDs")?;
        self.alerts.push(Alert {
            id,
            symbol,
            level,
            direction,
            status: Status::Armed,
            simulated,
            rule: None,
            repeat: Repeat::Once,
            last_fired: None,
        });
        Ok(id)
    }

    /// Saving an edited alert rearms it with a fresh baseline, as does enabling it.
    pub fn edit(&mut self, id: u64, level: f64, direction: Direction) -> Result<(), String> {
        let a = self.alerts.iter_mut().find(|a| a.id == id).ok_or("Alert no longer exists")?;
        validate_alert(&a.symbol, level)?;
        a.level = level;
        a.direction = direction;
        a.status = Status::Armed;
        a.last_fired = None;
        self.previous.remove(&id);
        Ok(())
    }

    pub fn set_repeat(&mut self, id: u64, repeat: Repeat) -> Result<(), String> {
        repeat.validate()?;
        let a = self.alerts.iter_mut().find(|a| a.id == id).ok_or("Alert no longer exists")?;
        a.repeat = repeat;
        a.status = Status::Armed;
        a.last_fired = None;
        self.previous.remove(&id);
        Ok(())
    }

    pub fn toggle(&mut self, id: u64) {
        if let Some(a) = self.alerts.iter_mut().find(|a| a.id == id) {
            a.status = if a.status == Status::Armed { Status::Disabled } else { Status::Armed };
            a.last_fired = None;
            self.previous.remove(&id);
        }
    }

    pub fn delete(&mut self, id: u64) {
        self.alerts.retain(|a| a.id != id);
        self.fired_through.remove(&id);
        self.previous.remove(&id);
    }

    pub fn reset_symbol(&mut self, symbol: &str) {
        self.previous
            .retain(|id, _| !self.alerts.iter().any(|a| a.id == *id && a.symbol == symbol && a.rule.is_none()));
    }

    pub fn observe(&mut self, q: Observation, now: i64) -> Vec<Event> {
        self.observe_target(q, now, None)
    }
    pub fn observe_rule(&mut self, id: u64, q: Observation, now: i64) -> Vec<Event> {
        self.observe_target(q, now, Some(id))
    }
    pub fn set_rule(&mut self, id: u64, rule: Option<Rule>) -> Result<(), String> {
        if let Some(r) = &rule {
            r.validate()?;
        }
        let a = self.alerts.iter_mut().find(|a| a.id == id).ok_or("Alert no longer exists")?;
        a.rule = rule;
        self.version = 3;
        self.previous.remove(&id);
        Ok(())
    }
    pub fn reset_rule(&mut self, id: u64) {
        self.previous.remove(&id);
    }
    fn observe_target(&mut self, q: Observation, now: i64, target: Option<u64>) -> Vec<Event> {
        if !q.valid_at(now) {
            if let Some(id) = target {
                self.previous.remove(&id);
            } else {
                self.reset_symbol(&q.symbol);
            }
            return vec![];
        }
        let mut fired = Vec::new();
        for a in self.alerts.iter_mut().filter(|a| {
            a.symbol == q.symbol
                && a.status == Status::Armed
                && match target {
                    Some(id) => a.id == id && a.rule.is_some(),
                    None => a.rule.is_none(),
                }
        }) {
            if a.simulated != q.simulated {
                self.previous.remove(&a.id);
                continue;
            }
            if let Some(prev) = self.previous.get(&a.id) {
                // Ignore out-of-order data. Source switches and gaps seed again.
                if q.ts < prev.ts {
                    continue;
                }
                if prev.source == q.source && q.ts - prev.ts <= 120 && a.direction.crosses(prev.price, q.price, a.level)
                {
                    let cooling = match a.repeat {
                        Repeat::Once => false,
                        Repeat::Rearm { cooldown_secs } => {
                            a.last_fired.is_some_and(|last| now.saturating_sub(last) < i64::from(cooldown_secs))
                        }
                    };
                    if cooling
                        || self.fired_through.get(&a.id).is_some_and(|ts| q.ts <= *ts)
                        || self.events.iter().any(|e| e.alert.id == a.id && e.quote_ts == q.ts)
                    {
                        self.previous.insert(a.id, q.clone());
                        continue;
                    }
                    if a.repeat == Repeat::Once {
                        a.status = Status::Triggered;
                    }
                    a.last_fired = Some(now);
                    self.fired_through.insert(a.id, q.ts);
                    fired.push(Event {
                        alert: a.clone(),
                        price: q.price,
                        quote_ts: q.ts,
                        observed_at: now,
                        source: q.source.clone(),
                        seen: false,
                    });
                }
            }
            self.previous.insert(a.id, q.clone());
        }
        self.events.extend(fired.iter().cloned());
        if self.events.len() > MAX_EVENTS {
            self.events.drain(..self.events.len() - MAX_EVENTS);
        }
        fired
    }
}

fn validate_alert(symbol: &str, level: f64) -> Result<(), String> {
    let tokens: Vec<_> = symbol.split_whitespace().collect();
    if !crate::Security::resolve_tokens(&tokens).is_some_and(|(s, n)| n == tokens.len() && s.key() == symbol) {
        return Err("Choose a valid market symbol".into());
    }
    if !level.is_finite() || level.abs() > 1e15 {
        return Err("Enter a finite price between -1e15 and 1e15".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn condition(timeframe: Option<crate::Timeframe>) -> Rule {
        Rule::Condition {
            root: ConditionGroup::comparisons(
                MatchMode::All,
                vec![crate::scanner::ScanCondition {
                    timeframe,
                    right: crate::scanner::ScanOperand::Constant(100.),
                    ..crate::scanner::ScanCondition::default()
                }],
            ),
            timeframe: crate::Timeframe::D1,
        }
    }

    #[test]
    fn condition_validate_and_value_set_match_no_match_unavailable() {
        let rule = condition(Some(crate::Timeframe::W1));
        rule.validate().unwrap();
        assert_eq!(rule.timeframes(), vec![crate::Timeframe::D1, crate::Timeframe::W1]);
        assert_eq!(rule.label(), "Condition: 1 conditions (match all) · 1D");
        assert!(rule.value_set(&SeriesSet::new()).is_err());
        for (close, expected) in [(101., 1.), (99., 0.)] {
            let bars = vec![crate::Bar::new(1000, close, close, close, close, 1.)];
            let set = SeriesSet::from([(
                crate::Timeframe::W1,
                crate::Series::new("AAPL", crate::Timeframe::W1, bars.clone()),
            )]);
            assert_eq!(rule.value_set(&set).unwrap(), expected);
            assert!(rule.value(&bars).is_err());
            assert_eq!(condition(None).value(&bars).unwrap(), expected);
        }
        let invalid = Rule::Condition {
            root: ConditionGroup::comparisons(MatchMode::All, vec![]),
            timeframe: crate::Timeframe::D1,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn condition_v3_book_roundtrip_and_legacy_books_load() {
        let mut b = book(Direction::Above);
        b.edit(1, 0.5, Direction::Above).unwrap();
        b.set_rule(1, Some(condition(Some(crate::Timeframe::W1)))).unwrap();
        let json = serde_json::to_string(&b).unwrap();
        let restored: Book = serde_json::from_str(&json).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.version, 3);
        assert_eq!(restored.alerts, b.alerts);
        for version in [2, 3] {
            let mut legacy = serde_json::to_value(book(Direction::Above)).unwrap();
            legacy["version"] = version.into();
            legacy["alerts"][0].as_object_mut().unwrap().remove("rule");
            let restored: Book = serde_json::from_value(legacy).unwrap();
            restored.validate().unwrap();
            assert!(restored.alerts[0].rule.is_none());
        }
    }

    #[test]
    fn condition_crossing_fires_once_and_rearm_respects_cooldown() {
        for repeat in [Repeat::Once, Repeat::Rearm { cooldown_secs: 60 }] {
            let mut b = book(Direction::Above);
            b.edit(1, 0.5, Direction::Above).unwrap();
            b.set_rule(1, Some(condition(None))).unwrap();
            b.set_repeat(1, repeat).unwrap();
            for (ts, value, fires) in [
                (1000, 0., false),
                (1001, 1., true),
                (1010, 0., false),
                (1011, 1., false),
                (1061, 1., false),
                (1062, 0., false),
                (1063, 1., repeat != Repeat::Once),
            ] {
                assert_eq!(b.observe_rule(1, quote(value, ts), ts).len(), usize::from(fires));
            }
        }
    }

    #[test]
    fn rearm_cooldown_consumes_crossings_and_fires_again() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        assert_eq!(b.observe(quote(101., 1001), 1001).len(), 1);
        assert_eq!(b.alerts[0].status, Status::Armed);
        assert_eq!(b.alerts[0].last_fired, Some(1001));
        b.observe(quote(99., 1010), 1010);
        assert!(b.observe(quote(101., 1011), 1011).is_empty());
        assert!(b.observe(quote(101., 1061), 1061).is_empty());
        b.observe(quote(99., 1062), 1062);
        assert_eq!(b.observe(quote(101., 1063), 1063).len(), 1);
    }

    #[test]
    fn duplicate_quote_timestamp_is_suppressed_even_after_rearming() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        assert_eq!(b.observe(quote(101., 1001), 1001).len(), 1);
        b.observe(quote(99., 1001), 1061);
        assert!(b.observe(quote(101., 1001), 1061).is_empty());
        b.edit(1, 100., Direction::Above).unwrap();
        assert_eq!(b.alerts[0].last_fired, None);
        b.observe(quote(99., 1001), 1062);
        assert!(b.observe(quote(101., 1001), 1062).is_empty());
    }

    #[test]
    fn duplicate_protection_survives_pruned_history_and_restart() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        b.observe(quote(101., 1001), 1001);
        b.events.clear();
        let mut restored: Book = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        restored.observe(quote(99., 1001), 1061);
        assert!(restored.observe(quote(101., 1001), 1061).is_empty());
    }

    #[test]
    fn v2_defaults_to_once_and_serializes_as_v3() {
        let json = r#"{"version":2,"alerts":[{"id":1,"symbol":"AAPL US Equity","level":100.0,"direction":"Above","status":"Armed","simulated":false}],"events":[],"next_id":2}"#;
        let b: Book = serde_json::from_str(json).unwrap();
        b.validate().unwrap();
        assert_eq!(b.alerts[0].repeat, Repeat::Once);
        assert_eq!(b.alerts[0].last_fired, None);
        assert_eq!(serde_json::to_value(b).unwrap()["version"], 3);
    }

    #[test]
    fn repeating_restart_first_observation_only_seeds() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        b.observe(quote(101., 1001), 1001);
        b.observe(quote(99., 1060), 1060);
        let mut restored: Book = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert!(restored.observe(quote(101., 1061), 1061).is_empty());
        assert_eq!(restored.events.len(), 1);
        assert_eq!(restored.alerts[0].last_fired, Some(1001));
    }

    #[test]
    fn cooldown_bounds_are_validated_on_edit_and_load() {
        let mut b = book(Direction::Above);
        for seconds in [0, 59, 86_401, u32::MAX] {
            assert!(b.set_repeat(1, Repeat::Rearm { cooldown_secs: seconds }).is_err());
            b.alerts[0].repeat = Repeat::Rearm { cooldown_secs: seconds };
            assert!(b.validate().is_err());
        }
        for seconds in [60, 86_400] {
            b.set_repeat(1, Repeat::Rearm { cooldown_secs: seconds }).unwrap();
            b.validate().unwrap();
        }
    }

    #[test]
    fn cooldown_boundary_fires_at_exactly_cooldown_seconds() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        assert_eq!(b.observe(quote(101., 1001), 1001).len(), 1);
        // Still inside the cooldown window at cooldown-1.
        b.observe(quote(99., 1050), 1050);
        assert!(b.observe(quote(101., 1060), 1060).is_empty());
        b.observe(quote(99., 1061), 1061);
        // Exactly cooldown_secs after last_fired (1001): the window has expired.
        assert_eq!(b.observe(quote(101., 1061), 1061).len(), 1);
    }

    #[test]
    fn rearm_after_edit_resets_cooldown_for_a_fresh_crossing() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 300 }).unwrap();
        b.observe(quote(99., 1000), 1000);
        assert_eq!(b.observe(quote(101., 1001), 1001).len(), 1);
        // Edit rearms with a fresh baseline and a fresh cooldown window.
        b.edit(1, 100., Direction::Above).unwrap();
        assert_eq!(b.alerts[0].last_fired, None);
        b.observe(quote(99., 1010), 1010);
        assert_eq!(b.observe(quote(101., 1011), 1011).len(), 1);
    }

    #[test]
    fn invalid_price_quote_must_not_reset_rule_alert_baselines() {
        let mut b = book(Direction::Above);
        b.edit(1, 0.5, Direction::Above).unwrap();
        b.set_rule(1, Some(condition(None))).unwrap();
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        // Seed the rule baseline at NoMatch.
        b.observe_rule(1, quote(0., 1000), 1000);
        // A price-feed failure for the same symbol must not touch the rule baseline.
        b.reset_symbol("AAPL US Equity");
        // The 0 -> 1 crossing is still armed and must fire.
        assert_eq!(b.observe_rule(1, quote(1., 1001), 1001).len(), 1);
    }

    #[test]
    fn validate_rejects_fired_through_pins_that_would_permanently_suppress_crossings() {
        let mut b = book(Direction::Above);
        b.set_repeat(1, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        let mut json = serde_json::to_value(&b).unwrap();
        // Far-future pin: every quote_ts is <= this sentinel, so nothing may ever fire.
        json["fired_through"] = serde_json::json!({ "1": 9_999_999_999_i64 });
        let crafted: Book = serde_json::from_value(json).unwrap();
        assert!(crafted.validate().is_err(), "fired_through must be bounds-checked like events.quote_ts");
    }

    #[test]
    fn validate_rejects_condition_levels_outside_the_zero_one_domain() {
        let mut b = book(Direction::Above);
        b.edit(1, 2.0, Direction::Above).unwrap();
        b.set_rule(1, Some(condition(None))).unwrap();
        assert!(b.validate().is_err(), "level 2.0 can never be crossed by a 0/1 condition value");
        b.edit(1, -1.0, Direction::Above).unwrap();
        assert!(b.validate().is_err(), "level -1.0 can never be crossed by a 0/1 condition value");
    }

    #[test]
    fn fired_through_validation_and_sanitizing_preserve_valid_alerts() {
        let mut b = book(Direction::Above);
        let other = b.add("MSFT US Equity".into(), 100., Direction::Above, false).unwrap();
        b.fired_through.insert(1, 1030);
        b.validate_at(1000).unwrap();
        b.fired_through.insert(other, 1031);
        b.fired_through.insert(999, 1000);
        assert!(b.validate_at(1000).is_err());
        b.sanitize_fired_through(1000);
        b.validate_at(1000).unwrap();
        assert_eq!(b.alerts.len(), 2);
        assert_eq!(b.fired_through, HashMap::from([(1, 1030)]));
        b.fired_through.insert(999, 1000);
        assert!(b.validate_at(1000).is_err());
    }

    #[test]
    fn invalid_price_observation_resets_only_price_baselines() {
        let mut b = book(Direction::Above);
        let rule = b.add("AAPL US Equity".into(), 0.5, Direction::Above, false).unwrap();
        b.set_rule(rule, Some(condition(None))).unwrap();
        b.observe(quote(99., 1000), 1000);
        b.observe_rule(rule, quote(0., 1000), 1000);
        b.observe(quote(f64::NAN, 1001), 1001);
        assert!(b.observe(quote(101., 1002), 1002).is_empty());
        assert_eq!(b.observe_rule(rule, quote(1., 1002), 1002).len(), 1);
        b.toggle(rule);
        b.observe_rule(rule, quote(0., 1003), 1003);
        b.observe_rule(rule, quote(f64::NAN, 1004), 1004);
        assert!(b.observe_rule(rule, quote(1., 1005), 1005).is_empty());
    }

    #[test]
    fn condition_level_domain_is_exclusive() {
        let mut b = book(Direction::Above);
        b.set_rule(1, Some(condition(None))).unwrap();
        for level in [0.0, 1.0, -1.0, 2.0] {
            b.alerts[0].level = level;
            assert!(b.validate().is_err());
        }
        b.alerts[0].level = 0.5;
        b.validate().unwrap();
    }

    #[test]
    fn conditions_are_isolated_and_line_math_and_migration_are_valid() {
        let mut b = book(Direction::Above);
        let id = b.add("AAPL US Equity".into(), 70., Direction::Above, false).unwrap();
        let rule = Rule::Formula { expression: "WRSI(14)".into(), timeframe: crate::Timeframe::D1 };
        rule.validate().unwrap();
        b.set_rule(id, Some(rule)).unwrap();
        b.observe_rule(id, quote(69., 1000), 1000);
        assert!(b.observe(quote(71., 1001), 1001).is_empty());
        assert_eq!(b.observe_rule(id, quote(71., 1001), 1001).len(), 1);
        assert_eq!(b.alerts[0].status, Status::Armed);
        let line = Rule::Line { start: 1000, end: 1100, first: 100., last: 110., logarithmic: false };
        line.validate().unwrap();
        let bar = crate::Bar::new(1200, 125., 125., 125., 125., 1.);
        assert_eq!(line.value(&[bar]).unwrap(), 5.);
        let json = r#"{"version":1,"alerts":[{"id":1,"symbol":"AAPL US Equity","level":100.0,"direction":"Above","status":"Armed","simulated":false}],"events":[],"next_id":2}"#;
        let legacy: Book = serde_json::from_str(json).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.alerts[0].rule.is_none());
        assert!(Rule::Formula { expression: "BOGUS(10)".into(), timeframe: crate::Timeframe::M1 }.validate().is_err());
    }
    fn quote(price: f64, ts: i64) -> Observation {
        Observation { symbol: "AAPL US Equity".into(), price, ts, simulated: false, source: "test".into() }
    }
    fn book(direction: Direction) -> Book {
        let mut b = Book::default();
        b.add("AAPL US Equity".into(), 100., direction, false).unwrap();
        b
    }
    #[test]
    fn crossings_fire_once_including_touch_and_gap_and_can_be_rearmed() {
        for (direction, before, after) in [(Direction::Above, 99., 100.), (Direction::Below, 101., 98.)] {
            let mut b = book(direction);
            assert!(b.observe(quote(before, 1000), 1000).is_empty());
            assert_eq!(b.observe(quote(after, 1001), 1001).len(), 1);
            assert!(b.observe(quote(after, 1002), 1002).is_empty());
            assert_eq!(b.events.len(), 1);
            b.toggle(1);
            assert!(b.observe(quote(after, 1003), 1003).is_empty());
            b.observe(quote(before, 1004), 1004);
            assert_eq!(b.observe(quote(after, 1005), 1005).len(), 1);
        }
    }
    #[test]
    fn initially_beyond_level_equality_and_opposite_moves_do_not_fire() {
        let mut b = book(Direction::Above);
        for (n, price) in [101., 100., 101., 102., 99.].into_iter().enumerate() {
            assert!(b.observe(quote(price, 1000 + n as i64), 1005).is_empty());
        }
        assert_eq!(b.observe(quote(101., 1006), 1006).len(), 1);
    }
    #[test]
    fn rejects_stale_future_invalid_out_of_order_wrong_source_and_simulated_quotes() {
        let mut b = book(Direction::Above);
        b.observe(quote(99., 1000), 1000);
        assert!(b.observe(quote(101., 999), 1000).is_empty());
        let mut q = quote(101., 1001);
        q.simulated = true;
        assert!(b.observe(q, 1001).is_empty());
        assert!(b.observe(quote(101., 1002), 1002).is_empty());
        b.observe(quote(99., 1003), 1003);
        let mut q = quote(101., 1004);
        q.source = "new source".into();
        assert!(b.observe(q, 1004).is_empty());
        for q in [quote(101., 1), quote(101., 9999), quote(f64::NAN, 1005)] {
            assert!(b.observe(q, 2000).is_empty());
        }
        assert!(b.observe(quote(101., 2000), 2000).is_empty());
    }
    #[test]
    fn edit_disable_delete_and_restart_do_not_reuse_baselines() {
        let mut b = book(Direction::Above);
        b.observe(quote(99., 1000), 1000);
        b.edit(1, 100., Direction::Above).unwrap();
        assert!(b.observe(quote(101., 1001), 1001).is_empty());
        b.toggle(1);
        b.observe(quote(99., 1002), 1002);
        assert!(b.observe(quote(101., 1003), 1003).is_empty());
        b.toggle(1);
        b.observe(quote(99., 1004), 1004);
        let mut restored: Book = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        restored.validate().unwrap();
        assert!(restored.observe(quote(101., 1005), 1005).is_empty());
        restored.delete(1);
        assert!(restored.observe(quote(99., 1006), 1006).is_empty());
        assert!(restored.alerts.is_empty());
    }
    #[test]
    fn long_gaps_seed_and_same_bar_updates_can_cross() {
        let mut b = book(Direction::Above);
        b.observe(quote(99., 1000), 1000);
        assert!(b.observe(quote(101., 1300), 1300).is_empty());
        b.observe(quote(99., 1300), 1301);
        assert_eq!(b.observe(quote(101., 1300), 1302).len(), 1);
    }
    #[test]
    fn validates_configuration_and_limits_history() {
        let mut b = book(Direction::Above);
        assert!(b.add("AAPL".into(), 1., Direction::Above, false).is_err());
        assert!(b.edit(1, f64::INFINITY, Direction::Above).is_err());
        for n in 0..MAX_EVENTS + 5 {
            b.toggle(1);
            // toggle back if currently disabled
            if b.alerts[0].status != Status::Armed {
                b.toggle(1);
            }
            let ts = 1000 + n as i64 * 2;
            b.observe(quote(99., ts), ts);
            b.observe(quote(101., ts + 1), ts + 1);
        }
        assert_eq!(b.events.len(), MAX_EVENTS);
        b.validate().unwrap();
    }
}
