//! Scraping helpers (port of `core/utils.py`).

use std::future::Future;
use std::time::Duration;

use tracing::{debug, error, warn};

use super::page::{ElementState, Page};
use crate::error::{Error, Result};

/// Pseudo-random number in `[min, max]` (std only), for human-like pauses.
pub fn jitter(min: f64, max: f64) -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    let unit = (h.finish() >> 11) as f64 / (1u64 << 53) as f64;
    min + (max - min) * unit
}

/// Retries `f` with exponential backoff (`backoff^attempt` seconds) while
/// `should_retry(&err)` is true. Port of the `@retry_async` decorator.
pub async fn retry_async<T, F, Fut>(
    max_attempts: u32,
    backoff: f64,
    should_retry: impl Fn(&Error) -> bool,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let max_attempts = max_attempts.max(1);
    let mut attempt = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if should_retry(&e) && attempt + 1 < max_attempts => {
                let wait = backoff.powi(attempt as i32);
                warn!(
                    "Attempt {}/{max_attempts} failed: {e}. Retrying in {wait}s...",
                    attempt + 1
                );
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                attempt += 1;
            }
            Err(e) => {
                if should_retry(&e) {
                    error!("All {max_attempts} attempts failed");
                }
                return Err(e);
            }
        }
    }
}

/// Returns [`Error::RateLimit`] if LinkedIn shows a checkpoint, a CAPTCHA or a
/// rate-limit message.
///
/// Note: in the Python version the CAPTCHA `RateLimitError` is raised inside a
/// `try/except Exception: pass` and therefore never propagates; it is
/// propagated here, which is what the code clearly intended.
pub async fn detect_rate_limit(page: &Page) -> Result<()> {
    wait_for_bot_challenge(page, Duration::from_secs(20)).await?;
    let current_url = page.url().await;
    if current_url.contains("linkedin.com/checkpoint") || current_url.contains("authwall") {
        return Err(Error::RateLimit {
            message: "LinkedIn security checkpoint detected. \
                      You may need to verify your identity or wait before continuing."
                .into(),
            suggested_wait_time: 3600,
        });
    }

    // LinkedIn embeds an *invisible* reCAPTCHA Enterprise iframe on normal
    // pages (background scoring): only a visible CAPTCHA blocks the user.
    let captcha = page
        .evaluate::<bool>(
            r#"[...document.querySelectorAll('iframe[title*="captcha" i], iframe[src*="captcha" i]')]
                .some(f => {
                    const r = f.getBoundingClientRect();
                    const st = getComputedStyle(f);
                    return r.width > 30 && r.height > 30
                        && st.visibility !== 'hidden' && st.display !== 'none' && st.opacity !== '0';
                })"#,
        )
        .await
        .unwrap_or(false);
    if captcha {
        return Err(Error::RateLimit {
            message: "CAPTCHA challenge detected. Manual intervention required.".into(),
            suggested_wait_time: 3600,
        });
    }

    match page
        .locator("body")
        .timeout(Duration::from_millis(1000))
        .text_content()
        .await
    {
        Ok(Some(body)) => {
            let body = body.to_lowercase();
            if [
                "too many requests",
                "rate limit",
                "slow down",
                "try again later",
            ]
            .iter()
            .any(|p| body.contains(p))
            {
                return Err(Error::RateLimit {
                    message: "Rate limit message detected on page.".into(),
                    suggested_wait_time: 1800,
                });
            }
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) if e.is_timeout() => Ok(()),
        Err(e) => Err(e),
    }
}

/// `true` while a Cloudflare "Just a moment..." / security verification page is shown.
pub async fn is_bot_challenge(page: &Page) -> bool {
    page.evaluate::<bool>(
        r#"(() => {
            const t = (document.title || '').toLowerCase();
            const b = (document.body?.innerText || '').slice(0, 2000).toLowerCase();
            return t.includes('just a moment') || t.includes('un instant')
                || b.includes('performing security verification')
                || b.includes('verify you are human')
                || !!document.querySelector('#challenge-form, #cf-challenge-running, iframe[src*="challenges.cloudflare.com"]');
        })()"#,
    )
    .await
    .unwrap_or(false)
}

/// Waits up to `limit` for a Cloudflare challenge to clear by itself.
/// Returns [`Error::RateLimit`] if it is still there afterwards.
pub async fn wait_for_bot_challenge(page: &Page, limit: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while is_bot_challenge(page).await {
        if start.elapsed() >= limit {
            return Err(Error::RateLimit {
                message: "Cloudflare bot verification (\"Just a moment...\") is blocking the browser. Wait and retry, or use a visible browser (linkedin_batch --headful, or BrowserOptions::default().headless(false))."
                    .into(),
                suggested_wait_time: 600,
            });
        }
        debug!("Cloudflare challenge detected, waiting...");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

/// Waits for an element and returns a helpful [`Error::ElementNotFound`] on timeout.
pub async fn wait_for_element_smart(
    page: &Page,
    selector: &str,
    timeout: Duration,
    state: ElementState,
    error_context: Option<&str>,
) -> Result<()> {
    match page.wait_for_selector(selector, timeout, state).await {
        Err(e) if e.is_timeout() => {
            let context = error_context
                .map(|c| format!(" when {c}"))
                .unwrap_or_default();
            Err(Error::ElementNotFound(format!(
                "Could not find element with selector '{selector}'{context}. \
                 This may indicate:\n  • The page structure has changed\n  \
                 • The profile has restricted visibility\n  \
                 • The content doesn't exist on this page\n  \
                 • Network slowness (try increasing timeout)\n{}",
                selector_suggestions(selector)
            )))
        }
        other => other,
    }
}

fn selector_suggestions(selector: &str) -> &'static str {
    if selector.contains('#') {
        "Tip: ID selectors may be dynamic. Consider using data attributes or text content."
    } else if selector.contains("pv-") || selector.contains("artdeco") {
        "Tip: LinkedIn class names change frequently. This selector may need updating."
    } else {
        ""
    }
}

/// Text of the first match, trimmed, or `default` if absent/empty.
pub async fn extract_text_safe(
    page: &Page,
    selector: &str,
    default: &str,
    timeout: Duration,
) -> String {
    match page
        .locator(selector)
        .first()
        .timeout(timeout)
        .text_content()
        .await
    {
        Ok(Some(t)) if !t.trim().is_empty() => t.trim().to_string(),
        Ok(_) => default.to_string(),
        Err(e) => {
            debug!("Element not found: {selector} ({e}), returning default: {default}");
            default.to_string()
        }
    }
}

/// Scrolls to the bottom until the page height stops growing.
pub async fn scroll_to_bottom(page: &Page, pause_time: f64, max_scrolls: u32) -> Result<()> {
    for i in 0..max_scrolls {
        let previous: f64 = page.evaluate("document.body.scrollHeight").await?;
        page.run_js("window.scrollTo(0, document.body.scrollHeight)")
            .await?;
        tokio::time::sleep(Duration::from_secs_f64(pause_time)).await;
        let new: f64 = page.evaluate("document.body.scrollHeight").await?;
        if new == previous {
            debug!("Reached bottom after {} scrolls", i + 1);
            break;
        }
    }
    Ok(())
}

/// Scrolls to the middle of the page.
pub async fn scroll_to_half(page: &Page) -> Result<()> {
    page.run_js("window.scrollTo(0, document.body.scrollHeight / 2)")
        .await
}

/// Clicks every visible "See more" / "Show more" / "show all" button.
pub async fn click_see_more_buttons(page: &Page, max_attempts: u32) -> u32 {
    let mut clicked = 0;
    for _ in 0..max_attempts {
        let see_more = page
            .locator(r#"button:has-text("See more"), button:has-text("Show more"), button:has-text("show all")"#)
            .first();
        match see_more.is_visible().await {
            Ok(true) => {
                if see_more.click().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                clicked += 1;
            }
            _ => break,
        }
    }
    if clicked > 0 {
        debug!("Clicked {clicked} 'see more' buttons");
    }
    clicked
}

/// Closes a blocking modal if one is visible.
pub async fn handle_modal_close(page: &Page) -> bool {
    let close = page
        .locator(r#"button[aria-label="Dismiss"], button[aria-label="Close"], button.artdeco-modal__dismiss"#)
        .first();
    if let Ok(true) = close.is_visible().await
        && close.click().await.is_ok()
    {
        tokio::time::sleep(Duration::from_millis(500)).await;
        debug!("Closed modal");
        return true;
    }
    false
}

/// `document.readyState == "complete"`.
pub async fn is_page_loaded(page: &Page) -> bool {
    matches!(page.evaluate::<String>("document.readyState").await, Ok(s) if s == "complete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test(start_paused = true)]
    async fn retry_retries_only_matching_errors() {
        let calls = AtomicU32::new(0);
        let res: Result<()> = retry_async(3, 2.0, Error::is_timeout, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::Timeout("t".into()))
        })
        .await;
        assert!(res.unwrap_err().is_timeout());
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let res: Result<()> = retry_async(3, 2.0, Error::is_timeout, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::Scraping("s".into()))
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn suggestions() {
        assert!(selector_suggestions("#main").starts_with("Tip: ID"));
        assert!(selector_suggestions(".pv-top-card").contains("class names"));
        assert_eq!(selector_suggestions("h1"), "");
    }
}
