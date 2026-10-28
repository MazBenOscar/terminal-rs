//! Small, non-interactive markdown subset for agent replies.
use crate::theme::{self, dp};
use egui::{Color32, TextFormat, Ui, text::LayoutJob};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(String),
    Heading(u8, String),
    Bullet(String),
    Numbered(String, String),
    Code(String),
    Table(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Link(String, String),
}

pub fn inline(original: &str) -> Vec<Span> {
    // Each delimiter cursor only moves forwards, including after a failed match.
    // In particular, a run of unclosed '[' must not rescan the remaining reply.
    let positions = |mark: &'static str| {
        original
            .as_bytes()
            .windows(mark.len())
            .enumerate()
            .filter_map(move |(i, bytes)| (bytes == mark.as_bytes()).then_some(i))
            .peekable()
    };
    fn next_at<I: Iterator<Item = usize>>(cursor: &mut std::iter::Peekable<I>, start: usize) -> Option<usize> {
        while cursor.peek().is_some_and(|i| *i < start) {
            cursor.next();
        }
        cursor.peek().copied()
    }
    let mut bold = positions("**");
    let mut italic = positions("*");
    let mut code = positions("`");
    let mut link = positions("](");
    let mut close = positions(")");
    let mut spans = Vec::new();
    let mut offset = 0;
    while offset < original.len() {
        let text = &original[offset..];
        let mut matched = false;
        for (mark, kind, cursor) in [("**", 0, &mut bold), ("*", 1, &mut italic), ("`", 2, &mut code)] {
            if text.starts_with(mark)
                && let Some(end) = next_at(cursor, offset + mark.len())
            {
                let content = original[offset + mark.len()..end].to_owned();
                spans.push(match kind {
                    0 => Span::Bold(content),
                    1 => Span::Italic(content),
                    _ => Span::Code(content),
                });
                offset = end + mark.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if text.starts_with('[')
            && let Some(end) = next_at(&mut link, offset + 1)
            && let Some(end_url) = next_at(&mut close, end + 2)
        {
            spans.push(Span::Link(original[offset + 1..end].into(), original[end + 2..end_url].into()));
            offset = end_url + 1;
            continue;
        }
        let end =
            text.char_indices().skip(1).find(|(_, c)| matches!(c, '*' | '`' | '[')).map_or(text.len(), |(i, _)| i);
        if let Some(Span::Text(previous)) = spans.last_mut() {
            previous.push_str(&text[..end]);
        } else {
            spans.push(Span::Text(text[..end].into()));
        }
        offset += end;
    }
    spans
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut lines = text.lines().peekable();
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    };
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with("```") {
            flush(&mut paragraph, &mut blocks);
            let mut code = Vec::new();
            for line in lines.by_ref() {
                if line.trim().starts_with("```") {
                    break;
                }
                code.push(line);
            }
            blocks.push(Block::Code(code.join("\n")));
        } else if line.starts_with('|') {
            flush(&mut paragraph, &mut blocks);
            let mut rows = vec![line];
            while lines.peek().is_some_and(|line| line.trim().starts_with('|')) {
                rows.push(lines.next().unwrap().trim());
            }
            blocks.push(Block::Table(grid(&rows)));
        } else if line.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Bullet(text.into()));
        } else if let Some((number, text)) = line.split_once(". ")
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Numbered(number.into(), text.into()));
        } else {
            let level = line.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
                flush(&mut paragraph, &mut blocks);
                blocks.push(Block::Heading(level as u8, line[level + 1..].into()));
            } else {
                paragraph.push(line);
            }
        }
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn grid(rows: &[&str]) -> String {
    // Keep the source intact. The grid owns a horizontal scroll area, not an ellipsis.
    rows.join("\n")
}

/// Frame-ready blocks: block and inline parsing happens only when a message changes.
#[derive(Debug, Clone)]
pub enum DisplayBlock {
    Rich(Vec<Span>),
    List(String, Vec<Span>),
    Heading(u8, String),
    Monospace(String),
    Table(String),
}
pub fn prepare(blocks: &[Block]) -> Vec<DisplayBlock> {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(text) => DisplayBlock::Rich(inline(text)),
            Block::Bullet(text) => DisplayBlock::List(String::new(), inline(text)),
            Block::Numbered(number, text) => DisplayBlock::List(format!("{number}."), inline(text)),
            Block::Heading(level, text) => DisplayBlock::Heading(*level, text.clone()),
            Block::Code(text) => DisplayBlock::Monospace(text.clone()),
            Block::Table(text) => DisplayBlock::Table(text.clone()),
        })
        .collect()
}

/// Byte ranges of comparable figures, not identifiers or entire sentences.
pub fn numeric_tokens(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if matches!(bytes[i], b'$' | b'+' | b'-') {
            i += 1;
        }
        if i < bytes.len() && bytes[i].is_ascii_digit() && (start == 0 || !bytes[start - 1].is_ascii_alphanumeric()) {
            i += 1;
            while i < bytes.len() {
                if bytes[i].is_ascii_digit()
                    || (matches!(bytes[i], b',' | b'.' | b':') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
                {
                    i += 1;
                } else {
                    break;
                }
            }
            if bytes.get(i) == Some(&b'%') {
                i += 1;
            }
            if !bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
                ranges.push(start..i);
            }
        } else {
            i = start + 1;
        }
    }
    ranges
}

pub fn wrapped_job(text: &str, width: f32, font: egui::FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width.max(1.0);
    job.append(text, 0.0, TextFormat { font_id: font, color, ..Default::default() });
    job
}

pub fn wrapped(ui: &mut Ui, text: &str, color: Color32) -> egui::Response {
    let mut job = rich_job(&[Span::Text(text.into())], ui.available_width(), 13.0);
    for section in &mut job.sections {
        section.format.color = color;
    }
    ui.add(egui::Label::new(job).wrap())
}

fn rich_job(spans: &[Span], width: f32, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width.max(1.0);
    for span in spans {
        let (text, bold, italic, code) = match span {
            Span::Text(t) => (t.clone(), false, false, false),
            Span::Bold(t) => (t.clone(), true, false, false),
            Span::Italic(t) => (t.clone(), false, true, false),
            Span::Code(t) => (t.clone(), false, false, true),
            Span::Link(label, url) => (format!("{label} ({url})"), false, false, false),
        };
        // Prose figures share the body font and kerning; mono is only for code
        // and aligned contexts such as table cells, never individual prose tokens.
        append_run(&mut job, &text, size, bold, italic, code);
    }
    job
}

/// Bold weight on the variable system font (SF Pro). One shaped run, one paint;
/// on a non-variable fallback face the coordinate is ignored and text stays regular.
const BOLD_WEIGHT: f32 = 680.0;

fn append_run(job: &mut LayoutJob, text: &str, size: f32, bold: bool, italic: bool, code: bool) {
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: if code { theme::mono(size) } else { theme::body_font(size) },
            color: theme::text(),
            background: if code { theme::control() } else { Color32::TRANSPARENT },
            coords: if bold && !code {
                egui::epaint::text::VariationCoords::new([(b"wght", BOLD_WEIGHT)])
            } else {
                Default::default()
            },
            italics: italic,
            ..Default::default()
        },
    );
}

fn rich(ui: &mut Ui, spans: &[Span], size: f32) {
    let width = ui.available_width();
    let galley = ui.fonts_mut(|f| f.layout_job(rich_job(spans, width, size)));
    let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.min, galley, theme::text());
    // One layout and one ink pass. Re-laying out a transparent-mask copy for
    // synthetic bold can change shaping at span boundaries and double-paint
    // glyphs at different positions (especially in mixed emphasis/numbers).
    // Keep semantic emphasis in the parsed spans; never simulate font weight
    // with a second, independently shaped galley.
}

pub fn show(ui: &mut Ui, blocks: &[DisplayBlock]) {
    for (index, block) in blocks.iter().enumerate() {
        ui.push_id(index, |ui| match block {
            DisplayBlock::Rich(spans) => rich(ui, spans, 13.0),
            DisplayBlock::List(marker, spans) => {
                ui.horizontal_top(|ui| {
                    let width = ui
                        .painter()
                        .layout_no_wrap(marker.clone(), theme::mono(13.0), theme::text())
                        .size()
                        .x
                        .clamp(dp(14.0), (ui.available_width() * 0.25).max(dp(14.0)));
                    let marker_job = wrapped_job(marker, width, theme::mono(13.0), theme::text());
                    let marker_galley = ui.fonts_mut(|f| f.layout_job(marker_job));
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(width, marker_galley.size().y.max(dp(16.0))),
                        egui::Sense::hover(),
                    );
                    if marker.is_empty() {
                        ui.painter().circle_filled(rect.center(), dp(1.5), theme::text());
                    } else {
                        ui.painter().galley(
                            rect.right_top() - egui::vec2(marker_galley.size().x, 0.0),
                            marker_galley,
                            theme::text(),
                        );
                    }
                    rich(ui, spans, 13.0);
                });
            }
            DisplayBlock::Heading(level, text) => {
                rich(ui, &[Span::Bold(text.clone())], if *level == 1 { 16.0 } else { 14.0 })
            }
            DisplayBlock::Monospace(text) => {
                egui::ScrollArea::horizontal().id_salt("code").auto_shrink([false, true]).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::mono(13.0))).extend());
                });
            }
            DisplayBlock::Table(text) => table(ui, text),
        });
    }
}

fn table(ui: &mut Ui, text: &str) {
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|r| r.trim_matches('|').split('|').map(str::trim).collect())
        .filter(|r: &Vec<&str>| !r.iter().all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))))
        .collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let is_numeric =
        |cell: &str| matches!(numeric_tokens(cell).as_slice(), [range] if range.start == 0 && range.end == cell.len());
    let numeric: Vec<_> = (0..columns)
        .map(|column| {
            let values: Vec<_> =
                rows.iter().skip(1).filter_map(|row| row.get(column)).filter(|cell| !cell.is_empty()).collect();
            !values.is_empty() && values.iter().all(|cell| is_numeric(cell))
        })
        .collect();
    let mut widths = vec![0.0_f32; columns];
    for (row_index, row) in rows.iter().enumerate() {
        for (column, cell) in row.iter().enumerate() {
            let font = if row_index > 0 && numeric[column] { theme::mono(13.0) } else { theme::body_font(13.0) };
            let width = ui.painter().layout_no_wrap((*cell).into(), font, theme::text()).size().x;
            widths[column] = widths[column].max(if numeric[column] { width } else { width.clamp(dp(48.0), dp(220.0)) });
        }
    }
    let gap = dp(12.0);
    let gaps = gap * columns.saturating_sub(1) as f32;
    let overflow = (widths.iter().sum::<f32>() + gaps - ui.available_width()).max(0.0);
    let flexible =
        widths.iter().enumerate().filter(|(c, _)| !numeric[*c]).map(|(_, w)| (w - dp(48.0)).max(0.0)).sum::<f32>();
    if flexible > 0.0 {
        for (column, width) in widths.iter_mut().enumerate() {
            if !numeric[column] {
                *width -= (*width - dp(48.0)).max(0.0) * (overflow / flexible).min(1.0);
            }
        }
    }
    let mut heights = Vec::with_capacity(rows.len());
    let cells: Vec<Vec<_>> = rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let mut height = dp(18.0);
            let cells = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let color =
                        if row_index == 0 { crate::agent_pane::readable(theme::muted()) } else { theme::text() };
                    let font =
                        if row_index > 0 && numeric[column] { theme::mono(13.0) } else { theme::body_font(13.0) };
                    let job = wrapped_job(cell, widths[column], font, color);
                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                    height = height.max(galley.size().y);
                    (numeric[column], galley)
                })
                .collect();
            heights.push(height + dp(12.0));
            cells
        })
        .collect();
    let width = widths.iter().sum::<f32>() + gaps;
    let height = heights.iter().sum::<f32>();
    egui::ScrollArea::horizontal().id_salt("table-scroll").auto_shrink([false, true]).show(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let mut y = rect.top();
        // Explicit column tracks avoid nested RTL layouts bleeding into the next
        // cell. No row backgrounds: header ink alone distinguishes the header.
        for (row_index, (row, height)) in cells.iter().zip(heights).enumerate() {
            let mut x = rect.left();
            for ((numeric, galley), width) in row.iter().zip(&widths) {
                let left = if *numeric { x + width - galley.size().x } else { x };
                ui.painter().galley(egui::pos2(left, y + dp(6.0)), galley.clone(), theme::text());
                x += width + gap;
            }
            y += height;
            if row_index == 0 {
                ui.painter().hline(rect.x_range(), y, egui::Stroke::new(dp(1.0), theme::border()));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_tokens_are_figures_not_whole_sentences_or_identifiers() {
        let text = "Levels 62,400 / $68,200 / 56.8 / +2.4% / 13:42; RSI 14, gpt6 foo42 1W.";
        let tokens: Vec<_> = numeric_tokens(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(tokens, ["62,400", "$68,200", "56.8", "+2.4%", "13:42", "14"]);
        assert_eq!(numeric_tokens("€ café 値 -2.0%").len(), 1);
        let job = rich_job(&inline("Support **$62,400**, rising."), 360.0, 13.0);
        assert!(job.sections.iter().all(|s| s.format.font_id.family == egui::FontFamily::Proportional));
        assert!(job.sections.iter().all(|s| s.format.extra_letter_spacing == 0.0));
        let prose = rich_job(&inline("At **$62,400** by 20:00; `RSI(14)`"), 360.0, 13.0);
        for section in &prose.sections {
            let code = &prose.text[section.byte_range.start.0..section.byte_range.end.0] == "RSI(14)";
            assert_eq!(section.format.font_id.family == egui::FontFamily::Monospace, code);
        }
        assert!(prose.sections.iter().all(|s| s.format.color == theme::text()));
        // Bold is a real weight on the same face, not a second painted copy.
        let bold = rich_job(&inline("a **bold** b"), 360.0, 13.0);
        let weighted: Vec<_> = bold
            .sections
            .iter()
            .filter(|s| s.format.coords != Default::default())
            .map(|s| &bold.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(weighted, ["bold"]);
    }

    #[test]
    fn mixed_emphasis_and_figures_have_one_ink_pass_at_all_pane_widths() {
        for width in [240.0, 360.0, 660.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0))),
                    ..Default::default()
                },
                |ui| show(ui, &prepare(&parse("**Verdict:** trend is **up**, at **80,799.51**."))),
            );
            let mut text = Vec::new();
            fn collect(shape: &egui::Shape, text: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) => text.push(t.galley.text().into()),
                    egui::Shape::Vec(shapes) => {
                        for s in shapes {
                            collect(s, text);
                        }
                    }
                    _ => {}
                }
            }
            for shape in &mut output.shapes {
                collect(&shape.shape, &mut text);
            }
            output.textures_delta.clear();
            assert_eq!(text, ["Verdict: trend is up, at 80,799.51."]);
        }
    }

    #[test]
    fn demo_table_column_positions_never_overlap_and_use_measured_fonts() {
        let table = parse(crate::agent_demo::MARKDOWN)
            .into_iter()
            .find_map(|block| if let Block::Table(source) = block { Some(source) } else { None })
            .unwrap();
        let rows = [
            ["Window", "Close", "Change", "RSI", "Bias"],
            ["1 week", "$65,840", "+2.4%", "56.8", "Recovering"],
            ["4 weeks", "$62,100", "+6.0%", "49.2", "Neutral"],
        ];
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for width in [360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let themes = crate::themes::builtins();
                theme::configure(&ctx, themes.iter().find(|t| t.name == name).unwrap(), theme::DEFAULT_ACCENT);
                let mut parent_width = 0.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        ..Default::default()
                    },
                    |ui| {
                        super::table(ui, &table);
                        parent_width = ui.min_rect().width();
                    },
                );
                output.textures_delta.clear();
                assert!(parent_width <= width, "{name} {width}: table expanded pane");
                let cells: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| if let egui::Shape::Text(text) = &shape.shape { Some(text) } else { None })
                    .collect();
                let cell = |s: &str| *cells.iter().find(|t| t.galley.text() == s).unwrap();
                let bounds = |s: &str| {
                    let t = cell(s);
                    t.galley.rect.translate(t.pos.to_vec2())
                };
                for row in rows {
                    for pair in row.windows(2) {
                        assert!(
                            bounds(pair[0]).right() + dp(12.0) <= bounds(pair[1]).left() + 0.01,
                            "{name} {width}: {} overlaps {}",
                            pair[0],
                            pair[1]
                        );
                    }
                }
                for column in 0..5 {
                    let left = rows.iter().map(|r| bounds(r[column]).left()).fold(f32::INFINITY, f32::min);
                    let right = rows.iter().map(|r| bounds(r[column]).right()).fold(0.0_f32, f32::max);
                    let widest = rows.iter().map(|r| bounds(r[column]).width()).fold(0.0_f32, f32::max);
                    assert!((right - left - widest).abs() < 0.1);
                    if (1..=3).contains(&column) {
                        assert!((bounds(rows[1][column]).right() - bounds(rows[2][column]).right()).abs() < 0.1);
                    } else {
                        assert!((bounds(rows[1][column]).left() - bounds(rows[2][column]).left()).abs() < 0.1);
                    }
                }
                for (row_index, row) in rows.iter().enumerate() {
                    for (column, text) in row.iter().enumerate() {
                        let formats = &cell(text).galley.job.sections;
                        assert_eq!(
                            formats[0].format.color,
                            if row_index == 0 { crate::agent_pane::readable(theme::muted()) } else { theme::text() }
                        );
                        let numeric = row_index > 0 && (1..=3).contains(&column);
                        assert_eq!(formats[0].format.font_id.family == egui::FontFamily::Monospace, numeric);
                    }
                }
            }
        }
    }

    #[test]
    fn code_scrolls_but_long_table_text_wraps_without_losing_content() {
        let ctx = egui::Context::default();
        let long = "BTCUSD".repeat(100);
        let blocks =
            prepare(&parse(&format!("```\n{long}\n```\n\n| Level | Value |\n| --- | --- |\n| {long} | 62,400 |")));
        let mut parent_width = 0.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 700.0))),
                ..Default::default()
            },
            |ui| {
                show(ui, &blocks);
                parent_width = ui.min_rect().width();
            },
        );
        output.textures_delta.clear();
        assert!(parent_width <= 360.0, "scroll block widened the pane to {parent_width}");
        let overwide: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == long => Some((t.galley.size().x, s.clip_rect.width())),
                _ => None,
            })
            .collect();
        assert!(overwide.len() >= 2, "code and table both preserve the full cell");
        assert!(overwide.iter().any(|(content, viewport)| *content > 360.0 && *viewport <= 360.0), "code scrolls");
        assert!(overwide.iter().any(|(content, _)| *content <= dp(220.0)), "table text wraps in its bounded column");
        let table_text = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == long && t.galley.size().x <= dp(220.0) => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(table_text.galley.rows.len() > 1);
        assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == theme::border() && stroke.width == dp(1.0))));
    }

    #[test]
    fn wrapping_breaks_long_tokens_without_overwide_rows() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for width in [90.0, 360.0, 450.0, 660.0] {
                for font in [theme::body_font(13.0), theme::mono(13.0)] {
                    for text in ["字éBTC".repeat(300), format!("{} ends here", "long-token".repeat(200))] {
                        let job = wrapped_job(&text, width, font.clone(), theme::text());
                        let galley = ui.fonts_mut(|f| f.layout_job(job));
                        assert!(galley.rows.len() > 1);
                        for row in &galley.rows {
                            assert!(row.size.x <= width + 0.5, "{} > {width}", row.size.x);
                        }
                        assert_eq!(galley.text(), text);
                    }
                }
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn paragraphs_headings_lists_fences_and_unclosed_fences() {
        assert_eq!(
            parse("# Trend\n\nTwo\nlines\n\n- up\n2. down\n```rust\n  x\n```\n```\ny"),
            vec![
                Block::Heading(1, "Trend".into()),
                Block::Paragraph("Two lines".into()),
                Block::Bullet("up".into()),
                Block::Numbered("2".into(), "down".into()),
                Block::Code("  x".into()),
                Block::Code("y".into()),
            ]
        );
    }

    #[test]
    fn inline_styles_and_links_are_literal_not_opened() {
        assert_eq!(
            inline("**bold** *italic* `code` [site](https://example.com)"),
            vec![
                Span::Bold("bold".into()),
                Span::Text(" ".into()),
                Span::Italic("italic".into()),
                Span::Text(" ".into()),
                Span::Code("code".into()),
                Span::Text(" ".into()),
                Span::Link("site".into(), "https://example.com".into()),
            ]
        );
        assert_eq!(inline("*unfinished"), vec![Span::Text("*unfinished".into())]);
    }

    #[test]
    fn unicode_tables_preserve_source_for_scrollable_grid() {
        assert_eq!(
            parse("| A | 値 |\n| --- | --- |\n| long | 2 |"),
            vec![Block::Table("| A | 値 |\n| --- | --- |\n| long | 2 |".into())]
        );
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn pathological_inputs_never_panic() {
        for text in [
            "",
            "#",
            "####### seven",
            "# ",
            "#é heading",
            "```",
            "```\n",
            "|",
            "||||",
            "| a |\n|",
            "**",
            "***",
            "*`[",
            "[](",
            "[a](b",
            "[a](b)",
            "[é](ü)tail",
            "1. ",
            ". x",
            "- ",
            "\u{feff}* x",
            "é*é`é[é",
        ] {
            for block in parse(text) {
                if let Block::Paragraph(text) | Block::Bullet(text) | Block::Numbered(_, text) = block {
                    let _ = inline(&text);
                }
            }
            let _ = inline(text);
        }
        let deep = "> ".repeat(10_000) + &"[".repeat(1000) + &"*".repeat(1000);
        let _ = parse(&deep);
        let _ = inline(&deep);
        let long = "x".repeat(200_000);
        assert_eq!(parse(&long).len(), 1);
    }

    #[test]
    fn table_grid_output_is_bounded_by_input() {
        let header = "|a".repeat(3000) + "|";
        let input = format!("{header}\n{}", "|\n".repeat(3000));
        let blocks = parse(&input);
        let size: usize = blocks
            .iter()
            .map(|block| match block {
                Block::Table(text) => text.len(),
                _ => 0,
            })
            .sum();
        assert!(size < input.len() * 64, "{} input bytes rendered as {size} bytes", input.len());
    }

    #[test]
    fn inline_is_linear_on_unclosed_link_openers() {
        let text = "[a".repeat(60_000);
        let start = std::time::Instant::now();
        let spans = inline(&text);
        assert!(!spans.is_empty());
        assert!(start.elapsed() < std::time::Duration::from_millis(50), "inline took {:?}", start.elapsed());
    }
}
