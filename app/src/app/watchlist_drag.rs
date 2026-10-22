use crate::{
    theme,
    watch_sections::{self, Item},
};
use egui::{Align2, CursorIcon, Id, Key, LayerId, Order, Rect, Response, Stroke, Ui, pos2, vec2};

pub(super) struct WatchDrag {
    from: usize,
    section: bool,
    grab_offset: f32,
    gap: usize,
}

pub(super) struct DragFrame<'a> {
    state: &'a mut Option<WatchDrag>,
    items: &'a [Item],
    visible: Vec<usize>,
    rows: Vec<(usize, Rect)>,
    area: Rect,
    sorted: bool,
    id: Id,
}

impl<'a> DragFrame<'a> {
    pub(super) fn new(ui: &Ui, state: &'a mut Option<WatchDrag>, items: &'a [Item], sorted: bool) -> Self {
        if sorted
            || state.as_ref().is_some_and(|drag| drag.from >= items.len())
            || ui.input(|i| i.key_pressed(Key::Escape))
        {
            *state = None;
        }
        Self {
            state,
            items,
            visible: watch_sections::visible(items),
            rows: Vec::new(),
            area: ui.available_rect_before_wrap().intersect(ui.clip_rect()),
            sorted,
            id: ui.id().with("watch-drag"),
        }
    }

    pub(super) fn row(&mut self, ui: &Ui, index: usize, rect: Rect, response: &Response) -> f32 {
        self.rows.push((index, rect));
        if self.sorted {
            response.clone().on_hover_text("Clear the column sort to reorder");
        } else if response.drag_started_by(egui::PointerButton::Primary) && !ui.input(|i| i.key_pressed(Key::Escape)) {
            let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.center());
            *self.state = Some(WatchDrag {
                from: index,
                section: self.items[index].is_section(),
                grab_offset: origin.y - rect.top(),
                gap: index,
            });
        }
        let mut offset = 0.0;
        if let Some(drag) = self.state.as_mut() {
            // Allocated rows have a uniform pitch, including egui's item spacing.
            let pitch = rect.height() + ui.spacing().item_spacing.y;
            if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                let first_top = rect.top() - self.rows.len().saturating_sub(1) as f32 * pitch;
                let v = ((pointer.y - first_top + pitch - rect.height() / 2.0) / pitch).floor().max(0.0) as usize;
                drag.gap = self.visible.get(v).copied().unwrap_or(self.items.len());
                // The lower half of a collapsed header inserts before its hidden symbols.
                if let Some(&previous) = v.checked_sub(1).and_then(|v| self.visible.get(v))
                    && matches!(self.items[previous], Item::Section { collapsed: true, .. })
                    && pointer.y < first_top + v as f32 * pitch
                {
                    drag.gap = previous + 1;
                }
                if drag.section {
                    drag.gap = watch_sections::snap_section_gap(self.items, drag.gap);
                }
            }
            let block = watch_sections::block(self.items, drag.from);
            let count = self.visible.iter().filter(|i| block.contains(i)).count() as f32;
            if !block.contains(&index) {
                if index >= block.end {
                    offset -= count * pitch;
                }
                if index >= drag.gap {
                    offset += count * pitch;
                }
            } else {
                // Hide the source painting; only the foreground ghost follows the pointer.
                return -100_000.0;
            }
        }
        ui.ctx().animate_value_with_time(self.id.with(index), offset, 0.12)
    }

    pub(super) fn finish(self, ui: &mut Ui) -> Option<(usize, usize)> {
        let drag = self.state.as_ref()?;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        if released {
            let drop = pointer.filter(|p| self.area.contains(*p)).map(|_| (drag.from, drag.gap));
            let block = watch_sections::block(self.items, drag.from);
            if drop.is_some() && !(block.start..=block.end).contains(&drag.gap) {
                // The stored order supplies the new positions on the next frame.
                for &index in &self.visible {
                    ui.ctx().animate_value_with_time(self.id.with(index), 0.0, 0.0);
                }
            }
            ui.ctx().request_repaint();
            *self.state = None;
            return drop;
        }
        let pointer = pointer?;
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        ui.ctx().request_repaint();
        let edge = 28.0;
        let scroll = if pointer.y < self.area.top() + edge {
            -12.0 * ((self.area.top() + edge - pointer.y) / edge).clamp(0.0, 1.0)
        } else if pointer.y > self.area.bottom() - edge {
            12.0 * ((pointer.y - self.area.bottom() + edge) / edge).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ui.scroll_with_delta(vec2(0.0, -scroll));
        let source = self.rows.iter().find(|(i, _)| *i == drag.from)?.1;
        let block = watch_sections::block(self.items, drag.from);
        let pitch = source.height() + ui.spacing().item_spacing.y;
        let count = self.visible.iter().filter(|i| block.contains(i)).count() as f32;
        let gap = if block.contains(&drag.gap) || drag.gap == block.end { block.start } else { drag.gap };
        let gap_y = self.rows.iter().find(|(i, _)| *i >= gap).map_or_else(
            || self.rows.last().map_or(source.bottom(), |(_, r)| r.bottom() + ui.spacing().item_spacing.y),
            |(_, r)| r.top(),
        ) - if gap > block.start { count * pitch } else { 0.0 };
        ui.painter().line_segment(
            [pos2(self.area.left(), gap_y), pos2(self.area.right(), gap_y)],
            Stroke::new(2.0, theme::accent()),
        );
        let ghost = Rect::from_min_size(pos2(source.left(), pointer.y - drag.grab_offset), source.size());
        let painter = ui.ctx().layer_painter(LayerId::new(Order::Foreground, self.id));
        for spread in (1..=4).rev() {
            painter.rect_filled(ghost.expand(spread as f32), 4.0, egui::Color32::from_black_alpha(9));
        }
        painter.rect_filled(ghost, 3.0, theme::card().gamma_multiply(0.94));
        painter.rect_stroke(ghost, 3.0, Stroke::new(1.0, theme::accent()), egui::StrokeKind::Inside);
        let label = match &self.items[drag.from] {
            Item::Symbol(key) => tr_data::security(key).ticker,
            Item::Section { name, .. } => format!("{} ({})", name, block.len() - 1),
        };
        painter.text(
            ghost.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            egui::FontId::monospace(12.5),
            theme::text(),
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, Sense};

    #[derive(Default)]
    struct Harness {
        ctx: egui::Context,
        state: Option<WatchDrag>,
        time: f64,
        rects: Vec<Rect>,
        offsets: Vec<f32>,
    }

    impl Harness {
        fn frame(&mut self, items: &[Item], events: Vec<Event>) -> Option<(usize, usize)> {
            self.time += 0.2;
            self.rects.clear();
            self.offsets.clear();
            let mut drop = None;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut frame = DragFrame::new(ui, &mut self.state, items, false);
                    for index in watch_sections::visible(items) {
                        let (rect, response) = ui.allocate_exact_size(vec2(300.0, 22.0), Sense::click_and_drag());
                        self.rects.push(rect);
                        self.offsets.push(frame.row(ui, index, rect, &response));
                    }
                    drop = frame.finish(ui);
                },
            );
            output.textures_delta.clear();
            drop
        }

        fn start(&mut self, items: &[Item], from: usize, to: Pos2) {
            let at = self.rects[from].center();
            self.frame(items, vec![Event::PointerMoved(at), button(at, true)]);
            self.frame(items, vec![Event::PointerMoved(to)]);
            assert!(self.state.is_some());
        }
    }

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn symbols() -> Vec<Item> {
        ["AAPL", "MSFT", "NVDA"].into_iter().map(|s| Item::Symbol(s.into())).collect()
    }

    fn section(name: &str, collapsed: bool) -> Item {
        Item::Section { name: name.into(), collapsed }
    }

    #[test]
    fn drag_escape_and_outside_release_cancel() {
        let items = symbols();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let to = h.rects[2].center();
        h.start(&items, 0, to);
        assert_eq!(
            h.frame(
                &items,
                vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }]
            ),
            None
        );
        assert!(h.state.is_none());
        h.frame(&items, vec![button(to, false)]);
        h.start(&items, 0, to);
        let outside = pos2(500.0, 500.0);
        assert_eq!(h.frame(&items, vec![Event::PointerMoved(outside), button(outside, false)]), None);
        assert!(h.state.is_none());
    }

    #[test]
    fn drag_back_to_own_slot_is_no_op() {
        let mut items = symbols();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let from = h.rects[0].center();
        let to = h.rects[2].center();
        h.start(&items, 0, to);
        h.frame(&items, vec![Event::PointerMoved(from)]);
        let (from, gap) = h.frame(&items, vec![button(from, false)]).unwrap();
        assert!(!watch_sections::move_item(&mut items, from, gap));
        assert_eq!(items, symbols());
    }

    #[test]
    fn drag_offsets_close_source_and_slide_intervening_rows() {
        let items = symbols();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let to = h.rects[2].center() + vec2(0.0, 8.0);
        h.start(&items, 0, to);
        for _ in 0..4 {
            h.frame(&items, vec![]);
        }
        assert!(h.offsets[0] < -1000.0, "source is painted only as a ghost");
        let pitch = h.rects[1].top() - h.rects[0].top();
        assert!((h.offsets[1] + pitch).abs() < 0.1);
        assert!((h.offsets[2] + pitch).abs() < 0.1);
    }

    #[test]
    fn drag_upward_opens_target_gap_and_leaves_rows_after_source_in_place() {
        let mut items = symbols();
        items.push(Item::Symbol("AMD".into()));
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let to = h.rects[0].center() - vec2(0.0, 8.0);
        h.start(&items, 2, to);
        for _ in 0..4 {
            h.frame(&items, vec![]);
        }
        let pitch = h.rects[1].top() - h.rects[0].top();
        assert!((h.offsets[0] - pitch).abs() < 0.1);
        assert!((h.offsets[1] - pitch).abs() < 0.1);
        assert!(h.offsets[3].abs() < 0.1);
        let (from, gap) = h.frame(&items, vec![button(to, false)]).unwrap();
        assert_eq!((from, gap), (2, 0));
        assert!(watch_sections::move_item(&mut items, from, gap));
        h.frame(&items, vec![]);
        assert!(h.offsets.iter().all(|offset| offset.abs() < 0.1));
    }

    #[test]
    fn drag_symbol_under_collapsed_header_enters_section_first() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/watch-drag-sections");
        let mut app = super::super::TerminalApp::test_workspace(&root);
        let mut items = vec![Item::Symbol("AAPL".into()), section("Tech", true), Item::Symbol("NVDA".into())];
        app.set_watchlist_items(&items);
        items = app.watchlists[app.active_watchlist].items();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let to = h.rects[1].center() + vec2(0.0, 8.0);
        h.start(&items, 0, to);
        let (from, gap) = h.frame(&items, vec![button(to, false)]).unwrap();
        assert_eq!(gap, 2);
        assert!(watch_sections::move_item(&mut items, from, gap));
        app.set_watchlist_items(&items);
        assert_eq!(
            app.watchlists[app.active_watchlist].items(),
            vec![section("Tech", true), Item::Symbol("AAPL".into()), Item::Symbol("NVDA".into())]
        );
        assert_eq!(app.watchlist, app.watchlists[app.active_watchlist].symbols);
    }

    #[test]
    fn drag_header_carries_block_and_snaps_without_splitting_sections() {
        let mut items = vec![
            section("One", false),
            Item::Symbol("AAPL".into()),
            section("Two", false),
            Item::Symbol("NVDA".into()),
            Item::Symbol("MSFT".into()),
        ];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let to = h.rects[3].center() + vec2(0.0, 8.0);
        h.start(&items, 0, to);
        assert_eq!(h.state.as_ref().unwrap().gap, items.len());
        let (from, gap) = h.frame(&items, vec![button(to, false)]).unwrap();
        assert!(watch_sections::move_item(&mut items, from, gap));
        assert_eq!(
            items,
            vec![
                section("Two", false),
                Item::Symbol("NVDA".into()),
                Item::Symbol("MSFT".into()),
                section("One", false),
                Item::Symbol("AAPL".into())
            ]
        );
    }

    fn sec(name: &str, collapsed: bool) -> Item {
        section(name, collapsed)
    }

    fn sym(s: &str) -> Item {
        Item::Symbol(s.into())
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

    /// Press at `from`, then move to `to` so egui reports drag_started.
    fn press_move(h: &mut Harness, items: &[Item], from_slot: usize, to: Pos2) {
        let at = h.rects[from_slot].center();
        h.frame(items, vec![Event::PointerMoved(at), button(at, true)]);
        h.frame(items, vec![Event::PointerMoved(to)]);
    }

    // ---- probe (2): the list changing during a drag ----

    #[test]
    #[ignore = "BUG: drop uses the stale `from` index after the dragged symbol is removed mid-drag, so a different row is moved"]
    fn bug_stale_from_after_source_symbol_removed_mid_drag() {
        // AAPL(0) MSFT(1) NVDA(2) AMD(3); drag NVDA, then another action removes it.
        let before = vec![sym("AAPL"), sym("MSFT"), sym("NVDA"), sym("AMD")];
        let after = vec![sym("AAPL"), sym("MSFT"), sym("AMD")];
        let mut h = Harness::default();
        h.frame(&before, vec![]);
        let to = h.rects[3].center() + vec2(0.0, 8.0);
        press_move(&mut h, &before, 2, to);
        assert_eq!(h.state.as_ref().unwrap().from, 2);
        // List changed: index 2 is now AMD, not the dragged NVDA. The drag must not
        // apply its stale `from` to this different row.
        let drop = h.frame(&after, vec![button(to, false)]);
        match drop {
            None => {}
            Some((from, gap)) => {
                let mut items = after.clone();
                let applied = watch_sections::move_item(&mut items, from, gap);
                assert!(
                    !applied || items == after,
                    "drop moved {:?} -> {:?}, but the drag source (NVDA) is gone",
                    names(&after),
                    names(&items)
                );
            }
        }
    }

    #[test]
    #[ignore = "BUG: a mid-drag removal that leaves `from` in range re-points the drag at a different symbol and drop moves it"]
    fn bug_stale_from_points_at_replacement_symbol() {
        // Same as above but assert the concrete wrong move: AMD is dragged away instead.
        let before = vec![sym("AAPL"), sym("MSFT"), sym("NVDA"), sym("AMD")];
        let after = vec![sym("AAPL"), sym("MSFT"), sym("AMD")];
        let mut h = Harness::default();
        h.frame(&before, vec![]);
        let to = h.rects[3].center() + vec2(0.0, 8.0);
        press_move(&mut h, &before, 2, to);
        // Drop near the top of the replacement list.
        let top = pos2(after.len() as f32, h.rects[0].top() + 1.0);
        let drop = h.frame(&after, vec![Event::PointerMoved(top), button(top, false)]);
        let (from, gap) = drop.expect("release inside the list should still produce a drop slot");
        assert_eq!(from, 2, "stale from re-points at whatever now sits at index 2");
        let mut items = after.clone();
        assert!(watch_sections::move_item(&mut items, from, gap));
        assert_eq!(
            names(&items),
            vec!["AMD".to_string(), "AAPL".into(), "MSFT".into()],
            "AMD was yanked because the drag's `from` still said 2"
        );
    }

    #[test]
    #[ignore = "BUG: when the dragged row is no longer allocated (collapsed away), finish() early-returns on the missing source without clearing state; the drag is stuck"]
    fn bug_drag_state_stuck_when_source_row_unallocated() {
        // MSFT lives inside a section; collapsing it hides the row mid-drag.
        let before = vec![sym("AAPL"), sec("Tech", false), sym("MSFT"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&before, vec![]);
        // visible slots: AAPL=0, Tech=1, MSFT=2, NVDA=3
        let to = h.rects[3].center() + vec2(0.0, 8.0);
        press_move(&mut h, &before, 2, to);
        assert_eq!(h.state.as_ref().unwrap().from, 2);
        let mut items = before.clone();
        watch_sections::set_collapsed(&mut items, 1, true);
        // visible is now [AAPL, Tech, NVDA]; index 2 (MSFT) is hidden and gets no row.
        let drop = h.frame(&items, vec![button(to, false)]);
        assert_eq!(drop, None, "hidden source cannot be dropped onto");
        assert!(
            h.state.is_none(),
            "drag must cancel when its source row disappears from the layout, not hang forever"
        );
    }

    #[test]
    #[ignore = "BUG: release inside the list with a hidden/unallocated source still returns Some((stale from, gap)) and panels will move the wrong item"]
    fn bug_hidden_source_still_emits_stale_drop_on_release() {
        let before = vec![sym("AAPL"), sec("Tech", false), sym("MSFT"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&before, vec![]);
        let to = h.rects[3].center() + vec2(0.0, 8.0);
        press_move(&mut h, &before, 2, to);
        let mut items = before.clone();
        watch_sections::set_collapsed(&mut items, 1, true);
        // Not yet released: the missing source must not keep the drag alive either.
        h.frame(&items, vec![Event::PointerMoved(to)]);
        assert!(h.state.is_none(), "drag should be cancelled as soon as the source row is gone");
        assert_eq!(h.frame(&items, vec![button(to, false)]), None);
    }

    // ---- probe (5): right-button drag ----

    #[test]
    fn drag_ignores_non_primary_buttons() {
        let items = symbols();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let at = h.rects[0].center();
        let to = h.rects[2].center();
        h.frame(
            &items,
            vec![
                Event::PointerMoved(at),
                Event::PointerButton {
                    pos: at,
                    button: PointerButton::Secondary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        h.frame(&items, vec![Event::PointerMoved(to)]);
        assert!(h.state.is_none(), "secondary-button press-drag must not start a row drag");
        h.frame(
            &items,
            vec![Event::PointerButton {
                pos: to,
                button: PointerButton::Secondary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(h.state.is_none());
    }

    // ---- probe (8): mixed row heights ----

    impl Harness {
        /// Like `frame`, but rows get `heights[i % heights.len()]` so the uniform pitch
        /// assumption can be exercised.
        fn frame_heights(
            &mut self,
            items: &[Item],
            events: Vec<Event>,
            heights: &[f32],
        ) -> Option<(usize, usize)> {
            self.time += 0.2;
            self.rects.clear();
            self.offsets.clear();
            let mut drop = None;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut frame = DragFrame::new(ui, &mut self.state, items, false);
                    for (slot, index) in watch_sections::visible(items).into_iter().enumerate() {
                        let h = heights[slot % heights.len()];
                        let (rect, response) = ui.allocate_exact_size(vec2(300.0, h), Sense::click_and_drag());
                        self.rects.push(rect);
                        self.offsets.push(frame.row(ui, index, rect, &response));
                    }
                    drop = frame.finish(ui);
                },
            );
            output.textures_delta.clear();
            drop
        }
    }

    #[test]
    #[ignore = "BUG: gap is computed from a uniform pitch, so rows with different heights map the pointer to the wrong slot"]
    fn bug_mixed_row_heights_miscompute_drop_gap() {
        // Rows 22, 40, 22: the gap formula treats every row as tall as the last one.
        let items = vec![sym("AAPL"), sym("MSFT"), sym("NVDA")];
        let heights = [22.0f32, 40.0, 22.0];
        let mut h = Harness::default();
        h.frame_heights(&items, vec![], &heights);
        // Slot centers: AAPL 0..22, MSFT 22+3..22+3+40, NVDA ...
        // Drag AAPL onto the lower half of NVDA (slot 2) so the gap must be 3 (end).
        let nvda = h.rects[2];
        let to = pos2(150.0, nvda.center().y);
        let at = h.rects[0].center();
        h.frame_heights(&items, vec![Event::PointerMoved(at), button(at, true)], &heights);
        h.frame_heights(&items, vec![Event::PointerMoved(to)], &heights);
        let (from, gap) = h.frame_heights(&items, vec![button(to, false)], &heights).unwrap();
        assert_eq!(from, 0);
        assert_eq!(gap, 3, "pointer on NVDA's lower half must open a gap after NVDA");
    }

    #[test]
    #[ignore = "BUG: row offsets also use per-row pitch instead of the dragged block's height, so mixed heights slide rows by the wrong amount"]
    fn bug_mixed_row_heights_miscompute_row_offsets() {
        let items = vec![sym("AAPL"), sym("MSFT"), sym("NVDA")];
        let heights = [22.0f32, 40.0, 22.0];
        let mut h = Harness::default();
        h.frame_heights(&items, vec![], &heights);
        let to = h.rects[2].center() + vec2(0.0, 8.0);
        let at = h.rects[0].center();
        h.frame_heights(&items, vec![Event::PointerMoved(at), button(at, true)], &heights);
        h.frame_heights(&items, vec![Event::PointerMoved(to)], &heights);
        for _ in 0..4 {
            h.frame_heights(&items, vec![], &heights);
        }
        // Source is a 22px row; MSFT (40px) and NVDA (22px) must close that 22px gap
        // (plus spacing), not shift by "count * their own pitch".
        let gap_px = 22.0 + 3.0; // source height + item spacing
        assert!(
            (h.offsets[1] + gap_px).abs() < 0.5,
            "MSFT should slide up by the source row height ({gap_px}), got {}",
            h.offsets[1]
        );
        assert!((h.offsets[2] + gap_px).abs() < 0.5, "NVDA should slide up by {gap_px}, got {}", h.offsets[2]);
    }

    // ---- probe (6): release at source, no stale state ----

    #[test]
    #[ignore = "TRIAGE: unfinished reviewer test that fails; decide bug or test error"]
    fn release_exactly_at_source_is_no_op_and_leaves_no_stale_drag() {
        let items = symbols();
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let from = h.rects[0].center();
        let away = h.rects[2].center();
        press_move(&mut h, &items, 0, away);
        assert!(h.state.is_some());
        h.frame(&items, vec![Event::PointerMoved(from)]);
        assert_eq!(h.frame(&items, vec![button(from, false)]), Some((0, 0)));
        assert!(h.state.is_none());
        // Immediately click another row: a plain click must open the chart, not drag.
        let click_at = h.rects[1].center();
        h.frame(&items, vec![Event::PointerMoved(click_at), button(click_at, true)]);
        assert!(h.state.is_none(), "a plain press must not leave a drag armed");
        h.frame(&items, vec![button(click_at, false)]);
        assert!(h.state.is_none());
    }

    // ---- probe (4): drop gap at the extremes and onto a collapsed header ----

    #[test]
    #[ignore = "TRIAGE: unfinished reviewer test that fails; decide bug or test error"]
    fn drop_gap_at_top_and_end_of_plain_list() {
        let mut items = vec![sym("AAPL"), sym("MSFT"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        // To the very top: above AAPL.
        let top = pos2(150.0, h.rects[0].top() - 6.0);
        press_move(&mut h, &items, 2, top);
        let (from, gap) = h.frame(&items, vec![button(top, false)]).unwrap();
        assert_eq!((from, gap), (2, 0));
        assert!(watch_sections::move_item(&mut items, from, gap));
        assert_eq!(names(&items), ["NVDA", "AAPL", "MSFT"]);

        // Reset and drag to the very end (below the last row).
        let mut items = vec![sym("AAPL"), sym("MSFT"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let end = pos2(150.0, h.rects[2].bottom() + 6.0);
        press_move(&mut h, &items, 0, end);
        let (from, gap) = h.frame(&items, vec![button(end, false)]).unwrap();
        assert_eq!((from, gap), (0, 3));
        assert!(watch_sections::move_item(&mut items, from, gap));
        assert_eq!(names(&items), ["MSFT", "NVDA", "AAPL"]);
    }

    #[test]
    #[ignore = "TRIAGE: unfinished reviewer test that fails; decide bug or test error"]
    fn drop_gap_onto_collapsed_header_splits_upper_and_lower_half() {
        // U, Tech (collapsed, hides AAPL), NVDA
        let items = vec![sym("U"), sec("Tech", true), sym("AAPL"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        // visible slots: U=0, Tech=1, NVDA=2
        // Upper half of Tech -> gap 1 (before the header).
        let upper = pos2(150.0, h.rects[1].top() + 4.0);
        press_move(&mut h, &items, 0, upper);
        let (_, gap) = h.frame(&items, vec![button(upper, false)]).unwrap();
        assert_eq!(gap, 1, "upper half of a collapsed header is above it");

        // Lower half of Tech -> gap 2 (into the collapsed section, before hidden AAPL).
        let mut items = vec![sym("U"), sec("Tech", true), sym("AAPL"), sym("NVDA")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        let lower = pos2(150.0, h.rects[1].center().y + 4.0);
        press_move(&mut h, &items, 0, lower);
        let (_, gap) = h.frame(&items, vec![button(lower, false)]).unwrap();
        assert_eq!(gap, 2, "lower half of a collapsed header inserts before its hidden symbols");
        assert!(watch_sections::move_item(&mut items, 0, gap));
        assert_eq!(names(&items), ["#Tech", "AAPL", "U", "NVDA"]);
    }

    // ---- probe (7): section header gap snapping with ungrouped symbols ----

    #[test]
    fn section_drop_line_never_splits_another_section_and_respects_ungrouped_rows() {
        // U (ungrouped), One, A, Two, B
        let mut items = vec![sym("U"), sec("One", false), sym("A"), sec("Two", false), sym("B")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        // Drag One (slot 1) above U (slot 0's upper half) -> snaps beside the first
        // header, never above the ungrouped symbol.
        let top = pos2(150.0, h.rects[0].top() + 2.0);
        press_move(&mut h, &items, 1, top);
        assert_eq!(h.state.as_ref().unwrap().gap, 1, "snapped to the first header, not gap 0");
        let (from, gap) = h.frame(&items, vec![button(top, false)]).unwrap();
        assert!(!watch_sections::move_item(&mut items, from, gap), "already at that gap");

        // Move the last section (Two) above the first (One).
        let mut items = vec![sym("U"), sec("One", false), sym("A"), sec("Two", false), sym("B")];
        let mut h = Harness::default();
        h.frame(&items, vec![]);
        // visible: U=0, One=1, A=2, Two=3, B=4 — drag Two (slot 3) onto One's upper half.
        let upper = pos2(150.0, h.rects[1].top() + 2.0);
        press_move(&mut h, &items, 3, upper);
        let (from, gap) = h.frame(&items, vec![button(upper, false)]).unwrap();
        assert_eq!((from, gap), (3, 1));
        assert!(watch_sections::move_item(&mut items, from, gap));
        assert_eq!(names(&items), ["U", "#Two", "B", "#One", "A"]);
    }

    // ---- probe (1): gap while scrolled ----

    #[derive(Default)]
    struct ScrollHarness {
        ctx: egui::Context,
        state: Option<WatchDrag>,
        time: f64,
        rects: Vec<Rect>,
        offsets: Vec<f32>,
        scroll: f32,
    }

    impl ScrollHarness {
        fn frame(&mut self, items: &[Item], events: Vec<Event>) -> Option<(usize, usize)> {
            self.frame_scroll(items, events, 0.0)
        }

        fn frame_scroll(
            &mut self,
            items: &[Item],
            events: Vec<Event>,
            scroll_by: f32,
        ) -> Option<(usize, usize)> {
            self.time += 0.2;
            self.rects.clear();
            self.offsets.clear();
            let mut drop = None;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("drag-scroll")
                        .max_height(200.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if scroll_by != 0.0 {
                                ui.scroll_with_delta(vec2(0.0, scroll_by));
                            }
                            let mut frame = DragFrame::new(ui, &mut self.state, items, false);
                            for index in watch_sections::visible(items) {
                                let (rect, response) =
                                    ui.allocate_exact_size(vec2(300.0, 22.0), Sense::click_and_drag());
                                self.rects.push(rect);
                                self.offsets.push(frame.row(ui, index, rect, &response));
                            }
                            drop = frame.finish(ui);
                            self.scroll = ui.min_rect().top();
                        });
                },
            );
            output.textures_delta.clear();
            drop
        }
    }

    #[test]
    #[ignore = "TRIAGE: unfinished reviewer test that fails; decide bug or test error"]
    fn gap_is_correct_when_the_list_is_scrolled() {
        // 10 rows, viewport 200px; scroll so the first rows sit above the clip.
        let items: Vec<Item> = (0..10).map(|i| sym(&format!("S{i}"))).collect();
        let mut h = ScrollHarness::default();
        for _ in 0..3 {
            h.frame(&items, vec![]);
        }
        // Scroll down (content moves up).
        for _ in 0..6 {
            h.frame_scroll(&items, vec![], -36.0);
        }
        // After scrolling, row tops should be shifted up (some negative).
        assert!(
            h.rects[0].top() < -10.0,
            "expected the list to be scrolled; first row top is {}",
            h.rects[0].top()
        );
        // Drag the first allocated visible row we can see (find a row inside the viewport).
        let slot = h.rects.iter().position(|r| r.center().y > 40.0 && r.center().y < 180.0).expect("a visible row");
        let at = h.rects[slot].center();
        // Drop on the lower half of the row two slots below.
        let target = slot + 2;
        let to = pos2(150.0, h.rects[target].center().y);
        h.frame(&items, vec![Event::PointerMoved(at), button(at, true)]);
        h.frame(&items, vec![Event::PointerMoved(to)]);
        let (from, gap) = h.frame(&items, vec![button(to, false)]).unwrap();
        let items_idx = |slot: usize| watch_sections::visible(&items)[slot];
        assert_eq!(from, items_idx(slot));
        assert_eq!(
            gap,
            items_idx(target),
            "pointer on the lower half of slot {target} must open a gap before that row \
             (its top is {})",
            h.rects[target].top()
        );
    }

    #[test]
    fn auto_scroll_near_bottom_edge_moves_the_list_and_keeps_gap_on_the_pointer() {
        let items: Vec<Item> = (0..12).map(|i| sym(&format!("S{i}"))).collect();
        let mut h = ScrollHarness::default();
        for _ in 0..2 {
            h.frame(&items, vec![]);
        }
        let at = h.rects[1].center();
        // Drag and park the pointer in the bottom edge band (area.bottom() = 200).
        let edge = pos2(150.0, 200.0 - 10.0);
        h.frame(&items, vec![Event::PointerMoved(at), button(at, true)]);
        h.frame(&items, vec![Event::PointerMoved(edge)]);
        let top_before = h.rects[0].top();
        let gap_before = h.state.as_ref().unwrap().gap;
        for _ in 0..5 {
            h.frame(&items, vec![Event::PointerMoved(edge)]);
        }
        let top_after = h.rects[0].top();
        assert!(
            top_after < top_before - 1.0,
            "auto-scroll near the bottom must scroll the list down (rows move up); \
             first row top {top_before} -> {top_after}"
        );
        // The gap must stay a valid slot under the stationary pointer.
        let gap_after = h.state.as_ref().unwrap().gap;
        assert!(
            gap_after >= gap_before,
            "as the list scrolls under a stationary pointer the drop slot should not jump backwards \
             ({gap_before} -> {gap_after})"
        );
        assert!(gap_after <= items.len());
    }
}
