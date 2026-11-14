//! Job posting scraper (port of `scrapers/job.py`).

use std::time::Duration;

use tracing::info;

use super::absolutize_linkedin_url;
use super::base::{BaseScraper, impl_with_callback};
use crate::core::page::{ElementState, Page};
use crate::error::Result;
use crate::models::Job;

/// Scrapes `https://www.linkedin.com/jobs/view/<id>/` pages.
pub struct JobScraper {
    base: BaseScraper,
}

impl_with_callback!(JobScraper);

const TOP_CARD: &str = ".job-details-jobs-unified-top-card__primary-description-container";

impl JobScraper {
    pub fn new(page: Page) -> Self {
        Self {
            base: BaseScraper::new(page),
        }
    }

    fn page(&self) -> &Page {
        &self.base.page
    }

    pub async fn scrape(&self, linkedin_url: &str) -> Result<Job> {
        let b = &self.base;
        info!("Starting job scraping: {linkedin_url}");
        b.callback.on_start("Job", linkedin_url).await;

        b.navigate_and_wait(linkedin_url).await?;
        b.progress("Navigated to job page", 10).await;
        b.check_rate_limit().await?;

        let job_title = self.get_job_title().await;
        b.progress(
            &format!("Got job title: {}", job_title.as_deref().unwrap_or("None")),
            20,
        )
        .await;
        let company = self.get_company().await;
        b.progress("Got company name", 30).await;
        let location = self.get_location().await;
        b.progress("Got location", 40).await;
        let posted_date = self.get_posted_date().await;
        b.progress("Got posted date", 50).await;
        let applicant_count = self.get_applicant_count().await;
        b.progress("Got applicant count", 60).await;
        let job_description = self.get_description().await;
        b.progress("Got job description", 80).await;
        let company_linkedin_url = self.get_company_url().await;
        b.progress("Got company URL", 90).await;

        let job = Job {
            linkedin_url: linkedin_url.to_string(),
            job_title: job_title.clone(),
            company,
            company_linkedin_url,
            location,
            posted_date,
            applicant_count,
            job_description,
            benefits: None,
        };
        job.validate()?;

        b.progress("Scraping complete", 100).await;
        b.callback.on_complete("Job", &job.to_dict()).await;
        info!(
            "Successfully scraped job: {}",
            job_title.as_deref().unwrap_or("None")
        );
        Ok(job)
    }

    async fn get_job_title(&self) -> Option<String> {
        let title = self
            .page()
            .locator("h1")
            .first()
            .timeout(Duration::from_secs(5));
        title.wait_for(ElementState::Visible).await.ok()?;
        Some(title.inner_text().await.ok()?.trim().to_string())
    }

    async fn get_company(&self) -> Option<String> {
        let texts = self
            .page()
            .locator(r#"a[href*="/company/"]"#)
            .all_inner_texts()
            .await
            .ok()?;
        texts
            .into_iter()
            .map(|t| t.trim().to_string())
            .find(|t| !t.is_empty() && t.chars().count() > 1 && !t.starts_with("logo"))
    }

    async fn get_company_url(&self) -> Option<String> {
        let link = self.page().locator(r#"a[href*="/company/"]"#).first();
        if link.count().await.ok()? == 0 {
            return None;
        }
        let href = link.get_attribute("href").await.ok()??;
        (!href.is_empty()).then(|| absolutize_linkedin_url(&href))
    }

    /// Text of the n-th `·`-separated part of the top card (first line only).
    async fn top_card_part(&self, n: usize) -> Option<String> {
        let container = self.page().locator(TOP_CARD).first();
        if container.count().await.ok()? == 0 {
            return None;
        }
        let text = container.inner_text().await.ok()?;
        top_card_field(&text, n)
    }

    async fn get_location(&self) -> Option<String> {
        if let Some(loc) = self.top_card_part(0).await {
            return Some(loc);
        }

        let panel = self
            .page()
            .locator("h1")
            .first()
            .locator("xpath=ancestor::*[5]");
        if panel.count().await.ok()? > 0 {
            let texts = panel.locator("span, div").all_inner_texts().await.ok()?;
            for text in texts {
                if text.is_empty()
                    || !(text.contains(',')
                        || text.contains("Remote")
                        || text.contains("United States"))
                {
                    continue;
                }
                let text = text.trim().to_string();
                if let Some(title) = self.get_job_title().await
                    && text == title
                {
                    continue;
                }
                let len = text.chars().count();
                if len > 3 && len < 100 && !text.starts_with('$') {
                    return Some(text);
                }
            }
        }
        None
    }

    async fn get_posted_date(&self) -> Option<String> {
        if let Some(d) = self.top_card_part(1).await {
            return Some(d);
        }
        let texts = self
            .page()
            .locator("span, div")
            .all_inner_texts()
            .await
            .ok()?;
        texts.into_iter().find_map(|text| {
            let lower = text.to_lowercase();
            let hit = ["ago", "day", "week", "hour"]
                .iter()
                .any(|w| lower.contains(w));
            let text = text.trim();
            (hit && !text.is_empty() && text.chars().count() < 50).then(|| text.to_string())
        })
    }

    async fn get_applicant_count(&self) -> Option<String> {
        if let Some(c) = self.top_card_part(2).await {
            return Some(c);
        }
        let main = self.page().locator("main").first();
        if main.count().await.ok()? == 0 {
            return None;
        }
        let texts = main.locator("span, div").all_inner_texts().await.ok()?;
        texts.into_iter().find_map(|text| {
            let text = text.trim();
            let lower = text.to_lowercase();
            (!text.is_empty()
                && text.chars().count() < 50
                && (lower.contains("applicant")
                    || lower.contains("people clicked")
                    || lower.contains("applied")))
            .then(|| text.to_string())
        })
    }

    async fn get_description(&self) -> Option<String> {
        let heading = self
            .page()
            .locator(r#"h2:has-text("About the job")"#)
            .first();
        if heading.count().await.ok()? > 0 {
            let article = heading.locator("xpath=ancestor::article[1]");
            if article.count().await.ok()? > 0 {
                return Some(article.inner_text().await.ok()?.trim().to_string());
            }
        }
        let article = self.page().locator("article").first();
        if article.count().await.ok()? > 0 {
            return Some(article.inner_text().await.ok()?.trim().to_string());
        }
        None
    }
}

/// `"Paris, France · 2 days ago · 50 applicants"` → part `n`, first line, trimmed.
pub(crate) fn top_card_field(text: &str, n: usize) -> Option<String> {
    let part = text.split('·').nth(n)?;
    Some(
        part.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_card() {
        let t = "Paris, Île-de-France, France · 2 days ago · Over 100 applicants\nPromoted";
        assert_eq!(
            top_card_field(t, 0).as_deref(),
            Some("Paris, Île-de-France, France")
        );
        assert_eq!(top_card_field(t, 1).as_deref(), Some("2 days ago"));
        assert_eq!(top_card_field(t, 2).as_deref(), Some("Over 100 applicants"));
        assert_eq!(top_card_field(t, 3), None);
    }
}
