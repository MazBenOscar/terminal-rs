//! Bounded, untrusted provider-name cache. Catalog names always take precedence.
use std::collections::BTreeMap;
use std::path::Path;

const LIMIT: usize = 5_000;
const TTL: i64 = 7 * 86_400;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Name(String),
    Missing { checked_at: i64 },
}

#[derive(Clone, Debug, Default)]
pub struct Names(pub BTreeMap<String, Entry>);

pub fn sanitize(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(80).collect()
}

pub fn catalog(key: &str) -> Option<&'static str> {
    tr_core::catalog::by_security(&tr_data::security(key)).map(|l| l.name).filter(|n| !n.trim().is_empty())
}

impl Names {
    pub fn name(&self, key: &str) -> Option<&str> {
        catalog(key).or_else(|| match self.0.get(key) {
            Some(Entry::Name(n)) => Some(n.as_str()),
            _ => None,
        })
    }

    pub fn needs(&self, key: &str, now: i64) -> bool {
        catalog(key).is_none()
            && match self.0.get(key) {
                Some(Entry::Name(_)) => false,
                Some(Entry::Missing { checked_at }) => now.saturating_sub(*checked_at) >= TTL,
                None => true,
            }
    }

    pub fn insert(&mut self, key: String, name: Option<String>, now: i64) {
        let entry = name
            .map(|n| sanitize(&n))
            .filter(|n| !n.is_empty())
            .map_or(Entry::Missing { checked_at: now }, Entry::Name);
        self.0.insert(key, entry);
        while self.0.len() > LIMIT {
            self.0.pop_first();
        }
    }

    pub fn load(path: &Path, now: i64) -> Self {
        use std::io::Read;
        let Ok(file) = std::fs::File::open(path) else { return Self::default() };
        let mut bytes = Vec::new();
        if file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).is_err() {
            return Self::default();
        }
        let parsed = if bytes.len() <= 8 * 1024 * 1024 {
            serde_json::from_slice::<BTreeMap<String, Entry>>(&bytes).ok()
        } else {
            None
        };
        let Some(entries) = parsed else {
            let _ = std::fs::rename(path, path.with_extension(format!("json.corrupt-{now}")));
            return Self::default();
        };
        let mut out = Self::default();
        for (key, entry) in entries {
            if key.len() > 256 || key.chars().any(char::is_control) || tr_core::Security::parse(&key).is_none() {
                continue;
            }
            match entry {
                Entry::Name(n) => out.insert(key, Some(n), now),
                Entry::Missing { checked_at } if checked_at <= now && now.saturating_sub(checked_at) < TTL => {
                    out.insert(key, None, checked_at);
                }
                _ => {}
            }
        }
        out
    }

    pub fn save(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec(&self.0) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

pub fn exact_name(key: &str, hits: &[tr_data::SearchHit]) -> Option<String> {
    hits.iter().find(|h| h.key == key).map(|h| sanitize(&h.name)).filter(|n| !n.is_empty())
}

pub fn order(a: Option<&str>, b: Option<&str>, descending: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let order = a.to_lowercase().cmp(&b.to_lowercase());
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

pub fn hover(ticker: &str, name: Option<&str>) -> String {
    name.map_or_else(|| ticker.to_string(), |n| format!("{ticker} · {n}"))
}

pub fn show_column(width: f32, custom: usize, enabled: bool) -> bool {
    enabled && width >= crate::theme::dp(380.0 + custom as f32 * 85.0 + 150.0)
}

pub fn request(
    ui: &egui::Ui,
    names: &Names,
    worker: &crate::worker::Worker,
    provider: &str,
    keys: impl Iterator<Item = String>,
) {
    if provider.eq_ignore_ascii_case("sample") || provider.eq_ignore_ascii_case("offline") {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let keys: Vec<_> = keys
        .filter(|key| names.needs(key, now))
        .filter(|key| {
            let id = egui::Id::new(("name-request", key));
            ui.data_mut(|d| {
                if d.get_temp::<i64>(id).is_some_and(|at| now.saturating_sub(at) < 60) {
                    return false;
                }
                d.insert_temp(id, now);
                true
            })
        })
        .collect();
    if !keys.is_empty() {
        worker.send(crate::worker::Request::SecurityNames(keys));
    }
}

fn name_job(name: &str, width: f32) -> egui::text::LayoutJob {
    let mut job =
        egui::text::LayoutJob::simple_singleline(name.to_string(), crate::theme::ui_font(12.0), crate::theme::muted());
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    job
}

pub fn paint(ui: &egui::Ui, rect: egui::Rect, name: &str, id: impl std::hash::Hash + std::fmt::Debug) {
    let galley = ui.fonts_mut(|f| f.layout_job(name_job(name, rect.width())));
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        crate::theme::muted(),
    );
    ui.interact(rect, ui.id().with(id), egui::Sense::hover()).on_hover_text(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_wins_and_missing_names_are_blank() {
        let mut names = Names::default();
        let key = "CL1 Comdty";
        names.insert(key.into(), Some("Untrusted replacement".into()), 100);
        assert_eq!(names.name(key), Some("WTI Crude Oil Future"));
        assert!(!names.needs(key, 100));
        assert_eq!(names.name("ZZUNKNOWN US Equity"), None);
    }

    #[test]
    fn exact_key_only_never_a_fuzzy_guess() {
        let hit = |key: &str| tr_data::SearchHit {
            key: key.into(),
            name: "  Provider\n name  ".into(),
            class: tr_core::AssetClass::Equity,
            exchange: None,
        };
        assert_eq!(exact_name("ZZUNKNOWN US Equity", &[hit("ZZUNKNOWN LN Equity")]), None);
        assert_eq!(exact_name("ZZUNKNOWN US Equity", &[hit("ZZUNKNOWN US Equity")]), Some("Provider name".into()));
    }

    #[test]
    fn negative_cache_expires_after_seven_days() {
        let mut names = Names::default();
        let key = "ZZUNKNOWN US Equity";
        names.insert(key.into(), None, 100);
        assert!(!names.needs(key, 100 + TTL - 1));
        assert!(names.needs(key, 100 + TTL));
    }

    #[test]
    fn cache_sanitizes_caps_round_trips_and_preserves_corruption() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target")
            .join(format!("names-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("security-names.json");
        let mut entries = BTreeMap::new();
        for i in 0..LIMIT + 10 {
            entries.insert(format!("ZZ{i:05} US Equity"), Entry::Name(format!(" \n{}\t ", "é".repeat(100))));
        }
        entries.insert("bad\nkey".into(), Entry::Name("Bad".into()));
        entries.insert("OLD US Equity".into(), Entry::Missing { checked_at: 0 });
        entries.insert("FUTURE US Equity".into(), Entry::Missing { checked_at: i64::MAX });
        std::fs::write(&path, serde_json::to_vec(&entries).unwrap()).unwrap();
        let names = Names::load(&path, TTL + 1);
        assert_eq!(names.0.len(), LIMIT);
        assert!(names.0.values().all(
            |entry| matches!(entry, Entry::Name(n) if n.chars().count() == 80 && !n.chars().any(char::is_control))
        ));
        names.save(&path);
        assert_eq!(Names::load(&path, TTL + 1).0.len(), LIMIT);
        let corrupt = b"{not json";
        std::fs::write(&path, corrupt).unwrap();
        assert!(Names::load(&path, 123).0.is_empty());
        assert_eq!(std::fs::read(path.with_extension("json.corrupt-123")).unwrap(), corrupt);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_names_sort_last_in_both_directions() {
        for descending in [false, true] {
            let mut names = [None, Some("Beta"), Some("alpha")];
            names.sort_by(|a, b| order(*a, *b, descending));
            assert_eq!(names[2], None);
            assert_eq!(names[0], Some(if descending { "Beta" } else { "alpha" }));
        }
    }

    #[test]
    fn narrow_width_hides_name_before_custom_columns() {
        assert!(show_column(crate::theme::dp(700.0), 2, true));
        assert!(!show_column(crate::theme::dp(699.0), 2, true));
        // Both custom columns still fit at this width (380 + 2 * 85).
        assert!(!show_column(crate::theme::dp(550.0), 2, true));
        assert!(!show_column(crate::theme::dp(1000.0), 0, false));
    }

    #[test]
    fn truncation_keeps_full_hover_text() {
        let name = "A very long instrument name that must not overflow the available width";
        assert_eq!(hover("MSTR", Some(name)), format!("MSTR · {name}"));
        assert_eq!(hover("MSTR", None), "MSTR");
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(70.0, 22.0));
            let galley = ui.fonts_mut(|f| f.layout_job(name_job(name, rect.width())));
            assert!(galley.elided);
            assert!(galley.size().x <= rect.width());
            assert_eq!(galley.job.text, name);
            paint(ui, rect, name, "name-test");
        });
        output.textures_delta.clear();
    }
}
