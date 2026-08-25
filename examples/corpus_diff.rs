//! Differential regression harness for parser changes, over a corpus of
//! real traffic rendered as JSON Lines.
//!
//! Relaxing a parser to accept more of what is on the air is easy to get
//! wrong in a way the test suite cannot catch. A packet that used to be
//! rejected may start decoding to the wrong values, and a packet that
//! already worked may start decoding differently. Neither is a test
//! failure, because no test knew about those packets.
//!
//! This makes the change measurable. Render the corpus before and after,
//! then compare:
//!
//! ```sh
//! # Any file of TNC2 monitor lines: an APRS-IS capture, a TNC log.
//! yodel decode --tnc2 --output-format jsonl corpus.txt > before.jsonl
//! # ... change the parser, rebuild ...
//! yodel decode --tnc2 --output-format jsonl corpus.txt > after.jsonl
//!
//! cargo run --release --example corpus_diff --features std,aprs -- \
//!     before.jsonl after.jsonl
//! ```
//!
//! Using the CLI's own JSON Lines output rather than a bespoke format
//! means the comparison sees exactly the fields the documented schema
//! promises, and the same files work with `jq` for anything this tool
//! does not report.
//!
//! # What it reports
//!
//! Every packet lands in one of five buckets, and only the first is
//! unambiguously good:
//!
//! * **RECOVERED**: was `malformed`, now decodes. The point of a
//!   relaxation.
//! * **REGRESSED**: decoded before, now `malformed`. Always a defect.
//! * **RETYPED**: decodes as a different kind. Usually a defect.
//! * **VALUE CHANGED**: same kind, different fields. The dangerous one,
//!   because the packet still looks fine. A relaxation should almost
//!   never change a packet that already worked; if it does, one of the
//!   two readings is wrong.
//! * **unchanged**: everything else.
//!
//! The process exits non-zero if anything but RECOVERED moved, so it can
//! gate a change.
//!
//! # Recovering a packet is not the same as reading it correctly
//!
//! A relaxation that accepts a packet and then misreads it looks
//! identical to one that reads it properly: both show up as RECOVERED.
//! The control for that is the schema's own `info` field, which carries
//! the bytes as received. Any recovered packet whose `info` changed is
//! reported separately, because the decoder should never rewrite the
//! bytes it was given.
//!
//! For the stronger check, that a recovered packet *rebuilds* to what
//! arrived, compare a `gen`-and-`decode` round trip; byte-exactness of
//! the rebuild is tracked in `docs/APRS_CONFORMANCE.md`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 2 {
        eprintln!("usage: corpus_diff <before.jsonl> <after.jsonl>");
        std::process::exit(2);
    }
    let before = load(&a[0])?;
    let after = load(&a[1])?;

    let mut recovered: BTreeMap<String, u64> = BTreeMap::new();
    let mut rewrote_info: Vec<u64> = Vec::new();
    let mut misread: Vec<u64> = Vec::new();
    let mut regressed: Vec<(u64, String)> = Vec::new();
    let mut retyped: BTreeMap<String, u64> = BTreeMap::new();
    let mut changed: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut unchanged = 0u64;

    for (n, old) in &before {
        let Some(new) = after.get(n) else { continue };
        if old.line == new.line {
            unchanged += 1;
            continue;
        }
        match (old.kind.as_str(), new.kind.as_str()) {
            ("malformed", "malformed") => unchanged += 1,
            ("malformed", k) => {
                *recovered.entry(k.to_string()).or_default() += 1;
                if old.info != new.info {
                    rewrote_info.push(*n);
                }
                // With --verify-rebuild, a recovery that does not
                // rebuild to the received bytes is a misreading rather
                // than a fix. Absent the flag there is nothing to check.
                if matches!(new.rebuild.as_str(), "differs" | "failed") {
                    misread.push(*n);
                }
            }
            (k, "malformed") => regressed.push((*n, k.to_string())),
            (o, k) if o != k => *retyped.entry(format!("{o} -> {k}")).or_default() += 1,
            (k, _) => changed.entry(k.to_string()).or_default().push(*n),
        }
    }

    println!("corpus of {} packets\n", before.len());

    let rec: u64 = recovered.values().sum();
    println!("RECOVERED      {rec:>6}   was malformed, now decodes");
    for (k, c) in &recovered {
        println!("                        {c:>6}  as {k}");
    }
    if !rewrote_info.is_empty() {
        println!(
            "\n  WARNING: {} recovered packets have a different `info` than before.",
            rewrote_info.len()
        );
        println!("  `info` is the bytes as received and must never change. Lines:");
        println!("    {:?}", &rewrote_info[..rewrote_info.len().min(10)]);
    }

    if !misread.is_empty() {
        println!(
            "\n  MISREAD: {} of those do not rebuild to the bytes that arrived.",
            misread.len()
        );
        println!("  The packet is accepted but read differently from how it was sent,");
        println!("  which is worse than rejecting it. Lines:");
        println!("    {:?}", &misread[..misread.len().min(10)]);
    }

    println!(
        "\nREGRESSED      {:>6}   decoded before, now malformed",
        regressed.len()
    );
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    for (_, k) in &regressed {
        *by_kind.entry(k.clone()).or_default() += 1;
    }
    for (k, c) in &by_kind {
        println!("                        {c:>6}  was {k}");
    }
    for (n, _) in regressed.iter().take(6) {
        println!("      line {n}");
        println!("        before  {}", trunc(&before[n].line, 96));
        println!("        after   {}", trunc(&after[n].line, 96));
    }

    let ret: u64 = retyped.values().sum();
    println!("\nRETYPED        {ret:>6}   decodes as a different kind");
    for (p, c) in &retyped {
        println!("                        {c:>6}  {p}");
    }

    let chg: usize = changed.values().map(Vec::len).sum();
    println!("\nVALUE CHANGED  {chg:>6}   same kind, different fields");
    for (k, v) in &changed {
        println!("                        {:>6}  {k}", v.len());
    }
    for v in changed.values() {
        for n in v.iter().take(4) {
            println!("      line {n}");
            println!("        before  {}", trunc(&before[n].line, 96));
            println!("        after   {}", trunc(&after[n].line, 96));
        }
    }

    println!("\nunchanged      {unchanged:>6}");

    let bad = regressed.len() as u64
        + ret
        + chg as u64
        + rewrote_info.len() as u64
        + misread.len() as u64;
    println!("\nnet: +{rec} recovered, {bad} needing review");
    if bad > 0 {
        println!("REVIEW REQUIRED: a relaxation should recover packets, not alter existing ones.");
        std::process::exit(1);
    }
    Ok(())
}

struct Row {
    kind: String,
    info: String,
    rebuild: String,
    line: String,
}

/// Reads JSON Lines, pulling out just the three fields this tool keys
/// on. A dependency-free scan is enough: the writer emits one object per
/// line with `"` escaped, so finding a top-level key is a substring
/// search that cannot run past the line.
fn load(path: &str) -> Result<BTreeMap<u64, Row>, Box<dyn std::error::Error>> {
    let mut map = BTreeMap::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Some(n) = field(&line, "\"n\":").and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        let kind = string_field(&line, "\"kind\":\"").unwrap_or_default();
        let info = string_field(&line, "\"info\":\"").unwrap_or_default();
        let rebuild = string_field(&line, "\"rebuild\":\"").unwrap_or_default();
        map.insert(
            n,
            Row {
                kind,
                info,
                rebuild,
                line,
            },
        );
    }
    Ok(map)
}

fn field(line: &str, key: &str) -> Option<String> {
    let i = line.find(key)? + key.len();
    let rest = &line[i..];
    let end = rest.find([',', '}'])?;
    Some(rest[..end].to_string())
}

fn string_field(line: &str, key: &str) -> Option<String> {
    let i = line.find(key)? + key.len();
    let rest = &line[i..];
    // Find the closing quote, honouring backslash escapes.
    let mut esc = false;
    for (j, c) in rest.char_indices() {
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(rest[..j].to_string());
        }
    }
    None
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
