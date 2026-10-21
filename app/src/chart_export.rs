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
    use std::io::Write;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let symbol: String = symbol.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    let stamp =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let path = directory.join(format!("{symbol}-{stamp}.{extension}"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    file.write_all(content.as_bytes()).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
    Ok(path)
}
#[cfg(test)]
mod tests {
    use super::*;
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
