//! Drawing undo stores inverse operations, never whole-chart history snapshots.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tr_chart::drawings::{Author, Drawing};

/// Shared by every live chart, including pop-outs. Claims survive removal so undo
/// cannot resurrect an id that another chart has since claimed.
pub(crate) type SharedDrawingIds = Arc<Mutex<HashSet<u64>>>;

#[derive(Default)]
pub(crate) struct DrawingIds {
    shared: SharedDrawingIds,
    owned: HashSet<u64>,
}

impl DrawingIds {
    pub(crate) fn attach(&mut self, shared: SharedDrawingIds) {
        if !Arc::ptr_eq(&self.shared, &shared) {
            *self = Self { shared, owned: HashSet::new() };
        }
    }

    pub(crate) fn repair(&mut self, drawings: &mut [Drawing]) {
        tr_chart::drawings::assign_ids(drawings);
        let mut claimed = self.shared.lock().unwrap();
        for drawing in drawings {
            if !self.owned.contains(&drawing.id) {
                while !claimed.insert(drawing.id) {
                    drawing.id = tr_chart::drawings::next_id();
                }
                self.owned.insert(drawing.id);
            }
        }
    }
}

impl Drop for DrawingIds {
    fn drop(&mut self) {
        let mut claimed = self.shared.lock().unwrap();
        for id in &self.owned {
            claimed.remove(id);
        }
    }
}

/// A position is anchored to identities, never to indices that later edits shift.
#[derive(Debug, Clone)]
pub struct Position {
    preceding: Option<u64>,
    following: Option<u64>,
}

impl Position {
    fn at(drawings: &[Drawing], index: usize) -> Self {
        Self {
            preceding: index.checked_sub(1).map(|i| drawings[i].id),
            following: drawings.get(index + 1).map(|d| d.id),
        }
    }

    fn resolve(&self, drawings: &[Drawing]) -> usize {
        self.following
            .and_then(|id| drawings.iter().position(|d| d.id == id))
            .or_else(|| self.preceding.and_then(|id| drawings.iter().position(|d| d.id == id).map(|i| i + 1)))
            .unwrap_or(drawings.len())
    }
}

#[derive(Debug, Clone)]
pub enum Operation {
    Add { position: Position, drawing: Drawing },
    Remove { position: Position, drawing: Drawing },
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
            Self::Add { position, drawing } => Self::Remove { position: position.clone(), drawing: drawing.clone() },
            Self::Remove { position, drawing } => Self::Add { position: position.clone(), drawing: drawing.clone() },
            Self::Update { before, after } => Self::Update { before: after.clone(), after: before.clone() },
            Self::Clear(_) => unreachable!("clear is inverted per drawing"),
        }
    }

    pub fn apply(&self, drawings: &mut Vec<Drawing>) -> bool {
        match self {
            Self::Add { position, drawing } => {
                if drawings.iter().any(|d| d.id == drawing.id) {
                    return false;
                }
                drawings.insert(position.resolve(drawings), drawing.clone());
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
                    None => operations
                        .push(Operation::Remove { position: Position::at(before, index), drawing: drawing.clone() }),
                    Some(next) if next != drawing => {
                        operations.push(Operation::Update { before: drawing.clone(), after: next.clone() })
                    }
                    _ => {}
                }
            }
            for (index, drawing) in after.iter().enumerate() {
                if !before.iter().any(|d| d.id == drawing.id) {
                    operations.push(Operation::Add { position: Position::at(after, index), drawing: drawing.clone() });
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
                    .map(|(index, drawing)| Operation::Add {
                        position: Position::at(all, index),
                        drawing: drawing.clone(),
                    })
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
