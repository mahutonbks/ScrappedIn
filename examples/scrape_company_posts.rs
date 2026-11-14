//! cargo run --example scrape_company_posts -- https://www.linkedin.com/company/microsoft/ 5

use linkedin_scraper::{BrowserManager, BrowserOptions, CompanyPostsScraper, ConsoleCallback};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .unwrap_or_else(|| "https://www.linkedin.com/company/microsoft/".into());
    let limit = args.next().and_then(|n| n.parse().ok()).unwrap_or(10);

    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    browser.load_session("linkedin_session.json").await?;

    let result = CompanyPostsScraper::new(browser.page()?)
        .with_callback(ConsoleCallback::new(false))
        .scrape(&url, limit)
        .await;
    browser.close().await;

    for post in result? {
        println!(
            "{post}\n  URL: {}\n",
            post.linkedin_url.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}
