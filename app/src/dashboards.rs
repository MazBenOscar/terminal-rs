//! Pristine built-in dashboards; opening one always creates a new saved workspace.
use crate::{
    dock::{Dir, Node},
    layout::Layout,
    layouts::{FeedSettings, LayoutDoc, Link, NewsSettings, PaneSettings, ScanSettings, chart_settings},
    prefs::PanelDefaults,
};

pub struct Dashboard {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub build: fn(&PanelDefaults) -> LayoutDoc,
    /// On-only: true enables the app-level tape; false preserves the user's preference.
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
pub const ALL: &[Dashboard] = &[
    Dashboard {
        id: "NEWS",
        name: "News desk",
        description: "Market headlines, story inspector and linked SPY chart",
        build: news_desk,
        headline_tape: true,
        quote_strip: false,
    },
    Dashboard {
        id: "BRIEF",
        name: "Morning brief",
        description: "World markets, yield curve, today's market stories and linked SPY chart",
        build: brief,
        headline_tape: true,
        quote_strip: true,
    },
    Dashboard {
        id: "NAME",
        name: "Single-name desk",
        description: "Linked AAPL chart, options with greeks, symbol news and congressional trades",
        build: single_name,
        headline_tape: false,
        quote_strip: false,
    },
    Dashboard {
        id: "FLOW",
        name: "Smart money",
        description: "Congress trades, politicians and watchlist filings; ticker click / C drives the chart",
        build: flow,
        headline_tape: false,
        quote_strip: false,
    },
    Dashboard {
        id: "MACRO",
        name: "Rates & data",
        description: "Yield curve, US10Y chart and seven-day CPI, FOMC, payrolls or inflation stories",
        build: macro_desk,
        headline_tape: false,
        quote_strip: false,
    },
    Dashboard {
        id: "SCAN",
        name: "Idea scan",
        description: "Scanner with linked SPY chart and symbol-scoped news",
        build: scan,
        headline_tape: false,
        quote_strip: false,
    },
    Dashboard {
        id: "RISK",
        name: "Risk & P&L",
        description: "Positions sorted by P&L, linked SPY chart and price alerts",
        build: risk,
        headline_tape: true,
        quote_strip: false,
    },
    Dashboard {
        id: "WALL",
        name: "Monitor wall",
        description: "Six independent charts: SPY, QQQ, IWM, TLT, crude oil and Bitcoin",
        build: wall,
        headline_tape: true,
        quote_strip: false,
    },
    Dashboard {
        id: "RESEARCH",
        name: "Research desk",
        description: "Market stories and linked Story inspector; Analyze uses the linked Agent pane",
        build: research,
        headline_tape: true,
        quote_strip: false,
    },
    Dashboard {
        id: "CRYPTO",
        name: "Crypto desk",
        description: "Linked BTCUSD chart and book, crypto stories and a Binance spot scanner",
        build: crypto_desk,
        headline_tape: false,
        quote_strip: false,
    },
];
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

fn brief(defaults: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        filters: tr_data::news_store::Filters { scope: tr_data::news_store::Scope::All, days: 1, ..Default::default() },
        flash_high: true,
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "Morning brief",
        Layout::Grid2x2,
        vec![
            PaneSettings::Wei,
            PaneSettings::Curve,
            PaneSettings::News(Some(news)),
            chart_settings("SPY US Equity", defaults),
        ],
    );
    // Left 34%: markets/curve 55:45; right 66%: stories/chart 42:58.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::split_with(Dir::Column, vec![Node::leaf(1), Node::leaf(2)], vec![0.55, 0.45]),
            Node::split_with(Dir::Column, vec![Node::leaf(3), Node::leaf(4)], vec![0.42, 0.58]),
        ],
        vec![0.34, 0.66],
    );
    doc.panes[2].link = Some(Link::Orange);
    doc.panes[3].link = Some(Link::Orange);
    doc.focus = Some(3);
    doc
}

fn single_name(defaults: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        symbol: Some("AAPL US Equity".into()),
        filters: tr_data::news_store::Filters { scope: tr_data::news_store::Scope::Symbol, ..Default::default() },
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "Single-name desk",
        Layout::Grid2x2,
        vec![
            chart_settings("AAPL US Equity", defaults),
            PaneSettings::Options { symbol: "AAPL US Equity".into(), strikes: 10, greeks: true },
            PaneSettings::News(Some(news)),
            PaneSettings::Ticker("AAPL US Equity".into()),
        ],
    );
    // Chart dominates at 60% width; right 40% stacks options/news/congress trades 40:35:25.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::leaf(1),
            Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3), Node::leaf(4)], vec![0.40, 0.35, 0.25]),
        ],
        vec![0.60, 0.40],
    );
    for pane in &mut doc.panes {
        pane.link = Some(Link::Orange);
    }
    doc.focus = Some(1);
    doc
}

fn flow(defaults: &PanelDefaults) -> LayoutDoc {
    let mut doc = LayoutDoc::preset(
        "Smart money",
        Layout::Grid2x2,
        vec![
            PaneSettings::Feed(Some(FeedSettings { tickers_only: true, ..Default::default() })),
            PaneSettings::Politicians,
            PaneSettings::Filings(Default::default()),
            chart_settings("SPY US Equity", defaults),
        ],
    );
    // Left 55%: feed/politicians 65:35; right 45%: filings/chart 45:55, all visible.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::split_with(Dir::Column, vec![Node::leaf(1), Node::leaf(2)], vec![0.65, 0.35]),
            Node::split_with(Dir::Column, vec![Node::leaf(3), Node::leaf(4)], vec![0.45, 0.55]),
        ],
        vec![0.55, 0.45],
    );
    doc.panes[2].link = Some(Link::Orange);
    doc.panes[3].link = Some(Link::Orange);
    doc.focus = Some(1);
    doc
}

fn macro_desk(defaults: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        filters: tr_data::news_store::Filters {
            scope: tr_data::news_store::Scope::All,
            include: "cpi fomc payrolls inflation".into(),
            days: 7,
            ..Default::default()
        },
        flash_high: true,
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "Rates & data",
        Layout::MainRight2,
        vec![PaneSettings::Curve, chart_settings("US10Y Govt", defaults), PaneSettings::News(Some(news))],
    );
    // Curve 52%, right 48%: chart/stories 58:42 (news → chart, not chart → All news).
    doc.dock = Node::split_with(
        Dir::Row,
        vec![Node::leaf(1), Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.58, 0.42])],
        vec![0.52, 0.48],
    );
    doc.panes[1].link = Some(Link::Orange);
    doc.panes[2].link = Some(Link::Orange);
    doc.focus = Some(1);
    doc
}

fn scan(defaults: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        symbol: Some("SPY US Equity".into()),
        filters: tr_data::news_store::Filters { scope: tr_data::news_store::Scope::Symbol, ..Default::default() },
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "Idea scan",
        Layout::MainRight2,
        vec![
            PaneSettings::Scanner(ScanSettings::from(tr_core::scanner::ScanDefinition::default())),
            chart_settings("SPY US Equity", defaults),
            PaneSettings::News(Some(news)),
        ],
    );
    // Scanner 52%, right 48%: chart/news 55:45.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![Node::leaf(1), Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.55, 0.45])],
        vec![0.52, 0.48],
    );
    for pane in &mut doc.panes {
        pane.link = Some(Link::Orange);
    }
    doc.focus = Some(1);
    doc
}

fn risk(defaults: &PanelDefaults) -> LayoutDoc {
    let mut doc = LayoutDoc::preset(
        "Risk & P&L",
        Layout::MainRight2,
        vec![
            PaneSettings::Portfolio {
                view: crate::command::PortView::Positions,
                account: None,
                symbol: None,
                sort: crate::app::PosSort::Pnl,
                open_only: false,
            },
            chart_settings("SPY US Equity", defaults),
            PaneSettings::Alerts,
        ],
    );
    // Positions 46%, right 54%: chart/alerts 62:38.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![Node::leaf(1), Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.62, 0.38])],
        vec![0.46, 0.54],
    );
    doc.panes[0].link = Some(Link::Orange);
    doc.panes[1].link = Some(Link::Orange);
    doc.focus = Some(1);
    doc
}

fn wall(defaults: &PanelDefaults) -> LayoutDoc {
    // Grid3x2 supplies two equal-height rows, each with three equal-width charts.
    LayoutDoc::preset(
        "Monitor wall",
        Layout::Grid3x2,
        ["SPY US Equity", "QQQ US Equity", "IWM US Equity", "TLT US Equity", "CL1 Comdty", "BTCUSD Curncy"]
            .iter()
            .map(|symbol| chart_settings(symbol, defaults))
            .collect(),
    )
}

fn research(_: &PanelDefaults) -> LayoutDoc {
    let news = NewsSettings {
        filters: tr_data::news_store::Filters { scope: tr_data::news_store::Scope::All, ..Default::default() },
        ..Default::default()
    };
    let mut doc = LayoutDoc::preset(
        "Research desk",
        Layout::MainRight2,
        vec![
            PaneSettings::News(Some(news)),
            PaneSettings::Story { link_follow: true },
            PaneSettings::Agent(Default::default()),
        ],
    );
    // Stories 40%, right 60%: inspector/Agent 55:45, with neither hidden in tabs.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![Node::leaf(1), Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.55, 0.45])],
        vec![0.40, 0.60],
    );
    // story_agent prefers a matching link, then the most recently focused Agent.
    for pane in &mut doc.panes {
        pane.link = Some(Link::Orange);
    }
    doc.focus = Some(1);
    doc
}

fn crypto_desk(defaults: &PanelDefaults) -> LayoutDoc {
    // Finnhub's crypto market job and CoinDesk stories both store the "crypto" category.
    let news = NewsSettings {
        filters: tr_data::news_store::Filters {
            scope: tr_data::news_store::Scope::All,
            categories: vec!["crypto".into()],
            ..Default::default()
        },
        flash_high: true,
        ..Default::default()
    };
    // Refresh stays off: a schedule runs the universe scan on the first frame instead of
    // waiting for Run scan.
    let scan = ScanSettings {
        universe: tr_data::universe::Universe::BinanceSpot,
        ..ScanSettings::from(tr_core::scanner::ScanDefinition::default())
    };
    let mut doc = LayoutDoc::preset(
        "Crypto desk",
        Layout::Grid2x2,
        vec![
            chart_settings("BTCUSD Curncy", defaults),
            PaneSettings::OrderBook { symbol: "BTCUSD Curncy".into(), depth: 25, group: 10.0 },
            PaneSettings::News(Some(news)),
            PaneSettings::Scanner(scan),
        ],
    );
    // Chart dominates at 60% width; left chart/scanner 68:32, right book/news 55:45.
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::split_with(Dir::Column, vec![Node::leaf(1), Node::leaf(4)], vec![0.68, 0.32]),
            Node::split_with(Dir::Column, vec![Node::leaf(2), Node::leaf(3)], vec![0.55, 0.45]),
        ],
        vec![0.60, 0.40],
    );
    doc.panes[0].link = Some(Link::Orange);
    doc.panes[1].link = Some(Link::Orange);
    // Scope::All news ignores incoming symbols, so it stays unlinked.
    doc.panes[3].link = Some(Link::Orange);
    doc.focus = Some(1);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_data::news_store::Scope;

    fn build(id: &str) -> LayoutDoc {
        (ALL.iter().find(|d| d.id == id).unwrap().build)(&Default::default())
    }

    fn news(doc: &LayoutDoc, index: usize) -> &NewsSettings {
        let PaneSettings::News(Some(news)) = &doc.panes[index].settings else { panic!("expected news") };
        news
    }

    fn assert_chart(doc: &LayoutDoc, index: usize, expected: &str) {
        assert!(matches!(&doc.panes[index].settings, PaneSettings::Chart { symbol, .. } if symbol == expected));
    }

    fn assert_links(doc: &LayoutDoc, expected: &[Option<Link>]) {
        assert_eq!(doc.panes.iter().map(|p| p.link).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn every_dashboard_builds_a_self_consistent_doc() {
        use std::collections::HashSet;
        assert_eq!(
            ALL.iter().map(|d| d.id).collect::<Vec<_>>(),
            ["NEWS", "BRIEF", "NAME", "FLOW", "MACRO", "SCAN", "RISK", "WALL", "RESEARCH", "CRYPTO"]
        );
        let mut names = HashSet::new();
        for dashboard in ALL {
            let doc = (dashboard.build)(&Default::default());
            assert_eq!(doc.name, dashboard.name);
            assert!(names.insert(dashboard.name), "{} duplicate name", dashboard.id);
            assert!(!dashboard.description.is_empty() && !dashboard.description.contains('\n'));
            assert!(!doc.panes.is_empty(), "{} has no panes", dashboard.id);
            let ids: Vec<_> = doc.panes.iter().map(|p| p.id).collect();
            assert_eq!(ids, (1..=doc.panes.len() as u64).collect::<Vec<_>>(), "{} ids", dashboard.id);
            let placed = doc.dock.panes();
            assert_eq!(placed.len(), ids.len(), "{} undocked panes", dashboard.id);
            assert_eq!(
                placed.iter().copied().collect::<HashSet<_>>(),
                ids.iter().copied().collect::<HashSet<_>>(),
                "{} missing, unknown or duplicate dock ids",
                dashboard.id
            );
            assert!(doc.panes.iter().all(|p| p.window.is_none()));
            if let Some(focus) = doc.focus {
                assert!(doc.dock.visible().contains(&focus), "{} invalid/hidden focus", dashboard.id);
            }
            for (i, pane) in doc.panes.iter().enumerate() {
                for other in &doc.panes[i + 1..] {
                    if pane.link.is_some() && pane.link == other.link {
                        assert_ne!(
                            doc.dock.find(pane.id),
                            doc.dock.find(other.id),
                            "{} hides linked panes {} and {} in one Tabs group",
                            dashboard.id,
                            pane.id,
                            other.id
                        );
                    }
                }
            }
            assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc, "{} round-trip", dashboard.id);
            let mut repaired = doc.clone();
            repaired.repair();
            assert_eq!(repaired, doc, "{} repair changed preset or dropped panes", dashboard.id);
            assert_eq!(
                dashboard.headline_tape,
                matches!(dashboard.id, "NEWS" | "BRIEF" | "RISK" | "WALL" | "RESEARCH")
            );
        }
    }

    #[test]
    fn brief_starts_with_today_stories_focused_and_only_news_chart_linked() {
        let doc = build("BRIEF");
        assert_eq!(doc.panes.len(), 4);
        assert!(matches!(doc.panes[0].settings, PaneSettings::Wei));
        assert!(matches!(doc.panes[1].settings, PaneSettings::Curve));
        let n = news(&doc, 2);
        assert_eq!(n.filters.scope, Scope::All);
        assert_eq!(n.filters.days, 1);
        assert!(n.flash_high && !n.display.wire);
        assert_chart(&doc, 3, "SPY US Equity");
        assert_links(&doc, &[None, None, Some(Link::Orange), Some(Link::Orange)]);
        assert_eq!(doc.focus, Some(3));
        assert_eq!(doc.dock.visible(), [1, 2, 3, 4]);
    }

    #[test]
    fn name_has_four_visible_aapl_panes_and_a_dominant_focused_chart() {
        let doc = build("NAME");
        assert_eq!(doc.panes.len(), 4);
        assert_chart(&doc, 0, "AAPL US Equity");
        assert!(matches!(&doc.panes[1].settings,
            PaneSettings::Options { symbol, strikes: 10, greeks: true } if symbol == "AAPL US Equity"));
        let n = news(&doc, 2);
        assert_eq!(n.symbol.as_deref(), Some("AAPL US Equity"));
        assert_eq!(n.filters.scope, Scope::Symbol);
        assert_eq!(doc.panes[3].settings, PaneSettings::Ticker("AAPL US Equity".into()));
        assert_links(&doc, &[Some(Link::Orange); 4]);
        assert_eq!(doc.focus, Some(1));
        assert_eq!(doc.dock.visible(), [1, 2, 3, 4]);
        let Node::Split { dir: Dir::Row, shares, children } = &doc.dock else { panic!() };
        assert_eq!(shares, &[0.60, 0.40]);
        assert_eq!(children[0], Node::leaf(1));
        assert!(matches!(&children[1], Node::Split { shares, .. } if shares == &[0.40, 0.35, 0.25]));
    }

    #[test]
    fn flow_keeps_filings_and_its_linked_chart_visible() {
        let doc = build("FLOW");
        assert_eq!(doc.panes.len(), 4);
        assert_eq!(
            doc.panes[0].settings,
            PaneSettings::Feed(Some(FeedSettings { tickers_only: true, ..Default::default() }))
        );
        assert_eq!(doc.panes[1].settings, PaneSettings::Politicians);
        assert_eq!(doc.panes[2].settings, PaneSettings::Filings(Default::default()));
        assert_chart(&doc, 3, "SPY US Equity");
        assert_links(&doc, &[None, None, Some(Link::Orange), Some(Link::Orange)]);
        assert_eq!(doc.dock.visible(), [1, 2, 3, 4]);
        assert_ne!(doc.dock.find(3), doc.dock.find(4));
        assert_eq!(doc.focus, Some(1));
        let description = ALL.iter().find(|d| d.id == "FLOW").unwrap().description;
        assert!(description.contains("ticker click / C"));
    }

    #[test]
    fn wall_is_an_unlinked_equal_share_grid_of_six_distinct_charts() {
        let doc = build("WALL");
        let symbols =
            ["SPY US Equity", "QQQ US Equity", "IWM US Equity", "TLT US Equity", "CL1 Comdty", "BTCUSD Curncy"];
        assert_eq!(doc.panes.len(), symbols.len());
        for (index, symbol) in symbols.iter().enumerate() {
            assert_chart(&doc, index, symbol);
        }
        assert_links(&doc, &[None; 6]);
        assert_eq!(doc.dock, Node::preset(Layout::Grid3x2, &[1, 2, 3, 4, 5, 6]));
        assert_eq!(doc.dock.visible(), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn scan_links_the_scanner_chart_and_symbol_news_deterministically() {
        let doc = build("SCAN");
        assert_eq!(
            doc.panes[0].settings,
            PaneSettings::Scanner(ScanSettings::from(tr_core::scanner::ScanDefinition::default()))
        );
        assert_chart(&doc, 1, "SPY US Equity");
        assert_eq!(news(&doc, 2).filters.scope, Scope::Symbol);
        assert_eq!(news(&doc, 2).symbol.as_deref(), Some("SPY US Equity"));
        assert_links(&doc, &[Some(Link::Orange); 3]);
        assert_eq!(doc.focus, Some(1));
    }

    #[test]
    fn research_uses_stories_following_inspector_and_linked_agent_in_separate_slots() {
        let doc = build("RESEARCH");
        assert_eq!(doc.panes.len(), 3);
        assert_eq!(news(&doc, 0).filters.scope, Scope::All);
        assert!(!news(&doc, 0).display.wire);
        assert_eq!(doc.panes[1].settings, PaneSettings::Story { link_follow: true });
        assert_eq!(doc.panes[2].settings, PaneSettings::Agent(Default::default()));
        assert_links(&doc, &[Some(Link::Orange); 3]);
        assert_eq!(doc.dock.visible(), [1, 2, 3]);
        assert_eq!(doc.focus, Some(1));
    }

    #[test]
    fn macro_uses_seven_day_any_term_filter_and_risk_uses_positions_pnl() {
        let doc = build("MACRO");
        assert_eq!(doc.panes[0].settings, PaneSettings::Curve);
        assert_chart(&doc, 1, "US10Y Govt");
        let n = news(&doc, 2);
        assert_eq!(n.filters.scope, Scope::All);
        assert_eq!(n.filters.include, "cpi fomc payrolls inflation");
        assert!(n.filters.search.is_empty());
        assert_eq!(n.filters.days, 7);
        assert!(n.flash_high);
        assert_links(&doc, &[None, Some(Link::Orange), Some(Link::Orange)]);
        let doc = build("RISK");
        assert_eq!(
            doc.panes[0].settings,
            PaneSettings::Portfolio {
                view: crate::command::PortView::Positions,
                account: None,
                symbol: None,
                sort: crate::app::PosSort::Pnl,
                open_only: false,
            }
        );
        assert_chart(&doc, 1, "SPY US Equity");
        assert_eq!(doc.panes[2].settings, PaneSettings::Alerts);
        assert_links(&doc, &[Some(Link::Orange), Some(Link::Orange), None]);
    }

    #[test]
    fn crypto_desk_links_the_btc_chart_book_and_scanner_and_scopes_news_to_crypto() {
        let doc = build("CRYPTO");
        assert_eq!(doc.panes.len(), 4);
        assert_chart(&doc, 0, "BTCUSD Curncy");
        assert_eq!(
            doc.panes[1].settings,
            PaneSettings::OrderBook { symbol: "BTCUSD Curncy".into(), depth: 25, group: 10.0 }
        );
        let n = news(&doc, 2);
        assert_eq!(n.filters.scope, Scope::All);
        assert_eq!(n.filters.categories, ["crypto"]);
        assert!(n.symbol.is_none() && n.flash_high);
        let PaneSettings::Scanner(scan) = &doc.panes[3].settings else { panic!("expected scanner") };
        assert_eq!(scan.definition, tr_core::scanner::ScanDefinition::default());
        assert_eq!(scan.universe, tr_data::universe::Universe::BinanceSpot);
        // A refresh schedule runs the market-wide scan on the first frame; the desk waits
        // for the user to press Run scan.
        assert!(scan.refresh.is_none());
        assert_links(&doc, &[Some(Link::Orange), Some(Link::Orange), None, Some(Link::Orange)]);
        assert_eq!(doc.focus, Some(1));
        // Dock order is left column (chart, scanner) then right (book, news); all four visible.
        assert_eq!(doc.dock.visible(), [1, 4, 2, 3]);
        let Node::Split { dir: Dir::Row, shares, children } = &doc.dock else { panic!() };
        assert_eq!(shares, &[0.60, 0.40]);
        assert!(matches!(&children[0], Node::Split { dir: Dir::Column, shares, .. } if shares == &[0.68, 0.32]));
        assert!(matches!(&children[1], Node::Split { dir: Dir::Column, shares, .. } if shares == &[0.55, 0.45]));
        let dashboard = ALL.iter().find(|d| d.id == "CRYPTO").unwrap();
        assert!(!dashboard.headline_tape, "CRYPTO must not enable the headline tape");
        assert!(!dashboard.quote_strip, "CRYPTO must not enable the quote strip");
        assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc, "open > save > reopen");
    }

    #[test]
    fn crypto_news_categories_match_the_stored_crypto_category() {
        let filters = news(&build("CRYPTO"), 2).filters.clone();
        let item = tr_data::NewsItem {
            title: "Bitcoin ticks higher".into(),
            publisher: String::new(),
            link: String::new(),
            ts: 1,
            tickers: Vec::new(),
        };
        let mut story = tr_data::news_store::Story::new(item, "CoinDesk");
        story.category = "crypto".into();
        let query = tr_data::news_store::Query { filters, ..Default::default() };
        assert!(tr_data::news_store::matches(&story, &query));
        story.category = "general".into();
        assert!(!tr_data::news_store::matches(&story, &query));
    }

    #[test]
    fn dashboard_strips_are_on_only() {
        assert!(!ALL[0].quote_strip, "NEWS must not enable the quote strip");
        let strip: Vec<_> = ALL.iter().filter(|d| d.quote_strip).map(|d| d.id).collect();
        assert_eq!(strip, ["BRIEF"], "only the Morning brief turns the quote strip on");
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
