//! Name → LinkedIn URL resolution from the command line.
//!
//! ```text
//! cargo run --example resolve_name -- company "Les Alchimistes" --domain alchimistes.co --location Toulon
//! cargo run --example resolve_name -- person "Jeanne Dupont" --company "Acme Compost" --title directrice
//! cargo run --example resolve_name -- compare-ids https://www.linkedin.com/company/acme/ …
//! ```
//!
//! Common options: `--headful` (visible browser, needed when Cloudflare
//! blocks headless Chrome), `--cache FILE` (default: no cache), `--refresh`.
//!
//! `compare-ids` opens each company page once and prints both id clues (the
//! top-card "employees" link and the most frequent company URN) to measure
//! how often they agree.

use linkedin_scraper::resolve::{CompanyQuery, CompanyRef, PersonQuery, ResolveCache};
use linkedin_scraper::{BrowserManager, BrowserOptions, CompanySearch, ResolveConfig, Resolver};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn positional(args: &[String]) -> Vec<String> {
    let with_value = [
        "--domain",
        "--location",
        "--industry",
        "--company",
        "--title",
        "--cache",
    ];
    args.iter()
        .enumerate()
        .filter(|(i, a)| {
            !a.starts_with("--") && !(*i > 0 && with_value.contains(&args[i - 1].as_str()))
        })
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pos = positional(&args);
    let (Some(cmd), rest) = (
        pos.first().cloned(),
        pos.get(1..).unwrap_or_default().to_vec(),
    ) else {
        eprintln!("usage: resolve_name company|person|compare-ids …  (see the file header)");
        return Ok(());
    };
    let headful = args.iter().any(|a| a == "--headful");

    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(!headful)).await?;
    browser.load_session("linkedin_session.json").await?;
    let page = browser.page()?;

    let cache = match flag(&args, "--cache") {
        Some(p) => ResolveCache::load(p)?,
        None => ResolveCache::in_memory(),
    };
    let mut resolver = Resolver::new(
        page.clone(),
        ResolveConfig {
            pause_secs: Some((3.0, 7.0)),
            ..ResolveConfig::default()
        },
    )
    .with_cache(cache)
    .with_refresh(args.iter().any(|a| a == "--refresh"));

    let company_query = |name: String| CompanyQuery {
        name,
        domain: flag(&args, "--domain"),
        location: flag(&args, "--location"),
        industry: flag(&args, "--industry"),
    };

    let result = match cmd.as_str() {
        "company" => {
            let q = company_query(rest.join(" "));
            let r = resolver.resolve_company(&q).await?;
            serde_json::to_value(&r)?
        }
        "person" => {
            let company = flag(&args, "--company").map(|c| {
                if c.contains("linkedin.com/company/") {
                    CompanyRef::Url(c)
                } else {
                    CompanyRef::Name(CompanyQuery::new(c))
                }
            });
            let q = PersonQuery {
                name: rest.join(" "),
                company,
                title: flag(&args, "--title"),
                location: flag(&args, "--location"),
            };
            let r = resolver.resolve_person(&q).await?;
            serde_json::to_value(&r)?
        }
        "compare-ids" => {
            let search = CompanySearch::new(page);
            let mut rows = Vec::new();
            let (mut agree, mut header_only, mut differ) = (0, 0, 0);
            for (i, url) in rest.iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                }
                let ev = search.company_id_evidence(url).await?;
                let majority = ev.urn_majority();
                match (ev.header, majority) {
                    (Some(h), Some(m)) if h == m => agree += 1,
                    (Some(_), None) => header_only += 1,
                    _ => differ += 1,
                }
                let mut top: Vec<_> = ev.urn_counts.iter().collect();
                top.sort_by(|a, b| b.1.cmp(a.1));
                println!(
                    "{url}\n   header link: {:?} · URN majority: {:?} · top URNs: {:?}",
                    ev.header,
                    majority,
                    top.iter().take(3).collect::<Vec<_>>()
                );
                rows.push(
                    serde_json::json!({"url": url, "header": ev.header, "urn_majority": majority}),
                );
            }
            println!("\n{agree} agree · {header_only} header only · {differ} differ or missing");
            serde_json::json!(rows)
        }
        other => {
            eprintln!("unknown command {other}");
            serde_json::Value::Null
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    eprintln!("pages opened by the resolver: {}", resolver.pages_loaded());
    browser.close().await;
    Ok(())
}
