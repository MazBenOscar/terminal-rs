//! Pristine built-in dashboards; opening one always creates a new saved workspace.
use crate::{
    dock::{Dir, Node},
    layout::Layout,
    layouts::{LayoutDoc, Link, NewsSettings, PaneSettings},
    prefs::PanelDefaults,
};

pub struct Dashboard {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub build: fn(&PanelDefaults) -> LayoutDoc,
    /// Turn the app-level headline tape on when this dashboard opens.
    pub headline_tape: bool,
    /// Turn the app-level quote strip on; false preserves the user's choice.
    pub quote_strip: bool,
}

impl Dashboard {
    /// Dashboard chrome preferences are deliberately on-only.
    pub fn enable_strips(&self, prefs: &mut crate::prefs::Prefs) -> bool {
        let changed = (self.headline_tape && !prefs.headline_tape) || (self.quote_strip && !prefs.quote_strip);
        prefs.headline_tape |= self.headline_tape;
        prefs.quote_strip |= self.quote_strip;
        changed
    }
}
pub const ALL: &[Dashboard] = &[Dashboard {
    id: "NEWS",
    name: "News desk",
    description: "Market headlines, story inspector and linked SPY chart",
    build: news_desk,
    headline_tape: true,
    quote_strip: false,
}];
fn news_desk(defaults: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        filters: tr_data::news_store::Filters { scope: tr_data::news_store::Scope::All, ..Default::default() },
        flash_high: true,
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "News desk",
        Layout::MainRight2,
        vec![
            PaneSettings::News(Some(news)),
            PaneSettings::Story { link_follow: true },
            crate::layouts::chart_settings("SPY US Equity", defaults),
        ],
    );
    doc.dock = Node::split_with(
        Dir::Row,
        vec![Node::leaf(1), Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.62, 0.38])],
        vec![0.64, 0.36],
    );
    for pane in &mut doc.panes {
        pane.link = Some(Link::Orange);
    }
    doc.focus = Some(1);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_strips_are_on_only() {
        assert!(!ALL[0].quote_strip, "NEWS must not enable the quote strip");
        let brief = Dashboard { quote_strip: true, ..ALL[0] };
        for initial in [false, true] {
            let mut prefs = crate::prefs::Prefs { quote_strip: initial, headline_tape: true, ..Default::default() };
            assert_eq!(brief.enable_strips(&mut prefs), !initial);
            assert!(prefs.quote_strip);
            assert!(!ALL[0].enable_strips(&mut prefs));
            assert!(prefs.quote_strip, "NEWS preserves an enabled strip");
        }
    }

    #[test]
    fn news_desk_has_expected_shares_links_and_legacy_defaults() {
        let doc = (ALL[0].build)(&Default::default());
        assert_eq!(doc.dock.panes(), vec![1, 2, 3]);
        assert!(doc.panes.iter().all(|p| p.link == Some(Link::Orange)));
        let Node::Split { shares, children, dir: Dir::Row } = &doc.dock else { panic!() };
        assert_eq!(shares, &[0.64, 0.36]);
        assert!(matches!(&children[1], Node::Split { dir: Dir::Column, shares, .. } if shares == &[0.62,0.38]));
        assert!(
            matches!(&doc.panes[0].settings, PaneSettings::News(Some(n)) if n.filters.scope == tr_data::news_store::Scope::All && !n.display.wire && n.flash_high)
        );
        assert!(matches!(&doc.panes[2].settings, PaneSettings::Chart { symbol, .. } if symbol == "SPY US Equity"));
        assert!(ALL[0].headline_tape);
        assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
        assert_eq!(
            serde_json::from_str::<PaneSettings>(r#"{"Story":{}}"#).unwrap(),
            PaneSettings::Story { link_follow: true }
        );
        assert!(
            LayoutDoc::from_json(
                br#"{"name":"Old","dock":{"type":"tabs","panes":[1]},"panes":[{"id":1,"News":null}]}"#
            )
            .is_ok()
        );
    }
}
