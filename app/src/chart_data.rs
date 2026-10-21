use serde::{Deserialize, Serialize};
use tr_core::session::{Session, Zone};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartData {
    pub zone: Zone,
    pub session: Session,
    pub refresh_seconds: u32,
    pub account: crate::chart_account::Settings,
    pub comparisons: Vec<String>,
    pub hidden_comparisons: Vec<String>,
    pub workflow: Box<crate::chart_workflow::Workflow>,
    pub context: crate::chart_context::Settings,
    pub analytics: Box<crate::chart_studies::Settings>,
}
impl ChartData {
    pub fn edit(&mut self, ui: &mut egui::Ui, us_equity: bool) {
        egui::ComboBox::from_id_salt("display-zone")
            .selected_text(format!("Time zone: {}", self.zone.label()))
            .show_ui(ui, |ui| {
                for zone in Zone::ALL {
                    ui.selectable_value(&mut self.zone, zone, zone.label());
                }
            });
        ui.add_enabled_ui(us_equity, |ui| {
            egui::ComboBox::from_id_salt("session").selected_text(self.session.label()).show_ui(ui, |ui| {
                for session in Session::ALL {
                    ui.selectable_value(&mut self.session, session, session.label());
                }
            });
        });
        ui.label("Sessions filter available intraday bars; they do not request extra data.");
        ui.label("US calendar: recurring holidays and selected exceptional closures.");
        let mut refresh = self.refresh_seconds > 0;
        if ui.checkbox(&mut refresh, "Refresh chart history automatically").changed() {
            self.refresh_seconds = if refresh { 60 } else { 0 };
        }
        if refresh {
            ui.horizontal(|ui| {
                ui.label("Seconds");
                ui.add(egui::DragValue::new(&mut self.refresh_seconds).range(15..=600));
            });
        }
        ui.label("History refresh does not establish a real-time subscription.");
    }
}
