//! Transactional watchlist column editor and named column-set library.
use egui::{RichText, Ui};
use tr_core::Timeframe;
use tr_core::columns::{ColumnDef, ColumnKind, ColumnSet, MAX_COLUMNS};
use tr_core::scanner::{ConditionGroup, MatchMode, ScanCondition};

use crate::theme::{self, dp, muted};
use crate::widgets::{self, Icon};
use crate::worker::data_dir;

pub(super) struct ColumnsEditor {
    working: ColumnSet,
    selected: Option<u32>,
    saved: Vec<ColumnSet>,
    notice: Option<String>,
    save_name: Option<String>,
}

fn load_saved_at(dir: &std::path::Path) -> Result<Vec<ColumnSet>, String> {
    match std::fs::read(dir.join("column-sets.json")) {
        Ok(bytes) => {
            let sets: Vec<ColumnSet> =
                serde_json::from_slice(&bytes).map_err(|e| format!("Could not read saved column sets: {e}"))?;
            Ok(sets.into_iter().map(|set| set.sanitized().0).collect())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Could not open saved column sets: {e}")),
    }
}

fn save_definition_at(definition: &ColumnSet, dir: &std::path::Path) -> Result<Vec<ColumnSet>, String> {
    definition.validate()?;
    if definition.name.trim().is_empty() {
        return Err("Enter a column set name".into());
    }
    let mut saved = load_saved_at(dir)?;
    saved.retain(|s| !s.name.eq_ignore_ascii_case(definition.name.trim()));
    let mut definition = definition.clone();
    definition.name = definition.name.trim().into();
    saved.push(definition);
    saved.sort_by(|a, b| a.name.cmp(&b.name));
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create column set folder: {e}"))?;
    let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| format!("Could not encode column sets: {e}"))?;
    let tmp = dir.join("column-sets.json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("Could not save column sets: {e}"))?;
    std::fs::rename(tmp, dir.join("column-sets.json")).map_err(|e| format!("Could not replace column sets: {e}"))?;
    Ok(saved)
}

fn move_column(set: &mut ColumnSet, index: usize, up: bool) {
    let target = if up { index.checked_sub(1) } else { index.checked_add(1) };
    if let Some(target) = target
        && index < set.columns.len()
        && target < set.columns.len()
    {
        set.columns.swap(index, target);
    }
}

fn remove_column(set: &mut ColumnSet, index: usize) {
    if index < set.columns.len() {
        set.columns.remove(index);
    }
}

fn apply_enabled(set: &ColumnSet) -> bool {
    set.validate().is_ok()
}

fn new_column(set: &ColumnSet, condition: bool) -> ColumnDef {
    ColumnDef {
        id: set.columns.iter().map(|c| c.id).max().unwrap_or(0).saturating_add(1),
        title: if condition { "Above SMA 20" } else { "SMA 20" }.into(),
        timeframe: Timeframe::D1,
        kind: if condition {
            ColumnKind::Condition(ConditionGroup::comparisons(MatchMode::All, vec![ScanCondition::default()]))
        } else {
            ColumnKind::Formula { expression: "AVGC20".into() }
        },
    }
}

impl ColumnsEditor {
    pub(super) fn new(set: &ColumnSet) -> Self {
        let (saved, notice) = match load_saved_at(&data_dir()) {
            Ok(saved) => (saved, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Self { working: set.clone(), selected: None, saved, notice, save_name: None }
    }

    /// Returns a validated set on Apply and whether the window should stay open.
    pub(super) fn show(&mut self, ctx: &egui::Context) -> (Option<ColumnSet>, bool) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return (None, false);
        }
        let mut open = true;
        let mut applied = None;
        let mut cancel = false;
        let default_size = egui::vec2(dp(520.0), dp(480.0));
        let mut bounds = ctx.content_rect();
        bounds.min.y = bounds.min.y.max(dp(super::chrome::TOOLBAR_H));
        let mut default_pos = bounds.center() - default_size * 0.5;
        default_pos.y = default_pos.y.max(bounds.top());
        egui::Window::new("Edit columns")
            .id(egui::Id::new("watchlist-column-editor"))
            .open(&mut open)
            .resizable(true)
            .default_size(default_size)
            .default_pos(default_pos)
            .constrain(true)
            .constrain_to(bounds)
            .show(ctx, |ui| {
                self.library(ui);
                ui.separator();
                egui::ScrollArea::vertical().max_height(dp(340.0)).show(ui, |ui| self.rows(ui));
                ui.horizontal_wrapped(|ui| {
                    for (label, condition) in [("Add formula column", false), ("Add condition column", true)] {
                        if ui
                            .add_enabled(self.working.columns.len() < MAX_COLUMNS, egui::Button::new(label))
                            .on_disabled_hover_text("A watchlist can show up to 8 columns")
                            .clicked()
                        {
                            let column = new_column(&self.working, condition);
                            self.selected = Some(column.id);
                            self.working.columns.push(column);
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let error = self.working.validate().err();
                    let mut response = ui.add_enabled(apply_enabled(&self.working), egui::Button::new("Apply"));
                    if let Some(error) = error {
                        response = response.on_disabled_hover_text(error);
                    }
                    if response.clicked() {
                        applied = Some(self.working.clone());
                    }
                    cancel = ui.button("Cancel").clicked();
                });
            });
        let keep_open = open && !cancel && applied.is_none();
        (applied, keep_open)
    }

    fn rows(&mut self, ui: &mut Ui) {
        let mut action = None;
        let count = self.working.columns.len();
        for (index, column) in self.working.columns.iter_mut().enumerate() {
            ui.push_id(("column", column.id, index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.selectable_label(self.selected == Some(column.id), format!("{}", index + 1)).clicked() {
                        self.selected = if self.selected == Some(column.id) { None } else { Some(column.id) };
                    }
                    if ui.add(egui::TextEdit::singleline(&mut column.title).desired_width(dp(130.0))).has_focus() {
                        self.selected = Some(column.id);
                    }
                    ui.label(
                        RichText::new(match column.kind {
                            ColumnKind::Formula { .. } => "Formula",
                            ColumnKind::Condition(_) => "Condition",
                        })
                        .color(muted()),
                    );
                    egui::ComboBox::from_id_salt("timeframe").selected_text(column.timeframe.label()).show_ui(
                        ui,
                        |ui| {
                            for tf in Timeframe::ALL {
                                ui.selectable_value(&mut column.timeframe, tf, tf.label());
                            }
                        },
                    );
                    if ui.add_enabled(index > 0, egui::Button::new("Up").small()).clicked() {
                        action = Some((index, Some(true)));
                    }
                    if ui.add_enabled(index + 1 < count, egui::Button::new("Down").small()).clicked() {
                        action = Some((index, Some(false)));
                    }
                    if widgets::icon_button(ui, Icon::Close, "Remove column", false).clicked() {
                        action = Some((index, None));
                    }
                });
                if self.selected == Some(column.id) {
                    match &mut column.kind {
                        ColumnKind::Formula { expression } => {
                            ui.add(egui::TextEdit::singleline(expression).desired_width(f32::INFINITY));
                        }
                        ColumnKind::Condition(group) => {
                            super::scanner_view::condition_group_editor(ui, group, column.timeframe, "conditions");
                        }
                    }
                    if let Err(error) = column.validate() {
                        ui.label(RichText::new(error).small().color(theme::down()));
                    }
                }
                ui.separator();
            });
        }
        if let Some((index, direction)) = action {
            match direction {
                Some(up) => move_column(&mut self.working, index, up),
                None => remove_column(&mut self.working, index),
            }
        }
    }

    fn library(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let mut loaded = None;
            egui::ComboBox::from_id_salt("load-column-set").selected_text("Load set").show_ui(ui, |ui| {
                for (index, set) in self.saved.iter().enumerate() {
                    if ui.selectable_label(false, &set.name).clicked() {
                        loaded = Some(index);
                    }
                }
            });
            if let Some(index) = loaded {
                self.working = self.saved[index].clone();
                self.selected = None;
            }
            if ui.button("Save set…").clicked() {
                self.save_name = Some(self.working.name.clone());
            }
        });
        if let Some(name) = &mut self.save_name {
            let mut save = false;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(name).hint_text("Set name"));
                save = ui.button("Save").clicked();
            });
            if save {
                let mut definition = self.working.clone();
                definition.name = name.clone();
                match save_definition_at(&definition, &data_dir()) {
                    Ok(saved) => {
                        self.saved = saved;
                        self.working.name = definition.name.trim().into();
                        self.save_name = None;
                        self.notice = Some("Column set saved".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
        }
        if let Some(notice) = &self.notice {
            ui.label(RichText::new(notice).small().color(muted()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ColumnSet {
        let mut set = ColumnSet::default();
        set.columns.push(new_column(&set, false));
        set.columns.push(new_column(&set, true));
        set
    }

    #[test]
    fn column_editor_renders_headless_without_changes() {
        let set = sample();
        let mut editor =
            ColumnsEditor { working: set.clone(), selected: None, saved: Vec::new(), notice: None, save_name: None };
        let ctx = egui::Context::default();
        for selected in [None, Some(1), Some(2)] {
            editor.selected = selected;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let (applied, open) = editor.show(ui.ctx());
                assert!(applied.is_none());
                assert!(open);
            });
            output.textures_delta.clear();
            assert_eq!(editor.working, set);
        }
    }

    #[test]
    fn column_move_up_down_and_remove() {
        let mut set = sample();
        let original = set.clone();
        move_column(&mut set, 0, true);
        move_column(&mut set, 1, false);
        assert_eq!(set, original);
        move_column(&mut set, 0, false);
        assert_eq!(set.columns[0].id, 2);
        move_column(&mut set, 1, true);
        assert_eq!(set, original);
        remove_column(&mut set, 99);
        remove_column(&mut set, 0);
        assert_eq!(set.columns[0].id, 2);
        assert_eq!(new_column(&set, false).id, 3);
    }

    #[test]
    fn column_apply_is_gated_on_whole_set_validation() {
        let mut set = sample();
        assert!(apply_enabled(&set));
        set.columns[0].kind = ColumnKind::Formula { expression: "INVALID(".into() };
        assert!(!apply_enabled(&set));
        set = sample();
        set.columns[1].title.clear();
        assert!(!apply_enabled(&set));
        set.columns = vec![sample().columns[0].clone(); 9];
        assert!(!apply_enabled(&set));
    }

    #[test]
    fn column_sets_library_merges_updates_and_preserves_corrupt_files() {
        // Keep all test writes within this worktree.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "target/column-library-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut definition = sample();
        definition.name = "First".into();
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 1);
        definition.name = "Second".into();
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 2);
        definition.name = " first ".into();
        definition.columns[0].timeframe = Timeframe::W1;
        let saved = save_definition_at(&definition, &dir).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|s| s.name == "first" && s.columns[0].timeframe == Timeframe::W1));
        assert_eq!(load_saved_at(&dir).unwrap(), saved);
        let mut invalid = definition.clone();
        invalid.columns[0].title.clear();
        std::fs::write(dir.join("column-sets.json"), serde_json::to_vec(&vec![invalid]).unwrap()).unwrap();
        assert_eq!(load_saved_at(&dir).unwrap()[0].columns.len(), 1);
        std::fs::write(dir.join("column-sets.json"), b"broken JSON").unwrap();
        assert!(save_definition_at(&definition, &dir).is_err());
        assert_eq!(std::fs::read(dir.join("column-sets.json")).unwrap(), b"broken JSON");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
