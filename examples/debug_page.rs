//! Diagnostic: shows what the browser actually sees on a URL.
//! cargo run --example debug_page -- <url> [out.html]
use linkedin_scraper::{BrowserManager, BrowserOptions, WaitUntil};
use std::time::Duration;

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headful = args.iter().any(|a| a == "--headful");
    let click = args
        .iter()
        .position(|a| a == "--click")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let mut pos = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && !(*i > 0 && args[i - 1] == "--click"))
        .map(|(_, a)| a);
    let url = pos.next().expect("url").clone();
    let out = pos.next().cloned();
    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(!headful)).await?;
    browser.load_session("linkedin_session.json").await?;
    let page = browser.page()?;
    page.goto(&url, WaitUntil::DomContentLoaded, Duration::from_secs(60))
        .await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for _ in 0..6 {
        page.run_js("window.scrollBy(0, window.innerHeight)")
            .await?;
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    if let Some(sel) = &click {
        page.run_js("window.scrollTo(0, 0)").await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        page.locator(sel).first().click().await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let dialog: String = page
            .evaluate("[...document.querySelectorAll('dialog[open], [role=dialog]')].map(d => d.innerText).join(' ===== ')")
            .await?;
        println!(
            "DIALOG:
{dialog}"
        );
    }
    let info: serde_json::Value = page.evaluate(r"(() => ({
        url: location.href, title: document.title,
        h1: [...document.querySelectorAll('h1')].map(e => e.innerText.trim()),
        h2: [...document.querySelectorAll('h2')].map(e => e.innerText.trim()).slice(0, 30),
        main: !!document.querySelector('main'),
        iframes: [...document.querySelectorAll('iframe')].map(f => ({ title: f.title, src: (f.src || '').slice(0, 150), visible: f.offsetWidth > 0 && f.offsetHeight > 0 })),
        body: (document.querySelector('main')?.innerText || document.body?.innerText || '').slice(0, 600)
    }))()").await?;
    println!("{}", serde_json::to_string_pretty(&info).unwrap());
    if let Some(out) = out {
        let html: String = page.evaluate("document.documentElement.outerHTML").await?;
        std::fs::write(out, html).ok();
    }
    browser.close().await;
    Ok(())
}
