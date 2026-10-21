//! cargo run -p tr-core --example chart_parity -- path/to/reference.json
fn main() {
    let result = (|| -> Result<_, String> {
        let path = std::env::args().nth(1).ok_or("Usage: chart_parity reference.json")?;
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut fixture: tr_core::indicators::parity::Fixture =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        tr_core::indicators::parity::compare(&mut fixture)
    })();
    match result {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if report.mismatches > 0 {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
