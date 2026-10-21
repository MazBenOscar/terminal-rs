//! Stable study legend targets shared by price overlays, panes and hidden studies.
use crate::{
    theme::{self, mono},
    worker::BuiltIndicator,
};
use egui::{Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use tr_core::indicators::{registry::Spec, study};

pub struct Row {
    pub id: u32,
    pub rect: Rect,
    pub text: String,
    pub visible: bool,
    pub color: Color32,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Visibility(u32, bool),
    Edit(u32),
    Remove(u32),
}

pub fn rows(
    ui: &Ui,
    specs: &[Spec],
    built: &[BuiltIndicator],
    price: Rect,
    panes: &[(Rect, usize)],
    bar: usize,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut price_row = 0;
    let mut add = |spec: &Spec, rect: Rect, row: usize, price: bool| {
        let Some(settings) = &spec.chart else {
            return;
        };
        let b = built.iter().find(|b| b.spec.chart.as_ref().is_some_and(|c| c.id == settings.id));
        let values = if settings.visible {
            b.map(|b| {
                // A single output repeats the study name ("SMA 20  SMA 1677.5"), so show only its value.
                let single = b.overlays.len() == 1;
                b.overlays
                    .iter()
                    .filter_map(|(name, o)| {
                        o.line.pyramid.data.get(bar).map(|p| p[1] as f64).filter(|v| v.is_finite()).map(|v| {
                            let value = tr_chart::scale::format_value(v);
                            if single { value } else { format!("{name} {value}") }
                        })
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .unwrap_or_default()
        } else {
            "(hidden)".into()
        };
        let label = format!("{}  {values}", spec.label());
        let color = if !settings.visible {
            theme::muted()
        } else {
            b.and_then(|b| b.overlays.first()).map_or(theme::amber(), |(_, o)| {
                Color32::from_rgb((o.color[0] * 255.) as u8, (o.color[1] * 255.) as u8, (o.color[2] * 255.) as u8)
            })
        };
        let font = mono(10.5);
        let width =
            (ui.painter().layout_no_wrap(label.clone(), font, color).size().x + 8.).min((rect.width() - 12.).max(0.));
        let at = pos2(rect.left() + 6., rect.top() + if price { 20. } else { 3. } + row as f32 * 14.);
        let hit = Rect::from_min_size(at, vec2(width, 14.)).intersect(rect);
        if hit.is_positive() {
            rows.push(Row { id: settings.id, rect: hit, text: label, visible: settings.visible, color });
        }
    };
    for (i, spec) in specs.iter().enumerate() {
        if spec.chart.as_ref().is_some_and(|c| !c.visible) || study::pane_key(specs, i).is_ok_and(|key| key.is_none()) {
            add(spec, price, price_row, true);
            price_row += 1;
        }
    }
    for (at, (rect, index)) in panes.iter().enumerate() {
        let Some(id) = built[*index].spec.chart.as_ref().map(|c| c.id) else {
            continue;
        };
        let Some(spec) = specs.iter().find(|s| s.chart.as_ref().is_some_and(|c| c.id == id && c.visible)) else {
            continue;
        };
        let row = panes[..at].iter().filter(|(r, _)| r == rect).count();
        add(spec, *rect, row, false);
    }
    rows
}

pub fn interact(ui: &Ui, pane: u64, rows: &[Row]) -> Option<Action> {
    let mut action = None;
    for row in rows {
        let response = ui
            .interact(row.rect, ui.id().with((pane, "study-legend", row.id)), Sense::click())
            .on_hover_text(format!("{}\nClick or right-click to show, hide, edit or remove this study.", row.text));
        if response.secondary_clicked() {
            egui::Popup::toggle_id(ui.ctx(), egui::Popup::default_response_id(&response));
        }
        egui::Popup::menu(&response).show(|ui| {
            ui.label(egui::RichText::new(&row.text).font(mono(11.)).color(theme::muted()));
            let mut visible = row.visible;
            if ui.checkbox(&mut visible, "Show on chart").changed() {
                action = Some(Action::Visibility(row.id, visible));
                ui.close();
            }
            if ui.button("Edit indicator…").clicked() {
                action = Some(Action::Edit(row.id));
                ui.close();
            }
            if ui
                .button("Remove indicator")
                .on_hover_text("Also removes indicators calculated from this study")
                .clicked()
            {
                action = Some(Action::Remove(row.id));
                ui.close();
            }
        });
    }
    action
}

pub fn paint(ui: &Ui, rows: &[Row]) {
    for row in rows {
        let painter = ui.painter_at(row.rect.expand2(vec2(2., 1.)));
        let galley = painter.layout_no_wrap(row.text.clone(), mono(10.5), row.color);
        crate::chart_view::label_plate(&painter, Rect::from_min_size(row.rect.min, galley.size()));
        if ui.rect_contains_pointer(row.rect) {
            painter.rect_filled(row.rect, 2., theme::control_hover());
            painter.hline(row.rect.x_range(), row.rect.bottom() - 1., Stroke::new(0.5, row.color));
        }
        painter.galley(row.rect.min, galley, row.color);
    }
}
