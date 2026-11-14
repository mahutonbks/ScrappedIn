//! Searches jobs, then scrapes the first results.
//!
//! cargo run --example scrape_jobs -- "rust developer" "Marseille" 3

use linkedin_scraper::{BrowserManager, BrowserOptions, JobScraper, JobSearchScraper};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let mut args = std::env::args().skip(1);
    let keywords = args.next().unwrap_or_else(|| "software engineer".into());
    let location = args.next().unwrap_or_else(|| "San Francisco".into());
    let limit = args.next().and_then(|n| n.parse().ok()).unwrap_or(3);

    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    browser.load_session("linkedin_session.json").await?;
    let page = browser.page()?;

    let urls = JobSearchScraper::new(page.clone())
        .search(Some(&keywords), Some(&location), limit)
        .await?;
    println!("Found {} jobs", urls.len());

    let scraper = JobScraper::new(page);
    for url in urls {
        match scraper.scrape(&url).await {
            Ok(job) => println!("\n{job}"),
            Err(e) => eprintln!("✗ {url}: {e}"),
        }
    }

    browser.close().await;
    Ok(())
}
