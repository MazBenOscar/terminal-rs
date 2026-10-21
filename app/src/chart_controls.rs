use egui::{RichText, Ui};

use crate::theme;
use tr_core::indicators::study::{BandFill, PlotStyle, Settings};

pub fn output_styles(ui: &mut Ui, settings: &mut Settings, names: &[String]) {
    let base = settings.color.unwrap_or([100, 180, 240]);
    ui.collapsing("Output styles & band fill", |ui| {
        egui::Grid::new("outputs").num_columns(2).spacing([theme::dp(12.0), theme::dp(6.0)]).show(ui, |ui| {
            for name in names {
                let style = settings.outputs.entry(name.clone()).or_default();
                ui.checkbox(&mut style.visible, name).on_hover_text("Show this output");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("plot", name))
                        .selected_text(style.plot.map_or("Original", plot_label))
                        .width(theme::dp(90.0))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut style.plot, None, "Original");
                            for plot in [PlotStyle::Line, PlotStyle::Dots, PlotStyle::Steps, PlotStyle::Histogram] {
                                ui.selectable_value(&mut style.plot, Some(plot), plot_label(plot));
                            }
                        });
                    ui.add(egui::DragValue::new(&mut style.width).range(0.5..=8.).speed(0.1).suffix(" px"))
                        .on_hover_text("Line and marker thickness. Histogram columns follow bar spacing.");
                    // The automatic color depends on the output's role, so only a custom one gets a swatch.
                    if let Some(color) = &mut style.color {
                        ui.color_edit_button_srgb(color);
                        if ui.small_button("Reset").on_hover_text("Use the automatic color").clicked() {
                            style.color = None;
                        }
                    } else if ui.small_button("Set color").clicked() {
                        style.color = Some(base);
                    }
                });
                ui.end_row();
            }
        });
        if names.len() >= 2 {
            ui.add_space(theme::dp(4.0));
            let mut enabled = settings.fill.is_some();
            if ui.checkbox(&mut enabled, "Fill between outputs").changed() {
                settings.fill = enabled.then(|| BandFill {
                    upper: names.iter().find(|n| n.as_str() == "Upper").unwrap_or(&names[0]).clone(),
                    lower: names.iter().find(|n| n.as_str() == "Lower").unwrap_or(&names[names.len() - 1]).clone(),
                    color: settings.color.unwrap_or([100, 180, 240]),
                    opacity: 35,
                });
            }
            if let Some(fill) = &mut settings.fill {
                egui::Grid::new("fill").num_columns(2).spacing([theme::dp(12.0), theme::dp(6.0)]).show(ui, |ui| {
                    for (label, value) in [("From", &mut fill.upper), ("To", &mut fill.lower)] {
                        ui.label(RichText::new(label).color(theme::muted()));
                        egui::ComboBox::from_id_salt(label)
                            .selected_text(value.as_str())
                            .width(theme::dp(140.0))
                            .show_ui(ui, |ui| {
                                for name in names {
                                    ui.selectable_value(value, name.clone(), name);
                                }
                            });
                        ui.end_row();
                    }
                    ui.label(RichText::new("Fill").color(theme::muted()));
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut fill.color);
                        ui.add(egui::Slider::new(&mut fill.opacity, 0..=150).text("opacity"));
                    });
                    ui.end_row();
                });
            }
        }
    });
}
fn plot_label(plot: PlotStyle) -> &'static str {
    match plot {
        PlotStyle::Line => "Line",
        PlotStyle::Dots => "Dots",
        PlotStyle::Steps => "Steps",
        PlotStyle::Histogram => "Histogram",
    }
}
