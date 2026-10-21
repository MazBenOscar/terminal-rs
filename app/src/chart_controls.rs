use egui::Ui;
use tr_core::indicators::study::{BandFill, PlotStyle, Settings};

pub fn output_styles(ui: &mut Ui, settings: &mut Settings, names: &[String]) {
    ui.collapsing("Output styles & band fill", |ui| {
        for name in names {
            ui.push_id(name, |ui| {
                let style = settings.outputs.entry(name.clone()).or_default();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut style.visible, name);
                    let mut custom = style.color.is_some();
                    if ui.checkbox(&mut custom, "Color").changed() {
                        style.color = custom.then_some([100, 180, 240]);
                    }
                    if let Some(color) = &mut style.color {
                        ui.color_edit_button_srgb(color);
                    }
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("plot")
                        .selected_text(style.plot.map_or("Original", plot_label))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut style.plot, None, "Original");
                            for plot in [PlotStyle::Line, PlotStyle::Dots, PlotStyle::Steps, PlotStyle::Histogram] {
                                ui.selectable_value(&mut style.plot, Some(plot), plot_label(plot));
                            }
                        });
                    ui.label("Width");
                    ui.add(egui::DragValue::new(&mut style.width).range(0.5..=8.).speed(0.1))
                        .on_hover_text("Line and marker thickness. Histogram columns follow bar spacing.");
                });
            });
        }
        if names.len() >= 2 {
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
                for (label, value) in [("From", &mut fill.upper), ("To", &mut fill.lower)] {
                    egui::ComboBox::from_id_salt(label).selected_text(format!("{label}: {value}")).show_ui(ui, |ui| {
                        for name in names {
                            ui.selectable_value(value, name.clone(), name);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.color_edit_button_srgb(&mut fill.color);
                    ui.label("Opacity");
                    ui.add(egui::Slider::new(&mut fill.opacity, 0..=150));
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
