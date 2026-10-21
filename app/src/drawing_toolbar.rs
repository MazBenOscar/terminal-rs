//! Shared geometry and controls for the chart drawing toolbar.
use crate::{
    theme,
    widgets::{self, Icon},
};
use egui::{Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use tr_chart::drawings::Tool;

pub const GROUPS: [(&str, &[Tool]); 4] = [
    ("LINES", &[Tool::Trendline, Tool::Ray, Tool::Horizontal, Tool::Vertical]),
    ("FIB & CHANNELS", &[Tool::Fib, Tool::FibExtension, Tool::Channel, Tool::Pitchfork]),
    ("SHAPES & NOTES", &[Tool::Rectangle, Tool::Text]),
    ("POSITIONS", &[Tool::LongPosition, Tool::ShortPosition]),
];

#[derive(Clone, Debug)]
pub struct State {
    pub last: [Tool; 4],
    pub open: Option<usize>,
    pub held: Option<(usize, f64)>,
    pub flyout: Rect,
    pub favorites_dragging: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            last: [Tool::Trendline, Tool::Fib, Tool::Rectangle, Tool::LongPosition],
            open: None,
            held: None,
            flyout: Rect::NOTHING,
            favorites_dragging: false,
        }
    }
}

pub fn rail_visible(width: f32, shown: bool, available: bool) -> bool {
    shown && available && width >= theme::dp(420.)
}

pub fn button(
    ui: &mut Ui,
    rect: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    icon: Icon,
    active: bool,
    hint: &str,
) -> Response {
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    if active || response.hovered() {
        ui.painter().rect_filled(
            rect,
            theme::CONTROL_RADIUS,
            if active { theme::accent_wash() } else { theme::control_hover() },
        );
    }
    widgets::draw_icon(
        ui.painter(),
        rect.shrink(theme::dp(7.)),
        icon,
        if active { theme::accent() } else { theme::text() },
    );
    response.on_hover_text(hint)
}

pub fn favorites_rect(plot: Rect, count: usize, position: Option<[f32; 2]>, top: f32) -> Rect {
    let size = vec2(theme::dp(20. + 28. * count as f32).min(plot.width()), theme::dp(32.).min(plot.height()));
    let min = position
        .map(|p| pos2(plot.left() + plot.width() * p[0], plot.top() + plot.height() * p[1]))
        .unwrap_or_else(|| pos2(plot.center().x - size.x / 2., top));
    Rect::from_min_size(
        pos2(
            min.x.clamp(plot.left(), (plot.right() - size.x).max(plot.left())),
            min.y.clamp(plot.top(), (plot.bottom() - size.y).max(plot.top())),
        ),
        size,
    )
}

pub fn fraction(plot: Rect, rect: Rect) -> [f32; 2] {
    [(rect.left() - plot.left()) / plot.width(), (rect.top() - plot.top()) / plot.height()]
}

pub fn separator(ui: &Ui, rect: Rect) {
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1., theme::border()));
}

impl super::ChartState {
    pub(super) fn drawing_available(&self) -> bool {
        self.replay_cutoff.is_none() && self.chart_type != tr_chart::scene::ChartType::Renko
    }

    fn tool_hint(tool: Tool) -> String {
        let shortcut = Self::tool_shortcut(tool);
        if shortcut.is_empty() { tool.label().into() } else { format!("{} · {shortcut}", tool.label()) }
    }

    pub(super) fn drawing_rail(&mut self, ui: &mut Ui, rail: Rect) {
        ui.painter().rect_filled(rail, 0., theme::header());
        ui.painter().vline(rail.right(), rail.y_range(), Stroke::new(1., theme::border()));
        let size = theme::dp(30.);
        let mut y = rail.top() + theme::dp(4.);
        let mut anchors = [rail.min; 4];
        for slot in 0..9 {
            if (1..=6).contains(&slot) {
                separator(
                    ui,
                    Rect::from_min_size(pos2(rail.left() + theme::dp(6.), y), vec2(theme::dp(24.), theme::dp(6.))),
                );
                y += theme::dp(8.);
            }
            let rect = Rect::from_min_size(pos2(rail.center().x - size / 2., y), vec2(size, size));
            y += size + theme::dp(4.);
            let tool = match slot {
                1..=4 => Some(self.drawing_toolbar.last[slot - 1]),
                5 => Some(Tool::Measure),
                _ => None,
            };
            let (icon, active, hint) = if let Some(tool) = tool {
                (widgets::tool_icon(tool), self.draw.tool == Some(tool), Self::tool_hint(tool))
            } else {
                match slot {
                    0 => (Icon::Crosshair, self.draw.tool.is_none(), "Crosshair".into()),
                    6 => (Icon::Magnet, self.magnet, "Magnet".into()),
                    7 => (
                        if self.drawings_hidden { Icon::EyeOff } else { Icon::Eye },
                        self.drawings_hidden,
                        "Hide drawings".into(),
                    ),
                    8 => (Icon::Trash, false, "Remove all drawings (undo with ⌘Z)".into()),
                    _ => break,
                }
            };
            let response = ui
                .add_enabled_ui(slot != 8 || !self.drawings.is_empty(), |ui| {
                    button(ui, rect, (self.id, "rail", slot), icon, active, &hint)
                })
                .inner;
            if (1..=4).contains(&slot) {
                let group = slot - 1;
                anchors[group] = pos2(rail.right() + theme::dp(4.), rect.top());
                let corner = Rect::from_min_max(rect.max - vec2(theme::dp(11.), theme::dp(11.)), rect.max);
                widgets::draw_icon(ui.painter(), corner, Icon::ChevronSmall, theme::muted());
                let now = ui.input(|i| i.time);
                if response.is_pointer_button_down_on() {
                    let held = self.drawing_toolbar.held.get_or_insert((group, now));
                    if held.0 == group && now - held.1 >= 0.35 {
                        self.drawing_toolbar.open = Some(group);
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(350));
                }
                let chevron = response.clicked() && response.interact_pointer_pos().is_some_and(|p| corner.contains(p));
                let long_press = self
                    .drawing_toolbar
                    .held
                    .is_some_and(|(held_group, start)| held_group == group && now - start >= 0.35);
                if chevron || response.secondary_clicked() || (response.clicked() && long_press) {
                    self.drawing_toolbar.open = Some(group);
                }
                if response.clicked() && !chevron && !long_press {
                    self.toggle_quick_tool(tool.unwrap());
                    self.drawing_toolbar.open = None;
                }
            } else if response.clicked() {
                match slot {
                    0 => {
                        self.cancel();
                    }
                    5 => self.toggle_quick_tool(Tool::Measure),
                    6 => self.magnet = !self.magnet,
                    7 => {
                        self.drawings_hidden = !self.drawings_hidden;
                        self.cancel();
                    }
                    8 => {
                        self.remember_drawings();
                        self.drawings.clear();
                        self.cancel();
                    }
                    _ => {}
                }
            }
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.drawing_toolbar.held = None;
        }
        self.drawing_toolbar.flyout = Rect::NOTHING;
        if let Some(group) = self.drawing_toolbar.open {
            let anchor = anchors[group];
            let mut favorites = crate::prefs::drawing_favorites(ui.ctx());
            let output = egui::Area::new(egui::Id::new((self.id, "drawing-flyout")))
                .order(egui::Order::Foreground)
                .fixed_pos(anchor)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(egui::RichText::new(GROUPS[group].0).small().color(theme::muted()));
                        for &tool in GROUPS[group].1 {
                            ui.push_id(tool, |ui| {
                                ui.horizontal(|ui| {
                                    if widgets::icon_button(
                                        ui,
                                        widgets::tool_icon(tool),
                                        tool.label(),
                                        self.draw.tool == Some(tool),
                                    )
                                    .clicked()
                                    {
                                        self.start_tool(tool);
                                        self.drawing_toolbar.open = None;
                                    }
                                    if ui
                                        .add_sized(
                                            vec2(theme::dp(190.), theme::dp(24.)),
                                            egui::Button::new(tool.label()).shortcut_text(Self::tool_shortcut(tool)),
                                        )
                                        .clicked()
                                    {
                                        self.start_tool(tool);
                                        self.drawing_toolbar.open = None;
                                    }
                                    if widgets::icon_button(ui, Icon::Star, "Favorite", favorites.0.contains(&tool))
                                        .clicked()
                                    {
                                        crate::prefs::toggle_drawing_favorite(&mut favorites.0, tool);
                                    }
                                })
                            });
                        }
                    });
                });
            self.drawing_toolbar.flyout = output.response.rect;
            crate::prefs::set_drawing_favorites(ui.ctx(), favorites);
            if ui.input(|i| {
                i.key_pressed(egui::Key::Escape)
                    || (i.pointer.any_pressed()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|p| !rail.contains(p) && !output.response.rect.contains(p)))
            }) {
                self.drawing_toolbar.open = None;
            }
        }
    }

    pub(super) fn favorites_rect(&mut self, ui: &Ui, price: Rect, rows: &[crate::chart_legend::Row]) -> Rect {
        self.favorites_plot = price;
        let (favorites, shown) = crate::prefs::drawing_favorites(ui.ctx());
        if !shown || favorites.is_empty() || !self.drawing_available() {
            return Rect::NOTHING;
        }
        favorites_rect(
            price,
            favorites.len(),
            crate::prefs::drawing_toolbar(ui.ctx()).1,
            self.legend_bottom(price, rows) + theme::dp(4.),
        )
    }

    pub(super) fn favorites_bar(&mut self, ui: &mut Ui, rect: Rect) {
        if !rect.is_positive() {
            self.drawing_toolbar.favorites_dragging = false;
            return;
        }
        widgets::card(ui, rect, 0.);
        let grip = Rect::from_min_size(rect.min, vec2(theme::dp(20.), rect.height()));
        let response = ui.interact(grip, ui.id().with((self.id, "favorites-grip")), Sense::click_and_drag());
        self.drawing_toolbar.favorites_dragging = response.dragged() || response.is_pointer_button_down_on();
        widgets::draw_icon(ui.painter(), grip.shrink(4.), Icon::Grip, theme::muted());
        let mut prefs = crate::prefs::drawing_toolbar(ui.ctx());
        if response.double_clicked() {
            prefs.1 = None;
        } else if response.dragged() {
            let moved = rect.translate(ui.input(|i| i.pointer.delta()));
            let clamped = favorites_rect(
                self.favorites_plot,
                crate::prefs::drawing_favorites(ui.ctx()).0.len(),
                Some(fraction(self.favorites_plot, moved)),
                rect.top(),
            );
            prefs.1 = Some(fraction(self.favorites_plot, clamped));
        }
        crate::prefs::set_drawing_toolbar(ui.ctx(), prefs);
        let (favorites, _) = crate::prefs::drawing_favorites(ui.ctx());
        let content =
            Rect::from_min_max(pos2(grip.right(), rect.top() + theme::dp(2.)), rect.max - vec2(0., theme::dp(2.)));
        ui.scope_builder(egui::UiBuilder::new().max_rect(content).id_salt((self.id, "favorites-content")), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(content));
            egui::ScrollArea::horizontal()
                .id_salt((self.id, "favorites-scroll"))
                .max_height(theme::dp(28.))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.;
                    ui.horizontal(|ui| {
                        for tool in favorites {
                            let (r, _) = ui.allocate_exact_size(vec2(theme::dp(28.), theme::dp(28.)), Sense::hover());
                            if button(
                                ui,
                                r,
                                (self.id, "favorite", tool),
                                widgets::tool_icon(tool),
                                self.draw.tool == Some(tool),
                                &Self::tool_hint(tool),
                            )
                            .clicked()
                            {
                                self.toggle_quick_tool(tool);
                            }
                        }
                    });
                });
        });
    }
}
