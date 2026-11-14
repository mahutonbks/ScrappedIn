//! cargo run --example scrape_company -- <company URL> [--headful] [--employees N] [--graph FILE]
//!
//! * `--headful`: show the browser; if LinkedIn displays a security check
//!   (Cloudflare / CAPTCHA) you get up to 3 minutes to solve it by hand.
//! * `--employees N`: also collect up to N employees ("People" tab), sorted by
//!   seniority.
//! * `--graph FILE`: write a knowledge graph; the format follows the extension
//!   (`.json` nodes/edges, `.jsonld` schema.org, `.cypher` Neo4j).

use std::time::{Duration, Instant};

use linkedin_scraper::core::utils::detect_rate_limit;
use linkedin_scraper::{
    BrowserManager, BrowserOptions, CompanyScraper, ConsoleCallback, KnowledgeGraph, WaitUntil,
};

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headful = args.iter().any(|a| a == "--headful");
    let employees: usize = flag_value(&args, "--employees")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let graph_file = flag_value(&args, "--graph");
    let url = args
        .iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with("--")
                && !(*i > 0 && matches!(args[i - 1].as_str(), "--employees" | "--graph"))
        })
        .map_or_else(
            || "https://www.linkedin.com/company/microsoft/".into(),
            |(_, a)| a.clone(),
        );

    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(!headful)).await?;
    browser.load_session("linkedin_session.json").await?;

    if headful {
        let page = browser.page()?;
        page.goto(&url, WaitUntil::DomContentLoaded, Duration::from_secs(60))
            .await?;
        let start = Instant::now();
        let mut warned = false;
        while let Err(e) = detect_rate_limit(&page).await {
            if start.elapsed() > Duration::from_secs(180) {
                browser.close().await;
                return Err(e);
            }
            if !warned {
                println!("🧩 Security check shown: solve it in the browser window (3 min max)...");
                warned = true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    let result = CompanyScraper::new(browser.page()?)
        .with_callback(ConsoleCallback::default())
        .with_employees(employees)
        .scrape(&url)
        .await;
    browser.close().await;

    let company = result?;
    println!("\n{company}\n\n{}", company.to_json_pretty()?);

    if !company.employees.is_empty() {
        println!("\nEmployees by seniority:");
        for e in &company.employees {
            println!(
                "  [{} {:<14}] {:<28} {}",
                e.seniority_rank,
                e.seniority
                    .map_or("unknown", linkedin_scraper::Seniority::as_str),
                e.name,
                e.designation.as_deref().unwrap_or("")
            );
        }
    }

    if let Some(path) = graph_file {
        let mut g = KnowledgeGraph::new();
        g.add_company(&company);
        let content = if path.ends_with(".cypher") || path.ends_with(".cql") {
            g.to_cypher()
        } else if path.ends_with(".jsonld") {
            serde_json::to_string_pretty(&g.to_json_ld())?
        } else {
            serde_json::to_string_pretty(&g.to_json())?
        };
        std::fs::write(&path, content)
            .map_err(|e| linkedin_scraper::Error::Runtime(e.to_string()))?;
        println!("\n🕸  Knowledge graph written to {path}");
    }
    Ok(())
}
