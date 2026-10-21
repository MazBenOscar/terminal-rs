//! Bounded exports of displayed OHLCV data. SVG is a portable price-chart image,
//! deliberately labeled separately from a screenshot of every UI pane.
use std::fmt::Write;
use tr_core::Bar;

pub fn csv(bars: &[Bar]) -> String {
    let mut text = String::from("timestamp_utc,open,high,low,close,volume\n");
    for b in bars {
        let _ = writeln!(text, "{},{},{},{},{},{}", b.ts, b.open, b.high, b.low, b.close, b.volume);
    }
    text
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
pub fn svg(title: &str, bars: &[Bar]) -> String {
    let mut text = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="700" viewBox="0 0 1200 700"><rect width="1200" height="700" fill="#10151c"/><text x="24" y="28" fill="#e4e8ee" font-family="monospace" font-size="16">{}</text><text x="24" y="51" fill="#98a4b3" font-family="monospace" font-size="12">OHLC price chart export · UTC timestamps · studies/drawings/UI panes excluded</text>"##,
        escape(title)
    );
    let lo = bars.iter().map(|b| b.low).filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
    let hi = bars.iter().map(|b| b.high).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1e-10);
    if !lo.is_finite() || !hi.is_finite() {
        text.push_str("</svg>");
        return text;
    }
    let y = |p: f64| 610. - 520. * (p - lo) / span;
    for i in 0..=5 {
        let price = lo + span * i as f64 / 5.;
        let _ = write!(
            text,
            r##"<path d="M24 {}H1080" stroke="#29313b"/><text x="1090" y="{}" fill="#98a4b3" font-family="monospace" font-size="12">{:.4}</text>"##,
            y(price),
            y(price),
            price
        );
    }
    let width = 1050. / bars.len().max(1) as f64;
    for (i, b) in bars.iter().enumerate() {
        if ![b.open, b.high, b.low, b.close].iter().all(|n| n.is_finite()) {
            continue;
        }
        let x = 24. + (i as f64 + 0.5) * width;
        let color = if b.close >= b.open { "#4fc49e" } else { "#e0717b" };
        let _ = write!(
            text,
            r##"<path d="M{x} {}V{}" stroke="{color}"/><rect x="{}" y="{}" width="{}" height="{}" fill="{color}"/>"##,
            y(b.high),
            y(b.low),
            x - width * 0.3,
            y(b.open.max(b.close)),
            (width * 0.6).max(0.3),
            (y(b.open) - y(b.close)).abs().max(1.)
        );
    }
    if let (Some(a), Some(b)) = (bars.first(), bars.last()) {
        let _ = write!(
            text,
            r##"<text x="24" y="654" fill="#98a4b3" font-family="monospace" font-size="12">{} to {} · {} displayed bars</text>"##,
            a.ts,
            b.ts,
            bars.len()
        );
    }
    text.push_str("</svg>");
    text
}
pub fn save(
    directory: &std::path::Path,
    symbol: &str,
    content: &str,
    extension: &str,
) -> Result<std::path::PathBuf, String> {
    save_bytes(directory, symbol, content.as_bytes(), extension)
}

pub fn save_bytes(
    directory: &std::path::Path,
    symbol: &str,
    bytes: &[u8],
    extension: &str,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let symbol: String = symbol.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    let stamp =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let path = directory.join(format!("{symbol}-{stamp}.{extension}"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
    Ok(path)
}
/// Encodes a screenshot crop, clamping its bounds to the source image.
pub fn crop_png(image: &egui::ColorImage, rect_px: [usize; 4]) -> Result<Vec<u8>, String> {
    let [x, y, width, height] = rect_px;
    let right = x.saturating_add(width).min(image.width());
    let bottom = y.saturating_add(height).min(image.height());
    let x = x.min(image.width());
    let y = y.min(image.height());
    let (width, height) = (right - x, bottom - y);
    if width == 0 || height == 0 {
        return Err("The chart crop is empty".into());
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in y..bottom {
        for pixel in &image.pixels[row * image.width() + x..row * image.width() + right] {
            rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crop_png_encodes_rgba_and_clamps_bounds() {
        let image = egui::ColorImage::filled([4, 3], egui::Color32::from_rgb(12, 34, 56));
        for (rect, size) in [([1, 1, 2, 1], [2, 1]), ([2, 1, usize::MAX, usize::MAX], [2, 2])] {
            let bytes = crop_png(&image, rect).unwrap();
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(&bytes[12..16], b"IHDR");
            assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), size[0]);
            assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), size[1]);
            let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut pixels).unwrap();
            assert_eq!(info.color_type, png::ColorType::Rgba);
            assert_eq!(&pixels[..4], &[12, 34, 56, 255]);
        }
    }

    #[test]
    fn crop_png_rejects_empty_crops() {
        let image = egui::ColorImage::filled([4, 3], egui::Color32::BLACK);
        for rect in [[0, 0, 0, 1], [0, 0, 1, 0], [4, 0, 1, 1], [0, usize::MAX, 1, 1]] {
            assert!(crop_png(&image, rect).is_err());
        }
    }

    #[test]
    fn save_bytes_sanitises_names_and_preserves_existing_exports() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target").join(format!("export-test-{stamp}"));
        let first = save_bytes(&directory, "../../A/B:C-123", &[0, 255, 42], "png").unwrap();
        let second = save_bytes(&directory, "../../A/B:C-123", &[1, 2], "png").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(directory.as_path()));
        assert!(first.file_name().unwrap().to_str().unwrap().starts_with("ABC-123-"));
        assert_eq!(first.extension().unwrap(), "png");
        assert_eq!(std::fs::read(&first).unwrap(), [0, 255, 42]);
        assert_eq!(std::fs::read(second).unwrap(), [1, 2]);
        assert!(std::fs::OpenOptions::new().write(true).create_new(true).open(first).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exports_preserve_values_and_escape_labels() {
        let b = [Bar::new(123, 1., 3., 0.5, 2., 123.45)];
        assert!(csv(&b).contains("123,1,3,0.5,2,123.45\n"));
        let image = svg("A<B & C", &b);
        assert!(image.contains("A&lt;B &amp; C"));
        assert!(image.contains("<rect x="));
        assert!(!image.contains("NaN"));
    }
}
