//! Company posts scraper (port of `scrapers/company_posts.py`).

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use tracing::{debug, info, warn};

use super::base::{BaseScraper, impl_with_callback};
use crate::core::page::{Page, WaitUntil};
use crate::error::Result;
use crate::models::Post;

/// Scrapes the latest posts of a company page (`/company/<id>/posts/`).
pub struct CompanyPostsScraper {
    base: BaseScraper,
}

impl_with_callback!(CompanyPostsScraper);

const TRIGGER_LAZY_LOAD_JS: &str = r"() => {
    const scrollHeight = document.documentElement.scrollHeight;
    const steps = 8;
    const stepSize = Math.min(scrollHeight / steps, 400);
    for (let i = 1; i <= steps; i++) {
        setTimeout(() => window.scrollTo(0, stepSize * i), i * 200);
    }
}";

const EXTRACT_POSTS_JS: &str = r#"() => {
    const posts = [];
    const html = document.body.innerHTML;
    const urnMatches = html.matchAll(/urn:li:activity:(\d+)/g);
    const seenUrns = new Set();

    for (const match of urnMatches) {
        const urn = match[0];
        if (seenUrns.has(urn)) continue;
        seenUrns.add(urn);

        const el = document.querySelector(`[data-urn="${urn}"]`);
        if (!el) continue;

        let text = '';
        const textSelectors = [
            '.feed-shared-update-v2__description',
            '.update-components-text',
            '.feed-shared-text',
            '[data-test-id="main-feed-activity-card__commentary"]',
            '.break-words.whitespace-pre-wrap'
        ];
        for (const sel of textSelectors) {
            const textEl = el.querySelector(sel);
            if (textEl) {
                const t = textEl.innerText?.trim() || '';
                if (t.length > text.length && t.length > 20 && !t.startsWith('Microsoft\nMicrosoft')) {
                    text = t;
                }
            }
        }

        if (!text || text.length < 30) {
            const allDivs = el.querySelectorAll('div, span');
            let maxLen = 0;
            allDivs.forEach(div => {
                const t = div.innerText?.trim() || '';
                if (t.length > maxLen && t.length > 50 &&
                    !t.includes('followers') &&
                    !t.includes('reactions') &&
                    !t.match(/^Microsoft\n/) &&
                    !t.match(/^\d+[hdwmy]\s/)) {
                    const parent = div.parentElement;
                    if (!parent?.classList?.contains('feed-shared-actor')) {
                        text = t;
                        maxLen = t.length;
                    }
                }
            });
        }

        if (!text || text.length < 20) continue;

        const timeEl = el.querySelector('[class*="actor__sub-description"], [class*="update-components-actor__sub-description"]');
        const timeText = timeEl ? timeEl.innerText : '';
        const reactionsEl = el.querySelector('button[aria-label*="reaction"], [class*="social-details-social-counts__reactions"]');
        const reactions = reactionsEl ? reactionsEl.innerText : '';
        const commentsEl = el.querySelector('button[aria-label*="comment"]');
        const comments = commentsEl ? commentsEl.innerText : '';
        const repostsEl = el.querySelector('button[aria-label*="repost"]');
        const reposts = repostsEl ? repostsEl.innerText : '';

        const images = [];
        el.querySelectorAll('img[src*="media"]').forEach(img => {
            if (img.src && !img.src.includes('profile') && !img.src.includes('logo')) {
                images.push(img.src);
            }
        });

        posts.push({
            urn: urn,
            text: text.substring(0, 2000),
            timeText: timeText,
            reactions: reactions,
            comments: comments,
            reposts: reposts,
            images: images
        });
    }
    return posts;
}"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPost {
    urn: String,
    text: String,
    #[serde(default)]
    time_text: String,
    #[serde(default)]
    reactions: String,
    #[serde(default)]
    comments: String,
    #[serde(default)]
    reposts: String,
    #[serde(default)]
    images: Vec<String>,
}

impl CompanyPostsScraper {
    pub fn new(page: Page) -> Self {
        Self {
            base: BaseScraper::new(page),
        }
    }

    fn page(&self) -> &Page {
        &self.base.page
    }

    /// Returns up to `limit` posts (Python default: 10).
    pub async fn scrape(&self, company_url: &str, limit: usize) -> Result<Vec<Post>> {
        let b = &self.base;
        info!("Starting company posts scraping: {company_url}");
        b.callback.on_start("company_posts", company_url).await;

        let posts_url = build_posts_url(company_url);
        b.navigate_and_wait(&posts_url).await?;
        b.progress("Navigated to posts page", 10).await;
        b.check_rate_limit().await?;

        self.wait_for_posts_to_load(Duration::from_secs(30)).await?;
        b.progress("Posts loaded", 20).await;

        let posts = self.scrape_posts(limit).await?;
        b.progress(&format!("Scraped {} posts", posts.len()), 100)
            .await;
        b.callback
            .on_complete("company_posts", &serde_json::to_value(&posts)?)
            .await;

        info!("Successfully scraped {} posts", posts.len());
        Ok(posts)
    }

    async fn wait_for_posts_to_load(&self, timeout: Duration) -> Result<()> {
        if let Err(e) = self
            .page()
            .wait_for_load_state(WaitUntil::DomContentLoaded, timeout)
            .await
        {
            debug!("DOM load timeout: {e}");
        }
        self.page().wait_for_timeout(3000).await;

        for attempt in 0..3 {
            self.trigger_lazy_load().await?;
            let has_posts: bool = self
                .page()
                .evaluate("() => document.body.innerHTML.includes('urn:li:activity:')")
                .await?;
            if has_posts {
                debug!("Posts found after attempt {}", attempt + 1);
                return Ok(());
            }
            self.page().wait_for_timeout(2000).await;
        }
        warn!("Posts may not have loaded fully");
        Ok(())
    }

    async fn trigger_lazy_load(&self) -> Result<()> {
        self.page().run_js(TRIGGER_LAZY_LOAD_JS).await?;
        self.page().wait_for_timeout(2500).await;
        self.page().run_js("window.scrollTo(0, 400)").await?;
        self.page().wait_for_timeout(1000).await;
        Ok(())
    }

    async fn scrape_posts(&self, limit: usize) -> Result<Vec<Post>> {
        let mut posts: Vec<Post> = Vec::new();
        let max_scrolls = limit / 3 + 2;
        let mut scroll_count = 0;

        while posts.len() < limit && scroll_count < max_scrolls {
            for post in self.extract_posts_from_page().await? {
                if post.urn.is_some() && !posts.iter().any(|p| p.urn == post.urn) {
                    posts.push(post);
                    if posts.len() >= limit {
                        break;
                    }
                }
            }
            if posts.len() < limit {
                self.scroll_for_more_posts().await;
                scroll_count += 1;
            }
        }
        posts.truncate(limit);
        Ok(posts)
    }

    async fn extract_posts_from_page(&self) -> Result<Vec<Post>> {
        let raw: Vec<RawPost> = self.page().evaluate(EXTRACT_POSTS_JS).await?;
        Ok(raw.into_iter().map(raw_to_post).collect())
    }

    async fn scroll_for_more_posts(&self) {
        if let Err(e) = self.page().press_key("End").await {
            debug!("Error scrolling: {e}");
        }
        self.page().wait_for_timeout(1500).await;
    }
}

fn raw_to_post(d: RawPost) -> Post {
    let activity_id = d.urn.replace("urn:li:activity:", "");
    Post {
        linkedin_url: Some(format!(
            "https://www.linkedin.com/feed/update/urn:li:activity:{activity_id}/"
        )),
        urn: Some(d.urn),
        text: Some(d.text),
        posted_date: extract_time_from_text(&d.time_text),
        reactions_count: parse_count(&d.reactions),
        comments_count: parse_count(&d.comments),
        reposts_count: parse_count(&d.reposts),
        image_urls: d.images,
        video_url: None,
        article_url: None,
    }
}

/// `https://…/company/x` → `https://…/company/x/posts/`.
pub(crate) fn build_posts_url(company_url: &str) -> String {
    let url = company_url.trim_end_matches('/');
    if url.contains("/posts") {
        url.to_string()
    } else {
        format!("{url}/posts/")
    }
}

static TIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\d+[hdwmy]|\d+\s*(?:hour|day|week|month|year)s?\s*ago)").expect("valid regex")
});
static COUNT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\d,]+").expect("valid regex"));

/// `"2d • Edited •"` → `"2d"`.
pub(crate) fn extract_time_from_text(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if let Some(m) = TIME_RE.captures(text).and_then(|c| c.get(1)) {
        return Some(m.as_str().trim().to_string());
    }
    Some(text.split('•').next().unwrap_or("").trim().to_string())
}

/// First integer in `text` (`"1,234 reactions"` → `1234`).
pub(crate) fn parse_count(text: &str) -> Option<i64> {
    if text.is_empty() {
        return None;
    }
    let cleaned = text.replace(',', "");
    COUNT_RE.find(&cleaned)?.as_str().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_url() {
        assert_eq!(
            build_posts_url("https://www.linkedin.com/company/microsoft/"),
            "https://www.linkedin.com/company/microsoft/posts/"
        );
        assert_eq!(
            build_posts_url("https://www.linkedin.com/company/microsoft/posts/"),
            "https://www.linkedin.com/company/microsoft/posts"
        );
    }

    #[test]
    fn times() {
        assert_eq!(
            extract_time_from_text("2d • Edited •").as_deref(),
            Some("2d")
        );
        assert_eq!(
            extract_time_from_text("3 weeks ago").as_deref(),
            Some("3 weeks ago")
        );
        assert_eq!(
            extract_time_from_text("Promoted • x").as_deref(),
            Some("Promoted")
        );
        assert_eq!(extract_time_from_text(""), None);
    }

    #[test]
    fn counts() {
        assert_eq!(parse_count("1,234 reactions"), Some(1234));
        assert_eq!(parse_count("56 comments"), Some(56));
        assert_eq!(parse_count("Repost"), None);
        assert_eq!(parse_count(""), None);
    }
}
