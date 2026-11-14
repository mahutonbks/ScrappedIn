//! Logs in with LINKEDIN_EMAIL / LINKEDIN_PASSWORD (from the environment or a
//! `.env` file) and saves the session.
//!
//! cargo run --example login_credentials

use std::time::Duration;

use linkedin_scraper::{BrowserManager, BrowserOptions, login_with_credentials};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(false)).await?;
    let page = browser.page()?;

    let result = login_with_credentials(&page, None, None, Duration::from_secs(30), true).await;
    match result {
        Ok(()) => {
            browser.save_session("linkedin_session.json").await?;
            println!("✅ Logged in, session saved to linkedin_session.json");
        }
        Err(e) => eprintln!("❌ {e}"),
    }
    browser.close().await;
    Ok(())
}
