//! Opens a visible browser, waits for a manual login (up to 5 minutes, 2FA and
//! CAPTCHA included) and saves the session to `linkedin_session.json`.
//!
//! cargo run --example create_session

use std::time::Duration;

use linkedin_scraper::{BrowserManager, BrowserOptions, WaitUntil, wait_for_manual_login};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(false)).await?;
    let page = browser.page()?;

    println!("Opening LinkedIn login page...");
    page.goto(
        "https://www.linkedin.com/login",
        WaitUntil::DomContentLoaded,
        Duration::from_secs(60),
    )
    .await?;

    println!("🔐 Please log in to LinkedIn in the browser window (5 minutes max)...");
    if let Err(e) = wait_for_manual_login(&page, Duration::from_secs(300)).await {
        eprintln!("❌ Login failed: {e}");
        browser.close().await;
        return Ok(());
    }

    let session_path = "linkedin_session.json";
    browser.save_session(session_path).await?;
    println!("✅ Session saved to {session_path} — keep it secret, never commit it.");

    browser.close().await;
    Ok(())
}
