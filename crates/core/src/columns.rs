//! Custom watchlist column definitions and evaluation.
use crate::Timeframe;
use crate::conditions::{ConditionGroup, FormulaCondition, SeriesSet};
use crate::scanner::{ScanDefinition, ScanOutcome};
use serde::{Deserialize, Serialize};

pub const MAX_COLUMNS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnKind {
    Formula { expression: String },
    Condition(ConditionGroup),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub id: u32,
    pub title: String,
    pub timeframe: Timeframe,
    pub kind: ColumnKind,
}

fn version() -> u32 {
    1
}

fn write_version<S: serde::Serializer>(_: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(1)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSet {
    #[serde(default = "version", serialize_with = "write_version")]
    version: u32,
    pub name: String,
    pub columns: Vec<ColumnDef>,
}

impl Default for ColumnSet {
    fn default() -> Self {
        Self { version: 1, name: String::new(), columns: Vec::new() }
    }
}

impl ColumnSet {
    /// Keep valid columns in input order, bounded by the supported column count.
    /// The returned count includes both invalid columns and excess valid columns.
    pub fn sanitized(mut self) -> (Self, usize) {
        let original_len = self.columns.len();
        let mut titles = std::collections::HashSet::new();
        self.columns.retain(|column| column.validate().is_ok() && titles.insert(column.title.to_ascii_lowercase()));
        self.columns.truncate(MAX_COLUMNS);
        let mut next = self.columns.iter().map(|c| c.id).max().unwrap_or(0).wrapping_add(1);
        let reserved: std::collections::HashSet<_> = self.columns.iter().map(|c| c.id).collect();
        let mut ids = std::collections::HashSet::new();
        for column in &mut self.columns {
            if !ids.insert(column.id) {
                while reserved.contains(&next) || ids.contains(&next) {
                    next = next.wrapping_add(1);
                }
                column.id = next;
                ids.insert(next);
                next = next.wrapping_add(1);
            }
        }
        let dropped = original_len - self.columns.len();
        (self, dropped)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.columns.len() > MAX_COLUMNS {
            return Err(format!("At most {MAX_COLUMNS} columns are supported"));
        }
        let mut ids = std::collections::HashSet::new();
        let mut titles = std::collections::HashSet::new();
        for column in &self.columns {
            column.validate()?;
            if !ids.insert(column.id) {
                return Err(format!("Duplicate column id: {}", column.id));
            }
            if !titles.insert(column.title.to_ascii_lowercase()) {
                return Err(format!("Duplicate column title: {}", column.title));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Value(f64),
    Check(bool),
    Unavailable(String),
}

impl ColumnDef {
    fn scan(&self, root: ConditionGroup) -> ScanDefinition {
        ScanDefinition { name: self.title.clone(), timeframe: self.timeframe, root }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() || self.title.chars().count() > 24 {
            return Err("Column title must contain 1 to 24 characters".into());
        }
        match &self.kind {
            ColumnKind::Formula { expression } => {
                FormulaCondition { expression: expression.clone(), timeframe: None }.validate()
            }
            ColumnKind::Condition(group) => group.validate(),
        }
    }

    pub fn timeframes(&self) -> Vec<Timeframe> {
        match &self.kind {
            ColumnKind::Formula { .. } => vec![self.timeframe],
            ColumnKind::Condition(group) => self.scan(group.clone()).timeframes(),
        }
    }

    pub fn required_bars_for(&self, tf: Timeframe) -> usize {
        match &self.kind {
            ColumnKind::Formula { expression } => {
                if tf == self.timeframe {
                    crate::indicators::pcf::lookback(expression).unwrap_or(300).max(300)
                } else {
                    1
                }
            }
            ColumnKind::Condition(group) => self.scan(group.clone()).required_bars_for(tf),
        }
    }

    pub fn evaluate(&self, set: &SeriesSet) -> Cell {
        match &self.kind {
            ColumnKind::Formula { expression } => {
                let Some(series) = set.get(&self.timeframe) else {
                    return Cell::Unavailable(format!("No {} history", self.timeframe.label()));
                };
                let result = FormulaCondition { expression: expression.clone(), timeframe: None }.evaluate(series);
                match result.left {
                    Some(value) => Cell::Value(value),
                    None => Cell::Unavailable(result.reason.unwrap_or_else(|| "Invalid input".into())),
                }
            }
            ColumnKind::Condition(group) => {
                let result = self.scan(group.clone()).evaluate_set(set);
                match result.outcome {
                    ScanOutcome::Match => Cell::Check(true),
                    ScanOutcome::NoMatch => Cell::Check(false),
                    ScanOutcome::Unavailable => {
                        Cell::Unavailable(result.reason.unwrap_or_else(|| "Unavailable".into()))
                    }
                }
            }
        }
    }
}

pub fn sort_key(cell: Option<&Cell>) -> Option<f64> {
    match cell {
        Some(Cell::Value(value)) => Some(*value).filter(|v| v.is_finite()),
        Some(Cell::Check(value)) => Some(u8::from(*value) as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conditions::{ConditionItem, ScanCondition};
    use crate::scanner::MatchMode;
    use crate::{Bar, Series};

    fn column(expression: &str) -> ColumnDef {
        ColumnDef {
            id: 1,
            title: "Close".into(),
            timeframe: Timeframe::D1,
            kind: ColumnKind::Formula { expression: expression.into() },
        }
    }

    #[test]
    fn formula_value_and_unavailable() {
        let set = SeriesSet::from([(
            Timeframe::D1,
            Series::new("TEST", Timeframe::D1, vec![Bar::new(1, 2., 2., 2., 2., 1.)]),
        )]);
        assert_eq!(column("C").evaluate(&set), Cell::Value(2.));
        for expression in ["AVGC20", "INVALID("] {
            assert!(matches!(column(expression).evaluate(&set), Cell::Unavailable(_)));
        }
        assert!(matches!(column("C").evaluate(&SeriesSet::new()), Cell::Unavailable(_)));
    }

    #[test]
    fn nested_condition_check() {
        let mut c = column("1");
        c.kind = ColumnKind::Condition(ConditionGroup {
            mode: MatchMode::All,
            items: vec![ConditionItem::Group(ConditionGroup {
                mode: MatchMode::Any,
                items: vec![ConditionItem::Formula(FormulaCondition {
                    expression: "1".into(),
                    timeframe: Some(Timeframe::W1),
                })],
            })],
        });
        assert!(c.validate().is_ok());
        assert!(c.timeframes().contains(&Timeframe::W1));
        let set = SeriesSet::from([(
            Timeframe::W1,
            Series::new("TEST", Timeframe::W1, vec![Bar::new(1, 2., 2., 2., 2., 1.)]),
        )]);
        assert_eq!(c.evaluate(&set), Cell::Check(true));
    }

    #[test]
    fn sort_keys() {
        assert_eq!(sort_key(Some(&Cell::Value(2.))), Some(2.));
        assert!(sort_key(Some(&Cell::Check(true))) > sort_key(Some(&Cell::Check(false))));
        assert_eq!(sort_key(Some(&Cell::Unavailable("No history".into()))), None);
        assert_eq!(sort_key(None), None);
    }

    #[test]
    fn sanitized_drops_invalid_and_oversized_columns_before_truncating() {
        let mut columns = vec![column("INVALID("), column(&"C".repeat(501)), column("C")];
        columns[2].title = "x".repeat(25);
        columns.extend((0..10).map(|id| ColumnDef { id, title: format!("Close {}", id + 1), ..column("C") }));
        let set = ColumnSet { name: "Imported".into(), columns, ..Default::default() };
        let (clean, dropped) = set.sanitized();
        assert_eq!(dropped, 5);
        assert_eq!(clean.name, "Imported");
        assert_eq!(clean.columns.iter().map(|c| c.id).collect::<Vec<_>>(), (0..8).collect::<Vec<_>>());
        assert!(clean.validate().is_ok());
        assert_eq!(clean.clone().sanitized(), (clean, 0));
        assert_eq!(ColumnSet::default().sanitized(), (ColumnSet::default(), 0));
    }

    #[test]
    fn column_set_roundtrip_and_limits() {
        let mut set = ColumnSet { columns: vec![column("C")], ..Default::default() };
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(serde_json::from_value::<ColumnSet>(json).unwrap(), set);
        assert!(set.validate().is_ok());
        set.columns[0].title = "x".repeat(25);
        assert!(set.validate().is_err());
        set.columns = vec![column("C"); 9];
        assert!(set.validate().is_err());
        assert!(column("").validate().is_err());
    }

    #[test]
    fn formula_columns_size_history_from_expression_lookback() {
        // AVG(C,1000) needs 1000 bars; XAVGC300 needs 300*4-1 + 1 = 1200.
        for (expression, needed) in [("AVG(C,1000)", 1000), ("XAVGC300", 1200), ("AVGC20", 20)] {
            let c = column(expression);
            assert!(c.validate().is_ok(), "{expression}");
            assert!(
                c.required_bars_for(Timeframe::D1) >= needed,
                "{expression} required_bars_for={} but needs {needed}",
                c.required_bars_for(Timeframe::D1)
            );
        }
        // Other timeframes must not inflate a single-timeframe formula column.
        assert_eq!(column("AVG(C,1000)").required_bars_for(Timeframe::W1), 1);
    }

    #[test]
    fn column_set_validate_rejects_duplicate_ids() {
        let mut set = ColumnSet::default();
        let mut a = column("C");
        a.id = 7;
        let mut b = column("H");
        b.id = 7;
        set.columns = vec![a, b];
        assert!(set.validate().is_err(), "duplicate column ids make editor selection and COL DEL ambiguous");
        let (clean, _) = set.sanitized();
        let mut seen = std::collections::HashSet::new();
        assert!(
            clean.columns.iter().all(|c| seen.insert(c.id)),
            "sanitized() must not keep duplicate ids: {:?}",
            clean.columns.iter().map(|c| c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_set_validate_rejects_duplicate_titles() {
        let mut set = ColumnSet::default();
        let mut a = column("C");
        a.title = "SMA".into();
        let mut b = column("H");
        b.title = "sma".into();
        set.columns = vec![a, b];
        // COL DEL matches titles case-insensitively and retain() drops every match,
        // so two same-titled columns would both disappear on one delete.
        assert!(set.validate().is_err(), "duplicate titles (case-insensitive) must be rejected");
    }

    #[test]
    fn sanitization_drops_duplicate_titles_and_renumbers_duplicate_ids() {
        let mut a = column("C");
        a.id = 7;
        a.title = "First".into();
        let mut b = column("H");
        b.id = 7;
        b.title = "Second".into();
        let mut c = column("L");
        c.id = 8;
        c.title = "Third".into();
        let mut duplicate = b.clone();
        duplicate.title = "FIRST".into();
        let set = ColumnSet { columns: vec![a.clone(), b, c, duplicate], ..Default::default() };
        let (clean, dropped) = set.sanitized();
        assert_eq!(dropped, 1);
        assert_eq!(clean.columns.len(), 3);
        assert_eq!(clean.columns[0], a);
        assert_eq!(clean.columns.iter().map(|c| c.id).collect::<Vec<_>>(), vec![7, 9, 8]);
        assert!(clean.validate().is_ok());
        assert_eq!(clean.clone().sanitized(), (clean, 0));
    }

    #[test]
    fn sort_key_maps_non_finite_values_to_unavailable() {
        assert_eq!(sort_key(Some(&Cell::Value(f64::NAN))), None);
        assert_eq!(sort_key(Some(&Cell::Value(f64::INFINITY))), None);
        assert_eq!(sort_key(Some(&Cell::Value(f64::NEG_INFINITY))), None);
        assert_eq!(sort_key(Some(&Cell::Value(-0.0))), Some(0.0));
    }

    #[test]
    fn condition_overrides_collect_timeframes_and_bars_ago() {
        let mut c = column("1");
        c.kind = ColumnKind::Condition(ConditionGroup {
            mode: MatchMode::All,
            items: vec![ConditionItem::Compare(ScanCondition {
                timeframe: Some(Timeframe::W1),
                right: crate::scanner::ScanOperand::Sma { period: 500, source: crate::scanner::Source::Close },
                right_bars_ago: 250,
                ..Default::default()
            })],
        });
        assert!(c.validate().is_ok());
        assert!(c.timeframes().contains(&Timeframe::D1));
        assert!(c.timeframes().contains(&Timeframe::W1));
        // SMA(500) shifted 250 bars ago needs 750 bars on the override timeframe.
        assert!(c.required_bars_for(Timeframe::W1) >= 750, "{}", c.required_bars_for(Timeframe::W1));
        // The column timeframe is still listed but unused by this leaf.
        assert_eq!(c.required_bars_for(Timeframe::D1), 1);
    }

    #[test]
    fn oversized_formulas_are_rejected_before_evaluation() {
        let mut c = column(&"C+".repeat(300));
        assert!(c.validate().is_err());
        c.kind = ColumnKind::Formula { expression: "C".repeat(501) };
        assert!(c.validate().is_err());
        c.kind = ColumnKind::Formula { expression: " ".repeat(20) };
        assert!(c.validate().is_err());
    }
}
