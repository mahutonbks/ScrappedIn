//! cargo run --example scrape_person -- https://www.linkedin.com/in/williamhgates/

use linkedin_scraper::{BrowserManager, BrowserOptions, ConsoleCallback, PersonScraper};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://www.linkedin.com/in/williamhgates/".into());

    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    browser.load_session("linkedin_session.json").await?;
    println!("✓ Session loaded");

    let result = PersonScraper::new(browser.page()?)
        .with_callback(ConsoleCallback::default())
        .scrape(&url)
        .await;
    browser.close().await;

    let person = result?;
    println!("\n{person}\n");
    println!("{}", person.to_json_pretty()?);
    Ok(())
}
