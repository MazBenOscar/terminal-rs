//! Small, non-interactive markdown subset for agent replies.
use egui::{RichText, Ui};

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
    const MAX_COLUMNS: usize = 12;
    const MAX_WIDTH: usize = 40;
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = row.trim_matches('|').split('|').map(str::trim);
            let mut result: Vec<_> = cells
                .by_ref()
                .take(MAX_COLUMNS)
                .map(|cell| {
                    let mut chars = cell.chars();
                    let mut text: String = chars.by_ref().take(MAX_WIDTH).collect();
                    if chars.next().is_some() {
                        text.pop();
                        text.push('…');
                    }
                    text
                })
                .collect();
            if cells.next().is_some() {
                result[MAX_COLUMNS - 1] = "…".into();
            }
            result
        })
        .collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<_> = (0..columns)
        .map(|i| rows.iter().map(|row| row.get(i).map_or(0, |s| s.chars().count())).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|row| {
            widths
                .iter()
                .enumerate()
                .map(|(i, width)| {
                    let text = row.get(i).map(String::as_str).unwrap_or("");
                    format!("{text}{}", " ".repeat(width.saturating_sub(text.chars().count())))
                })
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Frame-ready blocks: block and inline parsing happens only when a message changes.
#[derive(Debug, Clone)]
pub enum DisplayBlock {
    Rich(Vec<Span>),
    Heading(u8, String),
    Monospace(String),
}
pub fn prepare(blocks: &[Block]) -> Vec<DisplayBlock> {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(text) => DisplayBlock::Rich(inline(text)),
            Block::Bullet(text) => DisplayBlock::Rich(inline(&format!("• {text}"))),
            Block::Numbered(number, text) => DisplayBlock::Rich(inline(&format!("{number}. {text}"))),
            Block::Heading(level, text) => DisplayBlock::Heading(*level, text.clone()),
            Block::Code(text) | Block::Table(text) => DisplayBlock::Monospace(text.clone()),
        })
        .collect()
}

pub fn show(ui: &mut Ui, blocks: &[DisplayBlock]) {
    for block in blocks {
        match block {
            DisplayBlock::Rich(spans) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for span in spans {
                        let text = match span {
                            Span::Text(text) => RichText::new(text),
                            Span::Bold(text) => RichText::new(text).strong(),
                            Span::Italic(text) => RichText::new(text).italics(),
                            Span::Code(text) => {
                                RichText::new(text).monospace().background_color(crate::theme::header())
                            }
                            Span::Link(label, url) => RichText::new(format!("{label} ({url})")),
                        };
                        ui.label(text);
                    }
                });
            }
            DisplayBlock::Heading(level, text) => {
                ui.label(RichText::new(text).strong().size(20.0 - f32::from(*level)));
            }
            DisplayBlock::Monospace(text) => {
                ui.label(RichText::new(text).monospace());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn unicode_tables_are_padded_as_monospace_grid() {
        assert_eq!(
            parse("| A | 値 |\n| --- | --- |\n| long | 2 |"),
            vec![Block::Table("A    | 値  \n---  | ---\nlong | 2  ".into())]
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
