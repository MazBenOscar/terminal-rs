//! Drawing undo stores inverse operations, never whole-chart history snapshots.

use tr_chart::drawings::{Author, Drawing};

#[derive(Debug, Clone)]
pub enum Operation {
    Add { index: usize, drawing: Drawing },
    Remove { index: usize, drawing: Drawing },
    Update { before: Drawing, after: Drawing },
    Clear(Vec<Drawing>),
}

impl Operation {
    pub fn ids(&self) -> Vec<u64> {
        match self {
            Self::Add { drawing, .. } | Self::Remove { drawing, .. } => vec![drawing.id],
            Self::Update { after, .. } => vec![after.id],
            Self::Clear(all) => all.iter().map(|d| d.id).collect(),
        }
    }

    pub fn inverse(&self) -> Self {
        match self {
            Self::Add { index, drawing } => Self::Remove { index: *index, drawing: drawing.clone() },
            Self::Remove { index, drawing } => Self::Add { index: *index, drawing: drawing.clone() },
            Self::Update { before, after } => Self::Update { before: after.clone(), after: before.clone() },
            Self::Clear(_) => unreachable!("clear is inverted per drawing"),
        }
    }

    pub fn apply(&self, drawings: &mut Vec<Drawing>) -> bool {
        match self {
            Self::Add { index, drawing } => {
                if drawings.iter().any(|d| d.id == drawing.id) {
                    return false;
                }
                drawings.insert((*index).min(drawings.len()), drawing.clone());
            }
            Self::Remove { drawing, .. } => {
                let Some(index) = drawings.iter().position(|d| d == drawing) else { return false };
                drawings.remove(index);
            }
            Self::Update { before, after } => {
                let Some(drawing) = drawings.iter_mut().find(|d| *d == before) else { return false };
                *drawing = after.clone();
            }
            Self::Clear(all) => {
                for drawing in all {
                    if let Some(index) = drawings.iter().position(|d| d == drawing) {
                        drawings.remove(index);
                    }
                }
            }
        }
        true
    }

    pub fn adopt(&mut self, symbol: &str) {
        let adopt = |d: &mut Drawing| {
            if d.symbol.is_none() {
                d.symbol = Some(symbol.to_owned());
            }
        };
        match self {
            Self::Add { drawing, .. } | Self::Remove { drawing, .. } => adopt(drawing),
            Self::Update { before, after } => {
                adopt(before);
                adopt(after);
            }
            Self::Clear(all) => all.iter_mut().for_each(adopt),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Add { .. } => "drawing_add",
            Self::Remove { .. } => "drawing_remove",
            Self::Update { .. } => "drawing_update",
            Self::Clear(_) => "drawing_clear",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub revision: u64,
    pub author: Author,
    pub turn: Option<String>,
    pub operations: Vec<Operation>,
}

impl Edit {
    pub fn between(before: &[Drawing], after: &[Drawing], author: Author, turn: Option<String>) -> Self {
        let mut operations = Vec::new();
        if !before.is_empty() && after.is_empty() {
            operations.push(Operation::Clear(before.to_vec()));
        } else {
            for (index, drawing) in before.iter().enumerate().rev() {
                match after.iter().find(|d| d.id == drawing.id) {
                    None => operations.push(Operation::Remove { index, drawing: drawing.clone() }),
                    Some(next) if next != drawing => {
                        operations.push(Operation::Update { before: drawing.clone(), after: next.clone() })
                    }
                    _ => {}
                }
            }
            for (index, drawing) in after.iter().enumerate() {
                if !before.iter().any(|d| d.id == drawing.id) {
                    operations.push(Operation::Add { index, drawing: drawing.clone() });
                }
            }
        }
        Self { revision: 0, author, turn, operations }
    }

    pub fn inverse_operations(&self) -> Vec<Operation> {
        self.operations
            .iter()
            .rev()
            .flat_map(|op| match op {
                Operation::Clear(all) => all
                    .iter()
                    .enumerate()
                    .map(|(index, drawing)| Operation::Add { index, drawing: drawing.clone() })
                    .collect(),
                _ => vec![op.inverse()],
            })
            .collect()
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct UndoTurn {
    pub reverted: usize,
    pub skipped: Vec<u64>,
}
