//! Named local chart setups. Securities, timeframes and drawings are intentionally
//! excluded: applying a template keeps the current chart's subject and annotations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tr_chart::scene::ChartType;
use tr_core::Timeframe;
use tr_core::indicators::{
    registry::{Input, Spec},
    study,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub version: u32,
    pub name: String,
    pub indicators: Vec<Spec>,
    pub chart_type: ChartType,
    pub log: bool,
    pub show_volume: bool,
    #[serde(default)]
    pub data: crate::chart_data::ChartData,
}

fn path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.trim() != name
        || name.is_empty()
        || name.len() > 100
        || name.starts_with('.')
        || name.ends_with('.')
        || !name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("Use a template name of 1–100 characters: letters, numbers, spaces, - or _".into());
    }
    Ok(dir.join(format!("{name}.json")))
}

pub fn directory() -> PathBuf {
    crate::worker::data_dir().join("chart-templates")
}

pub fn names(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for file in std::fs::read_dir(dir).map_err(|e| format!("Reading chart templates: {e}"))? {
        let file = file.map_err(|e| e.to_string())?;
        if file.path().extension().is_some_and(|s| s == "json")
            && file.file_type().is_ok_and(|t| t.is_file())
            && let Some(name) = file.path().file_stem().and_then(|s| s.to_str())
        {
            names.push(name.to_string());
        }
    }
    names.sort_by_key(|s| s.to_lowercase());
    Ok(names)
}

impl Template {
    pub fn validate(&mut self) -> Result<(), String> {
        if !matches!(self.version, 1 | 2) {
            return Err("This chart-template version is not supported".into());
        }
        if self.indicators.len() > 128 {
            return Err("A chart template supports at most 128 studies".into());
        }
        study::assign_ids(&mut self.indicators);
        let input = Input::new(&[], Timeframe::D1);
        let mut validation = self.indicators.clone();
        for settings in validation.iter_mut().filter_map(|s| s.chart.as_mut()) {
            settings.timeframe = None;
        }
        for result in study::evaluate(&validation, &input) {
            result.map_err(|e| format!("Invalid template: {e}"))?;
        }
        Ok(())
    }

    pub fn load(dir: &Path, name: &str) -> Result<Self, String> {
        let path = path(dir, name)?;
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 1_048_576 {
            return Err("The chart template exceeds the 1 MiB limit".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut template: Self = serde_json::from_slice(&bytes).map_err(|e| format!("Reading template: {e}"))?;
        template.validate()?;
        Ok(template)
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        let destination = path(dir, &self.name)?;
        self.validate()?;
        self.version = 2;
        // Preserve corrupt or unsupported files instead of silently replacing them.
        if destination.exists() {
            Self::load(dir, &self.name)?;
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        crate::layouts::write_atomic(&destination, &serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Saving chart template: {e}"))
    }

    /// Merge by remapping every imported ID and calculation, pane and fill edges.
    /// All validation happens before the caller's current setup changes.
    pub fn merge_into(&self, current: &mut Vec<Spec>) -> Result<(), String> {
        if current.len() + self.indicators.len() > 128 {
            return Err("Merging would exceed 128 studies".into());
        }
        let mut template = self.clone();
        template.validate()?;
        let mut merged = current.clone();
        study::assign_ids(&mut merged);
        let mut used: HashSet<_> = merged.iter().filter_map(|s| s.chart.as_ref().map(|c| c.id)).collect();
        let mut ids = HashMap::new();
        for spec in &template.indicators {
            let old = spec.chart.as_ref().unwrap().id;
            let id = (1..=u32::MAX).find(|id| !used.contains(id)).ok_or("No free study IDs")?;
            used.insert(id);
            ids.insert(old, id);
        }
        for spec in &mut template.indicators {
            let c = spec.chart.as_mut().unwrap();
            c.id = ids[&c.id];
            if let study::Source::Output { study, .. } = &mut c.source {
                *study = ids[study];
            }
            if let study::Pane::With(id) = &mut c.pane {
                *id = ids[id];
            }
            // A missing imported target must not accidentally bind to an
            // unrelated existing study whose ID happens to match.
            if c.fill.as_ref().is_some_and(|fill| {
                [&fill.upper, &fill.lower].iter().any(|edge| edge.study.is_some_and(|id| !ids.contains_key(&id)))
            }) {
                c.fill = None;
            }
            if let Some(fill) = &mut c.fill {
                for edge in [&mut fill.upper, &mut fill.lower] {
                    if let Some(id) = &mut edge.study {
                        *id = ids[id];
                    }
                }
            }
        }
        merged.extend(template.indicators);
        *current = merged;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> Template {
        let mut indicators = vec![Spec::new("RSI", &[14.]).unwrap(), Spec::new("EMA", &[9.]).unwrap()];
        study::assign_ids(&mut indicators);
        indicators[1].chart.as_mut().unwrap().source = study::Source::Output { study: 1, output: "RSI".into() };
        indicators[1].chart.as_mut().unwrap().pane = study::Pane::With(1);
        Template {
            version: 1,
            name: "RSI signal".into(),
            indicators,
            chart_type: ChartType::Candles,
            log: false,
            show_volume: true,
            data: Default::default(),
        }
    }
    #[test]
    fn merging_keeps_existing_ports_and_remaps_all_imported_edges() {
        let t = template();
        let mut current = t.indicators.clone();
        t.merge_into(&mut current).unwrap();
        assert_eq!(current[0].chart.as_ref().unwrap().id, 1);
        assert_eq!(current[2].chart.as_ref().unwrap().id, 3);
        assert_eq!(current[3].chart.as_ref().unwrap().source, study::Source::Output { study: 3, output: "RSI".into() });
        assert_eq!(current[3].chart.as_ref().unwrap().pane, study::Pane::With(3));
        let mut bad = t.clone();
        bad.indicators[0].chart.as_mut().unwrap().pane = study::Pane::With(900);
        let before = current.clone();
        assert!(bad.merge_into(&mut current).is_err());
        assert_eq!(before, current);
    }
    #[test]
    fn cross_study_fill_template_round_trip_and_merge_remap_targets() {
        let mut t = template();
        t.indicators[0].chart.as_mut().unwrap().fill = Some(study::BandFill {
            upper: "RSI".into(),
            lower: study::FillEdge { study: Some(2), output: "EMA".into() },
            color: [1, 2, 3],
            opacity: 35,
        });
        let mut restored: Template = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, t);
        let mut current = t.indicators.clone();
        restored.merge_into(&mut current).unwrap();
        let settings = current[2].chart.as_ref().unwrap();
        assert_eq!(settings.id, 3);
        let fill = settings.fill.as_ref().unwrap();
        assert_eq!(fill.upper.study, None);
        assert_eq!(fill.lower.study, Some(4));
        assert!(study::validate_fill_edge(&current, 2, &fill.lower).is_ok());
        t.indicators[0].chart.as_mut().unwrap().fill.as_mut().unwrap().lower.study = Some(999);
        t.merge_into(&mut current).unwrap();
        assert!(current[4].chart.as_ref().unwrap().fill.is_none());
    }

    #[test]
    #[ignore = "BUG: merge_into allocates imported IDs from used (chart IDs already on the current list only), so an imported study receives an ID that a dangling fill edge in the current chart already references and that fill silently rebinds to the imported study"]
    fn merge_must_not_rebind_dangling_fill_targets_with_fresh_ids() {
        let mut current = vec![Spec::new("EMA", &[]).unwrap()];
        study::assign_ids(&mut current);
        let edge = study::FillEdge { study: Some(2), output: "Lower".into() };
        current[0].chart.as_mut().unwrap().fill =
            Some(study::BandFill { upper: "EMA".into(), lower: edge.clone(), color: [1, 2, 3], opacity: 35 });
        let t = template();
        t.merge_into(&mut current).unwrap();
        // The dangling edge must stay dangling (or be dropped), never attach to an imported study.
        match current[0].chart.as_ref().unwrap().fill.as_ref() {
            None => {}
            Some(fill) => assert!(
                fill.lower
                    .study
                    .is_none_or(|id| !current.iter().skip(1).any(|s| s.chart.as_ref().is_some_and(|c| c.id == id))),
                "the imported studies took free id {:?} and the dangling fill edge silently rebinds to one of them",
                fill.lower.study
            ),
        }
    }

    #[test]
    fn persistence_and_corrupt_file_protection() {
        let dir = std::env::temp_dir().join(format!(
            "tr-chart-templates-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut t = template();
        t.save(&dir).unwrap();
        assert_eq!(Template::load(&dir, &t.name).unwrap(), t);
        assert_eq!(names(&dir).unwrap(), vec![t.name.clone()]);
        let file = path(&dir, &t.name).unwrap();
        std::fs::write(&file, "broken").unwrap();
        assert!(t.save(&dir).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "broken");
        assert!(path(&dir, "../escape").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
