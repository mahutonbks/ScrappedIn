//! Job search scraper (port of `scrapers/job_search.py`).

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use tracing::{info, warn};

use super::absolutize_linkedin_url;
use super::base::{BaseScraper, impl_with_callback};
use crate::core::page::{ElementState, Page};
use crate::error::Result;

/// Searches jobs and returns posting URLs.
pub struct JobSearchScraper {
    base: BaseScraper,
}

impl_with_callback!(JobSearchScraper);

impl JobSearchScraper {
    pub fn new(page: Page) -> Self {
        Self {
            base: BaseScraper::new(page),
        }
    }

    /// Returns at most `limit` job URLs (Python default: 25).
    pub async fn search(
        &self,
        keywords: Option<&str>,
        location: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        let b = &self.base;
        info!(
            "Starting job search: keywords='{}', location='{}'",
            keywords.unwrap_or("None"),
            location.unwrap_or("None")
        );
        let search_url = build_search_url(keywords, location);
        b.callback.on_start("JobSearch", &search_url).await;

        b.navigate_and_wait(&search_url).await?;
        b.progress("Navigated to search results", 20).await;

        if b.page
            .wait_for_selector(
                r#"a[href*="/jobs/view/"]"#,
                Duration::from_secs(10),
                ElementState::Visible,
            )
            .await
            .is_err()
        {
            warn!("No job listings found on page");
            return Ok(Vec::new());
        }

        b.wait_and_focus(1.0).await;
        b.scroll_page_to_bottom(1.0, 3).await?;
        b.progress("Loaded job listings", 50).await;

        let job_urls = self.extract_job_urls(limit).await;
        b.progress(&format!("Found {} job URLs", job_urls.len()), 90)
            .await;
        b.progress("Search complete", 100).await;
        b.callback
            .on_complete("JobSearch", &Value::from(job_urls.clone()))
            .await;

        info!("Job search complete: found {} jobs", job_urls.len());
        Ok(job_urls)
    }

    async fn extract_job_urls(&self, limit: usize) -> Vec<String> {
        match self
            .base
            .page
            .locator(r#"a[href*="/jobs/view/"]"#)
            .evaluate_all::<Vec<Option<String>>>("els => els.map(e => e.getAttribute('href'))")
            .await
        {
            Ok(hrefs) => collect_job_urls(hrefs.into_iter().flatten(), limit),
            Err(e) => {
                warn!("Error extracting job URLs: {e}");
                Vec::new()
            }
        }
    }
}

/// `https://www.linkedin.com/jobs/search/?keywords=…&location=…`
pub(crate) fn build_search_url(keywords: Option<&str>, location: Option<&str>) -> String {
    let base = "https://www.linkedin.com/jobs/search/";
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    let mut any = false;
    for (k, v) in [("keywords", keywords), ("location", location)] {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            ser.append_pair(k, v);
            any = true;
        }
    }
    if any {
        format!("{base}?{}", ser.finish())
    } else {
        base.to_string()
    }
}

/// Cleans, absolutizes and de-duplicates job links, keeping order.
pub(crate) fn collect_job_urls(
    hrefs: impl IntoIterator<Item = String>,
    limit: usize,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for href in hrefs {
        if out.len() >= limit {
            break;
        }
        if !href.contains("/jobs/view/") {
            continue;
        }
        let url = absolutize_linkedin_url(&href);
        if seen.insert(url.clone()) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url() {
        assert_eq!(
            build_search_url(Some("software engineer"), Some("San Francisco, CA")),
            "https://www.linkedin.com/jobs/search/?keywords=software+engineer&location=San+Francisco%2C+CA"
        );
        assert_eq!(
            build_search_url(None, None),
            "https://www.linkedin.com/jobs/search/"
        );
    }

    #[test]
    fn job_urls() {
        let hrefs = [
            "/jobs/view/123/?trk=abc",
            "https://www.linkedin.com/jobs/view/123/",
            "/jobs/view/456/",
            "/company/acme/",
            "/jobs/view/789/",
        ]
        .map(String::from);
        assert_eq!(
            collect_job_urls(hrefs.clone(), 25),
            vec![
                "https://www.linkedin.com/jobs/view/123/",
                "https://www.linkedin.com/jobs/view/456/",
                "https://www.linkedin.com/jobs/view/789/",
            ]
        );
        assert_eq!(collect_job_urls(hrefs, 2).len(), 2);
    }
}
