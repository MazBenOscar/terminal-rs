//! Shared geometry and controls for the chart drawing toolbar.
use crate::{
    theme,
    widgets::{self, Icon},
};
use egui::{Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use tr_chart::drawings::Tool;

pub struct Section {
    pub title: &'static str,
    pub tools: &'static [Tool],
}

pub struct Category {
    pub title: &'static str,
    pub sections: &'static [Section],
}

impl Category {
    pub fn visible_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|section| !section.tools.is_empty())
    }

    pub fn tools(&self) -> impl Iterator<Item = Tool> + '_ {
        self.visible_sections().flat_map(|section| section.tools.iter().copied())
    }
}

pub const CATEGORIES: [Category; 5] = [
    Category {
        title: "Trend lines",
        sections: &[
            Section {
                title: "Lines",
                tools: &[
                    Tool::Trendline,
                    Tool::Ray,
                    Tool::Horizontal,
                    Tool::Vertical,
                    Tool::HorizontalRay,
                    Tool::ExtendedLine,
                    Tool::InfoLine,
                ],
            },
            Section { title: "Channels", tools: &[Tool::Channel, Tool::RegressionTrend] },
            Section { title: "Pitchforks", tools: &[Tool::Pitchfork] },
        ],
    },
    Category {
        title: "Fibonacci",
        sections: &[
            Section {
                title: "Fibonacci",
                tools: &[Tool::Fib, Tool::FibExtension, Tool::FibChannel, Tool::FibTimeZone],
            },
            Section { title: "Volume-based", tools: &[] },
        ],
    },
    Category {
        title: "Measure & volume",
        sections: &[
            Section {
                title: "Measurers",
                tools: &[Tool::Measure, Tool::PriceRange, Tool::DateRange, Tool::DatePriceRange],
            },
            Section { title: "Volume-based", tools: &[Tool::AnchoredVwap, Tool::FixedRangeVolumeProfile] },
            Section { title: "Forecasting", tools: &[Tool::LongPosition, Tool::ShortPosition] },
        ],
    },
    Category {
        title: "Annotation",
        sections: &[
            Section { title: "Arrows", tools: &[] },
            Section { title: "Brushes", tools: &[] },
            Section { title: "Shapes", tools: &[Tool::Rectangle] },
        ],
    },
    Category { title: "Text & notes", sections: &[Section { title: "Text and notes", tools: &[Tool::Text] }] },
];

pub const PATTERNS: Category = Category {
    title: "Patterns",
    sections: &[Section { title: "Chart patterns", tools: &[] }, Section { title: "Elliott waves", tools: &[] }],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailItem {
    Crosshair,
    Category(usize),
    Patterns,
    Separator,
    Magnet,
    Lock,
    Hide,
    RemoveAll,
}

const RAIL: [RailItem; 12] = [
    RailItem::Crosshair,
    RailItem::Category(0),
    RailItem::Category(1),
    RailItem::Patterns,
    RailItem::Category(2),
    RailItem::Category(3),
    RailItem::Category(4),
    RailItem::Separator,
    RailItem::Magnet,
    RailItem::Lock,
    RailItem::Hide,
    RailItem::RemoveAll,
];

/// Pure geometry shared by painting, interaction and tests.
pub fn rail_layout(rail: Rect) -> Vec<(RailItem, Rect)> {
    let size = theme::dp(30.);
    let mut y = rail.top() + theme::dp(4.);
    RAIL.into_iter()
        .filter_map(|item| {
            let visible = match item {
                RailItem::Category(index) => CATEGORIES[index].tools().next().is_some(),
                RailItem::Patterns => PATTERNS.tools().next().is_some(),
                _ => true,
            };
            if !visible {
                return None;
            }
            let height = if item == RailItem::Separator { theme::dp(4.) } else { size };
            let rect = Rect::from_min_size(pos2(rail.center().x - size / 2., y), vec2(size, height));
            y += height + theme::dp(4.);
            Some((item, rect))
        })
        .collect()
}

// Compatibility view for the pre-category interaction tests; derived from the table.
#[cfg(test)]
const fn category_tools<const N: usize>(index: usize) -> [Tool; N] {
    let mut tools = [Tool::Trendline; N];
    let mut section = 0;
    let mut out = 0;
    while section < CATEGORIES[index].sections.len() {
        let mut row = 0;
        while row < CATEGORIES[index].sections[section].tools.len() {
            tools[out] = CATEGORIES[index].sections[section].tools[row];
            out += 1;
            row += 1;
        }
        section += 1;
    }
    tools
}

#[cfg(test)]
pub const GROUPS: [(&str, &[Tool]); 5] = [
    (CATEGORIES[0].title, &category_tools::<10>(0)),
    (CATEGORIES[1].title, &category_tools::<4>(1)),
    (CATEGORIES[2].title, &category_tools::<8>(2)),
    (CATEGORIES[3].title, &category_tools::<1>(3)),
    (CATEGORIES[4].title, &category_tools::<1>(4)),
];

#[derive(Clone, Debug)]
pub struct State {
    pub last: [Tool; 5],
    pub open: Option<usize>,
    pub hide_open: bool,
    pub held: Option<(usize, f64)>,
    pub flyout: Rect,
    pub favorites_dragging: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            last: [Tool::Trendline, Tool::Fib, Tool::LongPosition, Tool::Rectangle, Tool::Text],
            open: None,
            hide_open: false,
            held: None,
            flyout: Rect::NOTHING,
            favorites_dragging: false,
        }
    }
}

impl State {
    /// Accept legacy four-slot layouts and reject tools belonging to other categories.
    pub fn restore_last(&mut self, saved: &[Tool]) {
        for &tool in saved {
            self.remember(tool);
        }
    }

    pub fn remember(&mut self, tool: Tool) {
        if let Some(index) = CATEGORIES.iter().position(|category| category.tools().any(|item| item == tool)) {
            self.last[index] = tool;
        }
    }
}

#[cfg(test)]
#[test]
fn rail_layout_orders_controls_and_omits_empty_patterns() {
    let rail = Rect::from_min_size(pos2(10., 10.), vec2(theme::dp(36.), theme::dp(600.)));
    let layout = rail_layout(rail);
    assert_eq!(
        layout.iter().map(|(item, _)| *item).collect::<Vec<_>>(),
        [
            RailItem::Crosshair,
            RailItem::Category(0),
            RailItem::Category(1),
            RailItem::Category(2),
            RailItem::Category(3),
            RailItem::Category(4),
            RailItem::Separator,
            RailItem::Magnet,
            RailItem::Lock,
            RailItem::Hide,
            RailItem::RemoveAll,
        ]
    );
    assert!(layout.windows(2).all(|pair| pair[0].1.bottom() < pair[1].1.top()));
    assert!(layout.iter().all(|(_, rect)| rail.contains_rect(*rect)));
    assert_eq!(PATTERNS.sections.iter().map(|s| s.title).collect::<Vec<_>>(), ["Chart patterns", "Elliott waves"]);
    assert!(PATTERNS.visible_sections().next().is_none());
}

#[cfg(test)]
#[test]
fn category_table_covers_every_tool_exactly_once() {
    let tools: Vec<_> = CATEGORIES.iter().flat_map(Category::tools).collect();
    assert_eq!(tools.len(), Tool::ALL.len());
    for tool in Tool::ALL {
        assert_eq!(tools.iter().filter(|&&item| item == tool).count(), 1, "{tool:?}");
    }
}

#[cfg(test)]
#[test]
fn empty_sections_are_omitted_in_table_order() {
    let category = &CATEGORIES[1];
    assert_eq!(category.sections.len(), 2);
    assert_eq!(category.visible_sections().map(|section| section.title).collect::<Vec<_>>(), ["Fibonacci"]);
    assert_eq!(
        CATEGORIES[0].visible_sections().map(|section| section.title).collect::<Vec<_>>(),
        ["Lines", "Channels", "Pitchforks"]
    );
}

#[cfg(test)]
#[test]
fn last_used_is_independent_per_category_and_legacy_lists_are_sanitized() {
    let mut state = State::default();
    state.restore_last(&[Tool::Ray, Tool::FibExtension, Tool::Text, Tool::ShortPosition]);
    assert_eq!(state.last, [Tool::Ray, Tool::FibExtension, Tool::ShortPosition, Tool::Rectangle, Tool::Text]);
    state.remember(Tool::Measure);
    assert_eq!(state.last, [Tool::Ray, Tool::FibExtension, Tool::Measure, Tool::Rectangle, Tool::Text]);
    state.restore_last(&[]);
    state.restore_last(&[Tool::Text; 8]);
    assert_eq!(state.last[0], Tool::Ray);
    for (category, tool) in CATEGORIES.iter().zip(state.last) {
        assert!(category.tools().any(|item| item == tool));
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
    fn any_hidden(&self) -> bool {
        self.drawings_hidden || self.indicators_hidden || self.positions_orders_hidden
    }

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
        let mut anchors = [rail.min; CATEGORIES.len()];
        let mut hide_anchor = rail.min;
        for (item, rect) in rail_layout(rail) {
            if item == RailItem::Separator {
                separator(ui, rect.shrink2(vec2(theme::dp(3.), 0.)));
                continue;
            }
            let slot = match item {
                RailItem::Crosshair => 0,
                RailItem::Category(index) => index + 1,
                RailItem::Magnet => 6,
                RailItem::Hide => 7,
                RailItem::RemoveAll => 8,
                RailItem::Lock => 9,
                _ => continue,
            };
            let tool = match item {
                RailItem::Category(index) => Some(self.drawing_toolbar.last[index]),
                _ => None,
            };
            let (icon, active, hint) = if let Some(tool) = tool {
                (
                    widgets::tool_icon(tool),
                    self.draw.tool == Some(tool),
                    format!("{} · {}", CATEGORIES[slot - 1].title, Self::tool_hint(tool)),
                )
            } else {
                match slot {
                    0 => (Icon::Crosshair, self.draw.tool.is_none(), "Crosshair".into()),
                    6 => (Icon::Magnet, self.magnet, "Magnet".into()),
                    7 => (if self.any_hidden() { Icon::EyeOff } else { Icon::Eye }, self.any_hidden(), "Hide".into()),
                    8 => (Icon::Trash, false, "Remove all drawings (undo with Cmd+Z)".into()),
                    9 => (
                        if self.drawings_locked { Icon::Lock } else { Icon::Unlock },
                        self.drawings_locked,
                        if self.drawings_locked { "Unlock drawings" } else { "Lock drawings" }.into(),
                    ),
                    _ => break,
                }
            };
            let response = ui
                .add_enabled_ui(slot != 8 || !self.drawings.is_empty(), |ui| {
                    button(ui, rect, (self.id, "rail", slot), icon, active, &hint)
                })
                .inner;
            if (1..=5).contains(&slot) {
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
            } else {
                if item == RailItem::Hide {
                    hide_anchor = pos2(rail.right() + theme::dp(4.), rect.top());
                    widgets::draw_icon(
                        ui.painter(),
                        Rect::from_min_max(rect.max - vec2(11., 11.), rect.max),
                        Icon::ChevronSmall,
                        theme::muted(),
                    );
                }
                if response.clicked() {
                    match slot {
                        0 => {
                            self.cancel();
                        }
                        6 => self.magnet = !self.magnet,
                        7 => {
                            self.drawing_toolbar.hide_open = !self.drawing_toolbar.hide_open;
                            self.drawing_toolbar.open = None;
                        }
                        9 => {
                            self.drawings_locked = !self.drawings_locked;
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
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.drawing_toolbar.held = None;
        }
        self.drawing_toolbar.flyout = Rect::NOTHING;
        if self.drawing_toolbar.hide_open {
            let output = egui::Area::new(egui::Id::new((self.id, "drawing-hide-menu")))
                .order(egui::Order::Foreground)
                .fixed_pos(hide_anchor)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        let changed = ui.checkbox(&mut self.drawings_hidden, "Hide drawings").changed();
                        ui.checkbox(&mut self.indicators_hidden, "Hide indicators");
                        ui.checkbox(&mut self.positions_orders_hidden, "Hide positions and orders");
                        if changed {
                            self.cancel();
                        }
                        if ui.button(if self.any_hidden() { "Show all" } else { "Hide all" }).clicked() {
                            let hidden = !self.any_hidden();
                            self.drawings_hidden = hidden;
                            self.indicators_hidden = hidden;
                            self.positions_orders_hidden = hidden;
                            self.cancel();
                        }
                    });
                });
            self.drawing_toolbar.flyout = output.response.rect;
            if ui.input(|i| {
                i.key_pressed(egui::Key::Escape)
                    || (i.pointer.any_pressed()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|p| !rail.contains(p) && !output.response.rect.contains(p)))
            }) {
                self.drawing_toolbar.hide_open = false;
            }
        }
        if let Some(group) = self.drawing_toolbar.open {
            self.drawing_toolbar.hide_open = false;
            let anchor = anchors[group];
            let output = egui::Area::new(egui::Id::new((self.id, "drawing-flyout")))
                .order(egui::Order::Foreground)
                .default_size(vec2(theme::dp(320.), theme::dp(440.)))
                .fixed_pos(anchor)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        let height =
                            theme::dp(420.).min(ui.ctx().content_rect().height() - theme::dp(24.)).max(theme::dp(24.));
                        ui.set_max_height(height);
                        egui::ScrollArea::vertical()
                            .id_salt((self.id, "drawing-sections", group))
                            .max_height(height)
                            .show(ui, |ui| self.drawing_category_menu(ui, group));
                    });
                });
            self.drawing_toolbar.flyout = output.response.rect;
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
