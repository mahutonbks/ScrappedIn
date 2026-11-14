//! Shared scraping functionality (port of `scrapers/base.py`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::callbacks::{ProgressCallback, SilentCallback};
use crate::core::page::{ElementState, Locator, Page, WaitUntil};
use crate::core::{auth, utils};
use crate::error::{Error, Result};

/// Common state and helpers used by every scraper.
pub struct BaseScraper {
    pub page: Page,
    pub callback: Box<dyn ProgressCallback>,
    /// Last rate-limit seen by [`Self::check_rate_limit`] (suggested wait in
    /// seconds, 0 = none). Section getters swallow their errors, so scrapers
    /// read this at the end to avoid returning partial data as a success.
    rate_limited: AtomicU64,
}

impl BaseScraper {
    pub fn new(page: Page) -> Self {
        Self {
            page,
            callback: Box::new(SilentCallback),
            rate_limited: AtomicU64::new(0),
        }
    }

    /// Returns (and clears) a rate limit hit during this scrape.
    pub fn take_rate_limit(&self) -> Option<Error> {
        match self.rate_limited.swap(0, Ordering::SeqCst) {
            0 => None,
            wait => Some(Error::RateLimit {
                message: "LinkedIn blocked a request during scraping (rate limit / bot check)."
                    .into(),
                suggested_wait_time: wait,
            }),
        }
    }

    /// Fails with [`Error::Authentication`] if the session is not logged in.
    pub async fn ensure_logged_in(&self) -> Result<()> {
        // LinkedIn renders its navigation client-side: poll instead of checking once.
        if !auth::poll_logged_in(&self.page, Duration::from_secs(10)).await {
            return Err(Error::Authentication(
                "Not logged in. Please authenticate before scraping.".into(),
            ));
        }
        Ok(())
    }

    pub async fn check_rate_limit(&self) -> Result<()> {
        let res = utils::detect_rate_limit(&self.page).await;
        if let Err(Error::RateLimit {
            suggested_wait_time,
            ..
        }) = &res
        {
            self.rate_limited
                .store((*suggested_wait_time).max(1), Ordering::SeqCst);
        }
        res
    }

    pub async fn scroll_page_to_bottom(&self, pause_time: f64, max_scrolls: u32) -> Result<()> {
        utils::scroll_to_bottom(&self.page, pause_time, max_scrolls).await
    }

    pub async fn scroll_page_to_half(&self) -> Result<()> {
        utils::scroll_to_half(&self.page).await
    }

    pub async fn scroll_element_into_view(&self, selector: &str) {
        if let Err(e) = self
            .page
            .locator(selector)
            .first()
            .scroll_into_view_if_needed()
            .await
        {
            debug!("Could not scroll element into view: {selector} - {e}");
        }
    }

    pub async fn click_all_see_more_buttons(&self, max_attempts: u32) -> u32 {
        utils::click_see_more_buttons(&self.page, max_attempts).await
    }

    pub async fn close_modals(&self) -> bool {
        utils::handle_modal_close(&self.page).await
    }

    /// Trimmed text of the first match or `default`.
    pub async fn safe_extract_text(
        &self,
        selector: &str,
        default: &str,
        timeout_ms: u64,
    ) -> String {
        utils::extract_text_safe(
            &self.page,
            selector,
            default,
            Duration::from_millis(timeout_ms),
        )
        .await
    }

    /// Clicks the first match; returns `false` instead of failing.
    pub async fn safe_click(&self, selector: &str, timeout_ms: u64) -> bool {
        let res = utils::retry_async(3, 2.0, Error::is_timeout, || async {
            self.page
                .locator(selector)
                .first()
                .timeout(Duration::from_millis(timeout_ms))
                .click()
                .await
        })
        .await;
        match res {
            Ok(()) => true,
            Err(e) if e.is_timeout() => {
                debug!("Could not click element: {selector}");
                false
            }
            Err(e) => {
                warn!("Error clicking element {selector}: {e}");
                false
            }
        }
    }

    pub async fn wait_for_navigation_complete(&self, timeout_ms: u64) {
        if self
            .page
            .wait_for_load_state(WaitUntil::NetworkIdle, Duration::from_millis(timeout_ms))
            .await
            .is_err()
        {
            warn!("Navigation did not complete within timeout");
        }
    }

    /// Navigates (60 s timeout by default in Python) then checks for rate limiting.
    pub async fn navigate_and_wait(&self, url: &str) -> Result<()> {
        self.navigate_and_wait_with(url, WaitUntil::DomContentLoaded, 60_000)
            .await
    }

    pub async fn navigate_and_wait_with(
        &self,
        url: &str,
        wait_until: WaitUntil,
        timeout_ms: u64,
    ) -> Result<()> {
        info!("Navigating to: {url}");
        self.page
            .goto(url, wait_until, Duration::from_millis(timeout_ms))
            .await?;
        self.check_rate_limit().await
    }

    /// Items matching `item_selector` inside the first `container_selector`.
    pub async fn extract_list_items(
        &self,
        container_selector: &str,
        item_selector: &str,
        timeout_ms: u64,
    ) -> Vec<Locator> {
        let container = self
            .page
            .locator(container_selector)
            .first()
            .timeout(Duration::from_millis(timeout_ms));
        match container.wait_for(ElementState::Visible).await {
            Ok(()) => container
                .locator(item_selector)
                .all()
                .await
                .unwrap_or_else(|e| {
                    warn!("Error extracting list items: {e}");
                    Vec::new()
                }),
            Err(e) if e.is_timeout() => {
                warn!("Container not found: {container_selector}");
                Vec::new()
            }
            Err(e) => {
                warn!("Error extracting list items: {e}");
                Vec::new()
            }
        }
    }

    /// Attribute of the first match or `default`.
    pub async fn get_attribute_safe(
        &self,
        selector: &str,
        attribute: &str,
        default: &str,
        timeout_ms: u64,
    ) -> String {
        match self
            .page
            .locator(selector)
            .first()
            .timeout(Duration::from_millis(timeout_ms))
            .get_attribute(attribute)
            .await
        {
            Ok(Some(v)) if !v.is_empty() => v,
            _ => default.to_string(),
        }
    }

    /// Sleeps then brings the tab to the front (helps lazy-loaded content).
    pub async fn wait_and_focus(&self, duration_secs: f64) {
        tokio::time::sleep(Duration::from_secs_f64(duration_secs)).await;
        let _ = self.page.bring_to_front().await;
    }

    pub async fn count_elements(&self, selector: &str) -> usize {
        self.page.locator(selector).count().await.unwrap_or(0)
    }

    pub async fn element_exists(&self, selector: &str, timeout_ms: u64) -> bool {
        self.page
            .wait_for_selector(
                selector,
                Duration::from_millis(timeout_ms),
                ElementState::Attached,
            )
            .await
            .is_ok()
    }

    /// `page.wait_for_selector(selector, timeout)` with the default `visible` state.
    pub async fn wait_for_selector(&self, selector: &str, timeout_ms: u64) -> Result<()> {
        self.page
            .wait_for_selector(
                selector,
                Duration::from_millis(timeout_ms),
                ElementState::Visible,
            )
            .await
    }

    pub async fn progress(&self, message: &str, percent: u8) {
        self.callback.on_progress(message, percent).await;
    }
}

/// Generates the `with_callback` builder shared by all scrapers.
macro_rules! impl_with_callback {
    ($t:ty) => {
        impl $t {
            /// Sets the progress callback (defaults to [`crate::SilentCallback`]).
            pub fn with_callback(
                mut self,
                callback: impl $crate::callbacks::ProgressCallback + 'static,
            ) -> Self {
                self.base.callback = Box::new(callback);
                self
            }

            /// Same as [`Self::with_callback`] for an already boxed callback.
            pub fn with_boxed_callback(
                mut self,
                callback: Box<dyn $crate::callbacks::ProgressCallback>,
            ) -> Self {
                self.base.callback = callback;
                self
            }
        }
    };
}
pub(crate) use impl_with_callback;
