use std::fs::{File, OpenOptions};
use std::io::BufReader;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.len() != 2 {
        bail!("usage: scrub_transcript <raw.jsonl> <new-scrubbed.jsonl>");
    }
    let input = File::open(&paths[0]).context("open raw transcript")?;
    let output =
        OpenOptions::new().write(true).create_new(true).open(&paths[1]).context("create scrubbed transcript")?;
    tr_agent::transcript::scrub_jsonl(BufReader::new(input), output)
}
