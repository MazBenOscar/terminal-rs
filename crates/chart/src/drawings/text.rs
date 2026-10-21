//! Text and note geometry and untrusted-content sanitisation.
use super::{Geometry, LabeledShape, Shape, Tool};

pub fn sanitize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| *c == '\n' || !c.is_control()).take(500).collect()
}
pub fn deserialize_text<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    use serde::Deserialize;
    String::deserialize(de).map(|text| sanitize(&text))
}
pub fn editable(tool: Tool) -> bool {
    matches!(tool, Tool::Text | Tool::PriceNote | Tool::Callout)
}
pub fn label(d: &super::Drawing) -> String {
    if d.tool == Tool::PriceNote {
        if d.text.is_empty() { format!("{:.2}", d.a.price) } else { format!("{:.2} {}", d.a.price, sanitize(&d.text)) }
    } else {
        sanitize(&d.text)
    }
}
pub fn point_count(tool: Tool) -> usize {
    if tool == Tool::Callout { 2 } else { 1 }
}
pub fn handles(d: &super::Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count(d.tool))
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}
pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let a = g.points[0];
    if g.drawing.tool == Tool::FlagMark {
        let top = [a[0], a[1] - 22.];
        return vec![
            (Shape::Segment([a[0], a[1] - 5.], top), None),
            (Shape::Polygon([top, [a[0] + 12., top[1]], [a[0] + 9., top[1] + 7.], [a[0], top[1] + 7.]]), None),
        ];
    }
    let origin = match g.drawing.tool {
        Tool::Callout => g.points[1],
        Tool::PriceNote => [a[0] + 14., a[1] - g.text_size[1] - 8.],
        _ => a,
    };
    let mut shapes = vec![];
    if g.drawing.tool == Tool::Callout {
        let edge_x = if a[0] > origin[0] + g.text_size[0] / 2. { origin[0] + g.text_size[0] } else { origin[0] };
        let mid_y = origin[1] + g.text_size[1] / 2.;
        let half = (g.text_size[1] / 3.).min(5.);
        shapes.push((Shape::Polygon([a, [edge_x, mid_y - half], [edge_x, mid_y + half], a]), None));
    } else if g.drawing.tool == Tool::PriceNote {
        shapes.push((Shape::Segment(a, [origin[0], origin[1] + g.text_size[1] / 2.]), None));
    }
    shapes
        .push((Shape::Rect(origin, [origin[0] + g.text_size[0], origin[1] + g.text_size[1]]), Some(label(g.drawing))));
    shapes
}

#[cfg(test)]
mod tests {
    #[test]
    fn callout_text_sanitising_on_load() {
        let json = format!(
            r#"{{"tool":"Callout","a":{{"ts":1,"price":10}},"b":{{"ts":2,"price":20}},"text":{}}}"#,
            serde_json::to_string(&format!("Hi\u{0}\n\t{}", "é".repeat(600))).unwrap()
        );
        let note: super::super::Drawing = serde_json::from_str(&json).unwrap();
        assert_eq!(note.text.chars().count(), 500);
        assert!(note.text.starts_with("Hi\né"));
        assert!(!note.text.chars().any(|c| c.is_control() && c != '\n'));
    }

    #[test]
    fn sanitising_preserves_normalised_newlines_and_caps_characters() {
        let controls: String =
            (0..=0x9f).filter_map(char::from_u32).filter(|c| c.is_control() && !matches!(c, '\r' | '\n')).collect();
        assert_eq!(super::sanitize(&format!("a\r\nb\rc\nd{controls}é")), "a\nb\nc\ndé");

        let text = format!("{controls}é\r\n{}tail", "界".repeat(498));
        let sanitised = super::sanitize(&text);
        assert_eq!(sanitised, format!("é\n{}", "界".repeat(498)));
        assert_eq!(sanitised.chars().count(), 500);
    }
}
