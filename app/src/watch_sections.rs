//! Watchlist sections: named, collapsible groups over a list's ordered symbols.
//!
//! A list stores its symbols in display order plus section headers, each placed before
//! the symbol at `at` (empty sections share `at` with what follows). Symbols before the
//! first header are ungrouped. Edits go through [`Item`] rows and convert back with
//! [`split`], so `symbols` stays the list's membership and order for quotes and scans.
// TODO(watchlist UI): remove once the watchlist pane uses every helper.
#![allow(dead_code)]
use serde::{Deserialize, Serialize};

pub const MAX_SECTIONS: usize = 50;
pub const MAX_SECTION_NAME: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WatchSection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
    /// Index in the list's symbols of the section's first symbol.
    #[serde(default)]
    pub at: usize,
}

/// One row of a watchlist in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Section { name: String, collapsed: bool },
    Symbol(String),
}

impl Item {
    pub fn is_section(&self) -> bool {
        matches!(self, Item::Section { .. })
    }
}

/// Rows for `symbols` and `sections`. Out-of-range positions clamp to the end, and
/// headers sharing a position keep their stored order.
pub fn items(symbols: &[String], sections: &[WatchSection]) -> Vec<Item> {
    let mut sorted: Vec<&WatchSection> = sections.iter().collect();
    sorted.sort_by_key(|s| s.at.min(symbols.len()));
    let mut headers = sorted.into_iter().peekable();
    let mut out = Vec::with_capacity(symbols.len() + sections.len());
    let header = |s: &WatchSection| Item::Section { name: s.name.clone(), collapsed: s.collapsed };
    for (i, symbol) in symbols.iter().enumerate() {
        while let Some(s) = headers.next_if(|s| s.at.min(symbols.len()) <= i) {
            out.push(header(s));
        }
        out.push(Item::Symbol(symbol.clone()));
    }
    out.extend(headers.map(header));
    out
}

/// The stored form of `items`: ordered symbols and positioned headers.
pub fn split(items: &[Item]) -> (Vec<String>, Vec<WatchSection>) {
    let mut symbols = Vec::new();
    let mut sections = Vec::new();
    for item in items {
        match item {
            Item::Symbol(s) => symbols.push(s.clone()),
            Item::Section { name, collapsed } => {
                sections.push(WatchSection { name: name.clone(), collapsed: *collapsed, at: symbols.len() })
            }
        }
    }
    (symbols, sections)
}

/// Keep the rows' sections and the order of symbols still in `symbols` (first occurrence
/// wins); symbols not yet placed are appended at the end in `symbols` order.
pub fn resync(items: &[Item], symbols: &[String]) -> Vec<Item> {
    let wanted: std::collections::HashSet<&String> = symbols.iter().collect();
    let mut placed = std::collections::HashSet::new();
    let mut out: Vec<Item> = items
        .iter()
        .filter(|item| match item {
            Item::Symbol(s) => wanted.contains(s) && placed.insert(s.clone()),
            Item::Section { .. } => true,
        })
        .cloned()
        .collect();
    for s in symbols {
        if placed.insert(s.clone()) {
            out.push(Item::Symbol(s.clone()));
        }
    }
    out
}

/// The index range an item drags with: a symbol alone, a header with its symbols.
pub fn block(items: &[Item], index: usize) -> std::ops::Range<usize> {
    let end = if items.get(index).is_some_and(Item::is_section) {
        items.iter().skip(index + 1).position(Item::is_section).map_or(items.len(), |k| index + 1 + k)
    } else {
        index + 1
    };
    index..end.min(items.len())
}

/// Index of the header that owns `index` (None when ungrouped).
pub fn section_of(items: &[Item], index: usize) -> Option<usize> {
    items.iter().take(index + 1).rposition(Item::is_section)
}

/// Rows shown on screen: symbols inside collapsed sections are hidden.
pub fn visible(items: &[Item]) -> Vec<usize> {
    let mut hidden = false;
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| match item {
            Item::Section { collapsed, .. } => {
                hidden = *collapsed;
                true
            }
            Item::Symbol(_) => !hidden,
        })
        .map(|(i, _)| i)
        .collect()
}

/// Where a dragged section may land: a gap before another header or at the end, so a
/// section never splits another. Returns the valid gap nearest to `gap` (later on ties).
pub fn snap_section_gap(items: &[Item], gap: usize) -> usize {
    let gap = gap.min(items.len());
    (0..=items.len())
        .filter(|&g| g == items.len() || items[g].is_section())
        .min_by_key(|&g| (g.abs_diff(gap), std::cmp::Reverse(g)))
        .unwrap_or(items.len())
}

/// Move the item at `from` (with its block, for a header) into gap `to`, where gaps are
/// numbered 0..=len before the move. Headers snap with [`snap_section_gap`]. Returns
/// false when nothing changes (unknown row, or a gap inside or beside the moved block).
pub fn move_item(items: &mut Vec<Item>, from: usize, to: usize) -> bool {
    if from >= items.len() {
        return false;
    }
    let range = block(items, from);
    let to = if items[from].is_section() { snap_section_gap(items, to) } else { to.min(items.len()) };
    if (range.start..=range.end).contains(&to) {
        return false;
    }
    let moved: Vec<Item> = items.drain(range.clone()).collect();
    let to = if to > range.end { to - moved.len() } else { to };
    items.splice(to..to, moved);
    true
}

/// A trimmed section name, or why it cannot be used.
pub fn section_name(name: &str) -> Result<String, String> {
    if !crate::app::valid_watchlist_name(name) {
        return Err(format!("Section names must contain 1–{MAX_SECTION_NAME} visible characters"));
    }
    Ok(name.trim().to_owned())
}

/// Insert an expanded header into gap `gap` (clamped to the end).
pub fn insert_section(items: &mut Vec<Item>, gap: usize, name: &str) -> Result<(), String> {
    if items.iter().filter(|i| i.is_section()).count() >= MAX_SECTIONS {
        return Err(format!("At most {MAX_SECTIONS} sections are allowed"));
    }
    let name = section_name(name)?;
    items.insert(gap.min(items.len()), Item::Section { name, collapsed: false });
    Ok(())
}

pub fn rename_section(items: &mut [Item], index: usize, name: &str) -> Result<(), String> {
    let name = section_name(name)?;
    match items.get_mut(index) {
        Some(Item::Section { name: current, .. }) => {
            *current = name;
            Ok(())
        }
        _ => Err("That section no longer exists".into()),
    }
}

/// Remove a header; its symbols stay and join the section above.
pub fn remove_section(items: &mut Vec<Item>, index: usize) -> bool {
    if items.get(index).is_some_and(Item::is_section) {
        items.remove(index);
        true
    } else {
        false
    }
}

pub fn set_collapsed(items: &mut [Item], index: usize, value: bool) {
    if let Some(Item::Section { collapsed, .. }) = items.get_mut(index) {
        *collapsed = value;
    }
}

/// Cleaned stored sections for `symbols`: valid names only, at most [`MAX_SECTIONS`].
pub fn sanitize(symbols: &[String], sections: Vec<WatchSection>) -> Vec<WatchSection> {
    let mut sections: Vec<WatchSection> =
        sections.into_iter().filter_map(|s| Some(WatchSection { name: section_name(&s.name).ok()?, ..s })).collect();
    sections.truncate(MAX_SECTIONS);
    split(&items(symbols, &sections)).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Item {
        Item::Symbol(s.into())
    }
    fn sec(s: &str) -> Item {
        Item::Section { name: s.into(), collapsed: false }
    }
    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Symbol(s) => s.clone(),
                Item::Section { name, .. } => format!("#{name}"),
            })
            .collect()
    }

    #[test]
    fn items_and_split_round_trip_including_empty_and_trailing_sections() {
        let rows = vec![sym("A"), sec("Empty"), sec("Macro"), sym("B"), sym("C"), sec("Tail")];
        let (symbols, sections) = split(&rows);
        assert_eq!(symbols, ["A", "B", "C"]);
        assert_eq!(sections.iter().map(|s| s.at).collect::<Vec<_>>(), [1, 1, 3]);
        assert_eq!(items(&symbols, &sections), rows);
        let wild = vec![WatchSection { name: "Late".into(), collapsed: true, at: 99 }];
        assert_eq!(names(&items(&symbols, &wild)), ["A", "B", "C", "#Late"]);
    }

    #[test]
    fn resync_keeps_sections_and_order_and_appends_new_symbols() {
        let rows = vec![sec("One"), sym("A"), sym("B"), sec("Two"), sym("C")];
        let out = resync(&rows, &["C".into(), "D".into(), "A".into(), "A".into()]);
        assert_eq!(names(&out), ["#One", "A", "#Two", "C", "D"]);
    }

    #[test]
    fn moving_symbols_crosses_sections_and_ignores_no_op_gaps() {
        let mut rows = vec![sec("One"), sym("A"), sym("B"), sec("Two"), sym("C")];
        assert!(!move_item(&mut rows, 1, 1) && !move_item(&mut rows, 1, 2), "gaps beside the row");
        assert!(move_item(&mut rows, 1, 5));
        assert_eq!(names(&rows), ["#One", "B", "#Two", "C", "A"]);
        assert!(move_item(&mut rows, 4, 1));
        assert_eq!(names(&rows), ["#One", "A", "B", "#Two", "C"]);
        assert!(!move_item(&mut rows, 9, 0));
    }

    #[test]
    fn moving_a_section_carries_its_symbols_and_never_splits_another() {
        let mut rows = vec![sym("U"), sec("One"), sym("A"), sym("B"), sec("Two"), sym("C")];
        assert_eq!(block(&rows, 1), 1..4);
        // Headers land only before another header or at the end, snapping to the nearest:
        // gap 3 (lower half of "One") snaps back beside "Two" itself, gap 2 moves above "One".
        assert!(!move_item(&mut rows, 4, 3));
        assert!(move_item(&mut rows, 4, 2));
        assert_eq!(names(&rows), ["U", "#Two", "C", "#One", "A", "B"]);
        assert!(move_item(&mut rows, 1, 6));
        assert_eq!(names(&rows), ["U", "#One", "A", "B", "#Two", "C"]);
        assert_eq!(snap_section_gap(&rows, 0), 1, "ungrouped symbols stay above the first header");
    }

    #[test]
    fn collapse_hides_only_that_sections_symbols() {
        let mut rows = vec![sym("U"), sec("One"), sym("A"), sec("Two"), sym("B")];
        set_collapsed(&mut rows, 1, true);
        assert_eq!(visible(&rows), [0, 1, 3, 4]);
        assert_eq!(section_of(&rows, 2), Some(1));
        assert_eq!(section_of(&rows, 0), None);
    }

    #[test]
    fn section_edits_validate_names_and_limits() {
        let mut rows = vec![sym("A")];
        assert!(insert_section(&mut rows, 0, "  Macro & ratios ").is_ok());
        assert_eq!(names(&rows), ["#Macro & ratios", "A"]);
        assert!(insert_section(&mut rows, 9, "").is_err());
        assert!(insert_section(&mut rows, 9, &"x".repeat(41)).is_err());
        assert!(rename_section(&mut rows, 1, "Nope").is_err(), "not a header");
        assert!(rename_section(&mut rows, 0, "T4 running").is_ok());
        assert!(remove_section(&mut rows, 0) && !remove_section(&mut rows, 0));
        assert_eq!(names(&rows), ["A"]);
        let mut many: Vec<Item> = (0..MAX_SECTIONS).map(|i| sec(&i.to_string())).collect();
        assert!(insert_section(&mut many, 0, "one too many").is_err());
        let stored = vec![
            WatchSection { name: "\u{200b}".into(), collapsed: false, at: 0 },
            WatchSection { name: "Kept".into(), collapsed: true, at: 5 },
        ];
        assert_eq!(sanitize(&["A".into()], stored), [WatchSection { name: "Kept".into(), collapsed: true, at: 1 }]);
    }

    #[test]
    fn remove_section_keeps_ungrouped_symbols_and_reveals_collapsed_ones() {
        let mut rows = vec![sym("U"), sec("A"), sym("X"), sec("B"), sym("Y")];
        set_collapsed(&mut rows, 1, true);
        assert_eq!(visible(&rows), [0, 1, 3, 4], "X is hidden under collapsed A");
        assert!(remove_section(&mut rows, 1));
        assert_eq!(names(&rows), ["U", "X", "#B", "Y"]);
        assert_eq!(visible(&rows), [0, 1, 2, 3], "X joins the ungrouped run and is shown again");
        // First header with ungrouped symbols above: they simply stay ungrouped.
        let mut rows = vec![sym("U"), sec("A"), sym("X")];
        assert!(remove_section(&mut rows, 1));
        assert_eq!(names(&rows), ["U", "X"]);
        assert!(section_of(&rows, 0).is_none() && section_of(&rows, 1).is_none());
    }

    #[test]
    fn block_end_of_a_section_is_unaffected_by_collapse() {
        let mut rows = vec![sec("A"), sym("X"), sym("Y"), sec("B"), sym("Z")];
        set_collapsed(&mut rows, 0, true);
        assert_eq!(block(&rows, 0), 0..3, "the hidden symbols stay inside the block");
        let mut rows2 = rows.clone();
        insert_section(&mut rows2, block(&rows, 0).end, "Below A").unwrap();
        assert_eq!(names(&rows2), ["#A", "X", "Y", "#Below A", "#B", "Z"]);
    }

    #[test]
    fn insert_section_above_the_first_row_and_an_ungrouped_row() {
        let mut rows = vec![sym("U"), sym("V"), sec("A"), sym("X")];
        insert_section(&mut rows, 0, "Top").unwrap();
        assert_eq!(names(&rows), ["#Top", "U", "V", "#A", "X"]);
        insert_section(&mut rows, 2, "Between").unwrap();
        assert_eq!(names(&rows), ["#Top", "U", "#Between", "V", "#A", "X"]);
    }

    #[test]
    fn section_name_trims_and_rejects_blank_long_and_invisible_input() {
        assert_eq!(section_name("  Macro  ").unwrap(), "Macro");
        assert!(section_name("   ").is_err());
        assert!(section_name("").is_err());
        assert!(section_name(&"x".repeat(41)).is_err());
        assert_eq!(section_name(&"y".repeat(40)).unwrap(), "y".repeat(40));
        assert!(section_name("A\u{200b}B").is_err());
        assert!(section_name("\u{202e}niam").is_err());
        // Duplicates are permitted: nothing in the model keys sections by name.
        let mut rows = vec![sec("Same"), sec("Same")];
        rename_section(&mut rows, 1, "Same").unwrap();
        assert_eq!(names(&rows), ["#Same", "#Same"]);
    }
}
