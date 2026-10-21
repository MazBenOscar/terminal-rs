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
        self.columns.retain(|column| column.validate().is_ok());
        self.columns.truncate(MAX_COLUMNS);
        let dropped = original_len - self.columns.len();
        (self, dropped)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.columns.len() > MAX_COLUMNS {
            return Err(format!("At most {MAX_COLUMNS} columns are supported"));
        }
        for column in &self.columns {
            column.validate()?;
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
            ColumnKind::Formula { .. } => {
                if tf == self.timeframe {
                    300
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
    use crate::conditions::ConditionItem;
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
        columns.extend((0..10).map(|id| ColumnDef { id, ..column("C") }));
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
}
