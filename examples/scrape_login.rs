//! Checks `is_logged_in()` on a few pages with a saved session, then on the
//! login page without one (port of `samples/scrape_login.py`).
//!
//! cargo run --example scrape_login

use std::time::Duration;

use linkedin_scraper::{BrowserManager, BrowserOptions, WaitUntil, is_logged_in};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let session = "linkedin_session.json";
    if !std::path::Path::new(session).exists() {
        eprintln!("❌ {session} not found. Run: cargo run --example create_session");
        return Ok(());
    }

    let cases = [
        ("Feed page", "https://www.linkedin.com/feed/", true),
        (
            "Profile page",
            "https://www.linkedin.com/in/williamhgates/",
            true,
        ),
        (
            "Company page",
            "https://www.linkedin.com/company/microsoft/",
            true,
        ),
        (
            "My Network page",
            "https://www.linkedin.com/mynetwork/",
            true,
        ),
    ];

    let mut passed = 0;
    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    browser.load_session(session).await?;
    let page = browser.page()?;
    for (name, url, expected) in cases {
        page.goto(url, WaitUntil::DomContentLoaded, Duration::from_secs(60))
            .await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let actual = is_logged_in(&page).await;
        let ok = actual == expected;
        passed += usize::from(ok);
        println!(
            "{} {name}: expected {expected}, got {actual}",
            if ok { "✓" } else { "✗" }
        );
    }
    browser.close().await;

    // Negative test: fresh browser, no session.
    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    let page = browser.page()?;
    page.goto(
        "https://www.linkedin.com/login",
        WaitUntil::DomContentLoaded,
        Duration::from_secs(60),
    )
    .await?;
    let actual = is_logged_in(&page).await;
    passed += usize::from(!actual);
    println!(
        "{} Login page without session: expected false, got {actual}",
        if actual { "✗" } else { "✓" }
    );
    browser.close().await;

    println!("\n{passed}/{} passed", cases.len() + 1);
    Ok(())
}
