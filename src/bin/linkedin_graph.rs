//! Builds a knowledge graph from `linkedin_batch` JSONL output(s).
//!
//! ```text
//! cargo run --release --bin linkedin_graph -- results.jsonl -o graph.cypher
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::ExitCode;

use linkedin_scraper::KnowledgeGraph;

const HELP: &str = "\
linkedin_graph — build a knowledge graph from linkedin_batch JSONL results

USAGE:
    linkedin_graph <RESULTS.jsonl>... [-o FILE] [--format json|jsonld|cypher]

Profiles and companies from all files are merged: the same person, company or
school is one node. Only \"ok\" records are used.

OPTIONS:
    -o, --output <FILE>   Output file (default: stdout)
    -f, --format <FMT>    json (nodes/edges), jsonld (schema.org) or cypher (Neo4j).
                          Default: from the output extension, else json.
    -h, --help            Show this help

NEO4J:  cypher-shell -u neo4j -p <password> -f graph.cypher
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut inputs, mut output, mut format) = (Vec::new(), None::<String>, None::<String>);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-o" | "--output" => output = it.next().cloned(),
            "-f" | "--format" => format = it.next().cloned(),
            s if s.starts_with('-') => {
                eprintln!("error: unknown option {s}\n\n{HELP}");
                return ExitCode::from(1);
            }
            s => inputs.push(s.to_string()),
        }
    }
    if inputs.is_empty() {
        eprintln!("error: no input file\n\n{HELP}");
        return ExitCode::from(1);
    }
    let format = format.unwrap_or_else(|| match output.as_deref() {
        Some(o) if o.ends_with(".cypher") || o.ends_with(".cql") => "cypher".into(),
        Some(o) if o.ends_with(".jsonld") => "jsonld".into(),
        _ => "json".into(),
    });

    let mut g = KnowledgeGraph::new();
    let (mut used, mut skipped) = (0usize, 0usize);
    for path in &inputs {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("error: cannot open {path}: {e}");
                return ExitCode::from(1);
            }
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            match serde_json::from_str(&line) {
                Ok(v) if g.add_record(&v) => used += 1,
                _ => skipped += 1,
            }
        }
    }

    let content = match format.as_str() {
        "cypher" => g.to_cypher(),
        "jsonld" => serde_json::to_string_pretty(&g.to_json_ld()).unwrap_or_default(),
        "json" => serde_json::to_string_pretty(&g.to_json()).unwrap_or_default(),
        other => {
            eprintln!("error: unknown format {other} (json, jsonld, cypher)");
            return ExitCode::from(1);
        }
    };
    match &output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, content) {
                eprintln!("error: cannot write {path}: {e}");
                return ExitCode::from(1);
            }
        }
        None => print!("{content}"),
    }
    eprintln!(
        "🕸  {used} records used, {skipped} skipped · {} nodes, {} relations{}",
        g.nodes().count(),
        g.edges().len(),
        output
            .map(|o| format!(" · {o} ({format})"))
            .unwrap_or_default()
    );
    ExitCode::SUCCESS
}
