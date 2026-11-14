//! Browser lifecycle management (port of `core/browser.py`).
//!
//! Sessions are stored in Playwright's `storage_state` JSON format, so a
//! `linkedin_session.json` created by the Python library can be loaded here
//! and vice versa.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{
    Cookie, CookieParam, CookieSameSite, TimeSinceEpoch,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use super::page::Page;
use crate::error::{Error, Result};

/// Launch options (Python `BrowserManager.__init__` arguments).
#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Run without a visible window.
    pub headless: bool,
    /// Slow down every action by this many milliseconds.
    pub slow_mo: u64,
    /// Viewport `(width, height)`, default 1280x720.
    pub viewport: (u32, u32),
    /// Custom user agent.
    pub user_agent: Option<String>,
    /// Path to the Chrome/Chromium executable (auto-detected when `None`).
    pub executable: Option<PathBuf>,
    /// Extra command-line arguments for Chrome.
    pub args: Vec<String>,
    /// Pass `--no-sandbox` (Playwright does this by default on Linux).
    pub no_sandbox: bool,
    /// Chrome profile directory. `None` (default) creates a fresh temporary
    /// profile per launch, deleted on [`BrowserManager::close`], like Playwright.
    pub user_data_dir: Option<PathBuf>,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            headless: true,
            slow_mo: 0,
            viewport: (1280, 720),
            user_agent: None,
            executable: None,
            args: Vec::new(),
            no_sandbox: true,
            user_data_dir: None,
        }
    }
}

impl BrowserOptions {
    pub fn headless(mut self, headless: bool) -> Self {
        self.headless = headless;
        self
    }

    pub fn slow_mo(mut self, ms: u64) -> Self {
        self.slow_mo = ms;
        self
    }

    pub fn viewport(mut self, width: u32, height: u32) -> Self {
        self.viewport = (width, height);
        self
    }

    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    pub fn executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Uses a persistent Chrome profile directory.
    pub fn user_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.user_data_dir = Some(dir.into());
        self
    }
}

fn temp_profile_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "linkedin-scraper-rs-{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Owns the Chrome process, the CDP handler task and the main page.
///
/// Rust has no async `Drop`, so call [`BrowserManager::close`] when done
/// (the process is still killed on drop as a fallback).
pub struct BrowserManager {
    options: BrowserOptions,
    browser: Option<Browser>,
    handler: Option<JoinHandle<()>>,
    page: Option<Page>,
    is_authenticated: bool,
    /// Temporary profile to delete on close.
    temp_profile: Option<PathBuf>,
}

impl BrowserManager {
    pub fn new(options: BrowserOptions) -> Self {
        Self {
            options,
            browser: None,
            handler: None,
            page: None,
            is_authenticated: false,
            temp_profile: None,
        }
    }

    /// Creates and starts a browser (the `async with BrowserManager()` equivalent).
    pub async fn launch(options: BrowserOptions) -> Result<Self> {
        let mut manager = Self::new(options);
        manager.start().await?;
        Ok(manager)
    }

    /// Launches Chrome and opens the main page.
    pub async fn start(&mut self) -> Result<()> {
        match self.try_start().await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.close().await;
                Err(Error::Network(format!("Failed to start browser: {e}")))
            }
        }
    }

    async fn try_start(&mut self) -> Result<()> {
        let o = &self.options;
        let (w, h) = o.viewport;
        let mut builder = BrowserConfig::builder()
            .window_size(w, h)
            .viewport(Viewport {
                width: w,
                height: h,
                ..Viewport::default()
            })
            .request_timeout(Duration::from_secs(60))
            .args(o.args.clone());
        builder = if o.headless {
            builder.new_headless_mode()
        } else {
            builder.with_head()
        };
        if o.no_sandbox {
            builder = builder.no_sandbox();
        }
        if let Some(exe) = &o.executable {
            builder = builder.chrome_executable(exe);
        }
        let profile = match &o.user_data_dir {
            Some(dir) => dir.clone(),
            None => {
                let dir = temp_profile_dir();
                self.temp_profile = Some(dir.clone());
                dir
            }
        };
        builder = builder.user_data_dir(profile);
        let config = builder.build().map_err(Error::Runtime)?;

        let (browser, mut handler) = Browser::launch(config).await?;
        self.handler = Some(tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    debug!("CDP handler error: {e}");
                }
            }
        }));
        info!("Browser launched (headless={})", self.options.headless);

        let page = self.open_page(&browser).await?;
        self.browser = Some(browser);
        self.page = Some(page);
        info!("Browser context and page created");
        Ok(())
    }

    async fn open_page(&self, browser: &Browser) -> Result<Page> {
        let inner = browser.new_page("about:blank").await?;
        if let Some(ua) = &self.options.user_agent {
            inner.set_user_agent(ua.as_str()).await?;
        }
        Ok(Page::new(inner).with_slow_mo(Duration::from_millis(self.options.slow_mo)))
    }

    /// Closes the browser and frees resources. Errors are logged, not returned.
    pub async fn close(&mut self) {
        self.page = None;
        if let Some(mut browser) = self.browser.take() {
            if let Err(e) = browser.close().await {
                error!("Error closing browser: {e}");
            }
            let _ = browser.wait().await;
        }
        if let Some(handler) = self.handler.take() {
            handler.abort();
        }
        if let Some(dir) = self.temp_profile.take() {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        info!("Browser closed");
    }

    /// Opens an additional page.
    pub async fn new_page(&self) -> Result<Page> {
        let browser = self.browser()?;
        self.open_page(browser).await
    }

    /// The main page.
    pub fn page(&self) -> Result<Page> {
        self.page.clone().ok_or_else(|| {
            Error::Runtime(
                "Browser not started. Use BrowserManager::launch or call start().".into(),
            )
        })
    }

    pub fn browser(&self) -> Result<&Browser> {
        self.browser
            .as_ref()
            .ok_or_else(|| Error::Runtime("Browser not started.".into()))
    }

    /// Saves cookies and localStorage to `filepath` (Playwright format).
    pub async fn save_session(&self, filepath: impl AsRef<Path>) -> Result<()> {
        let browser = self.browser()?;
        let cookies = browser
            .get_cookies()
            .await?
            .into_iter()
            .map(StoredCookie::from)
            .collect();

        let mut origins: BTreeMap<String, Vec<NameValue>> = BTreeMap::new();
        for page in browser.pages().await? {
            let page = Page::new(page);
            let snapshot: Option<(String, Vec<NameValue>)> = page
                .evaluate(
                    "(() => { try { if (!location.origin.startsWith('http')) return null; \
                     return [location.origin, Object.keys(localStorage).map(k => ({name: k, value: localStorage.getItem(k)}))]; } \
                     catch (e) { return null; } })()",
                )
                .await
                .unwrap_or(None);
            if let Some((origin, items)) = snapshot
                && !items.is_empty()
            {
                origins.insert(origin, items);
            }
        }

        let state = StorageState {
            cookies,
            origins: origins
                .into_iter()
                .map(|(origin, local_storage)| OriginState {
                    origin,
                    local_storage,
                })
                .collect(),
        };

        let path = filepath.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, serde_json::to_string_pretty(&state)?).await?;
        info!("Session saved to {}", path.display());
        Ok(())
    }

    /// Loads a session file (Playwright `storage_state` format).
    ///
    /// Python creates a fresh browser context; here the cookie jar is cleared
    /// and refilled instead, so existing [`Page`] handles stay valid.
    pub async fn load_session(&mut self, filepath: impl AsRef<Path>) -> Result<()> {
        let path = filepath.as_ref();
        if !path.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Session file not found: {}", path.display()),
            )));
        }
        let state: StorageState = serde_json::from_str(&tokio::fs::read_to_string(path).await?)?;

        let browser = self.browser()?;
        browser.clear_cookies().await?;
        let params: Vec<CookieParam> = state.cookies.iter().map(StoredCookie::to_param).collect();
        if !params.is_empty() {
            browser.set_cookies(params).await?;
        }

        if !state.origins.is_empty() {
            let page = self.page()?;
            let data: BTreeMap<&str, &Vec<NameValue>> = state
                .origins
                .iter()
                .map(|o| (o.origin.as_str(), &o.local_storage))
                .collect();
            let script = format!(
                "(() => {{ try {{ const data = {}; const items = data[location.origin]; \
                 if (!items || sessionStorage.getItem('__lsrs_restored')) return; \
                 for (const it of items) localStorage.setItem(it.name, it.value); \
                 sessionStorage.setItem('__lsrs_restored', '1'); }} catch (e) {{}} }})()",
                serde_json::to_string(&data)?
            );
            page.inner().add_init_script(script).await?;
        }

        self.is_authenticated = true;
        info!("Session loaded from {}", path.display());
        Ok(())
    }

    /// Sets a single cookie.
    pub async fn set_cookie(&self, name: &str, value: &str, domain: &str) -> Result<()> {
        let param = CookieParam::builder()
            .name(name)
            .value(value)
            .domain(domain)
            .path("/")
            .build()
            .map_err(Error::Runtime)?;
        self.browser()?.set_cookies(vec![param]).await?;
        debug!("Cookie set: {name}");
        Ok(())
    }

    pub fn is_authenticated(&self) -> bool {
        self.is_authenticated
    }

    pub fn set_authenticated(&mut self, value: bool) {
        self.is_authenticated = value;
    }
}

impl Drop for BrowserManager {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            handler.abort();
        }
        if let Some(dir) = self.temp_profile.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Playwright `storage_state` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageState {
    #[serde(default)]
    pub cookies: Vec<StoredCookie>,
    #[serde(default)]
    pub origins: Vec<OriginState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Unix time in seconds, `-1` for session cookies.
    pub expires: f64,
    pub http_only: bool,
    pub secure: bool,
    /// `"Strict"`, `"Lax"` or `"None"`.
    pub same_site: String,
}

impl From<Cookie> for StoredCookie {
    fn from(c: Cookie) -> Self {
        let same_site = match c.same_site {
            Some(CookieSameSite::Strict) => "Strict",
            Some(CookieSameSite::None) => "None",
            _ => "Lax",
        };
        Self {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            expires: if c.session { -1.0 } else { c.expires },
            http_only: c.http_only,
            secure: c.secure,
            same_site: same_site.to_string(),
        }
    }
}

impl StoredCookie {
    fn to_param(&self) -> CookieParam {
        let mut p = CookieParam::new(self.name.clone(), self.value.clone());
        p.domain = Some(self.domain.clone());
        p.path = Some(self.path.clone());
        p.secure = Some(self.secure);
        p.http_only = Some(self.http_only);
        p.same_site = match self.same_site.as_str() {
            "Strict" => Some(CookieSameSite::Strict),
            "None" => Some(CookieSameSite::None),
            "Lax" => Some(CookieSameSite::Lax),
            _ => None,
        };
        if self.expires > 0.0 {
            p.expires = Some(TimeSinceEpoch::new(self.expires));
        }
        p
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginState {
    pub origin: String,
    #[serde(default)]
    pub local_storage: Vec<NameValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameValue {
    pub name: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playwright_storage_state() {
        let json = r#"{
          "cookies": [
            {"name": "li_at", "value": "abc", "domain": ".www.linkedin.com", "path": "/",
             "expires": 1790000000.5, "httpOnly": true, "secure": true, "sameSite": "None"},
            {"name": "lang", "value": "v=2&lang=fr-fr", "domain": ".linkedin.com", "path": "/",
             "expires": -1, "httpOnly": false, "secure": true, "sameSite": "Lax"}
          ],
          "origins": [
            {"origin": "https://www.linkedin.com", "localStorage": [{"name": "k", "value": "v"}]}
          ]
        }"#;
        let state: StorageState = serde_json::from_str(json).unwrap();
        assert_eq!(state.cookies.len(), 2);
        assert_eq!(state.origins[0].local_storage[0].name, "k");

        let p = state.cookies[0].to_param();
        assert_eq!(p.same_site, Some(CookieSameSite::None));
        assert!(p.expires.is_some());
        assert!(state.cookies[1].to_param().expires.is_none());

        // Round-trip keeps Playwright's camelCase keys.
        let back = serde_json::to_value(&state).unwrap();
        assert_eq!(back["cookies"][0]["httpOnly"], true);
        assert_eq!(back["origins"][0]["localStorage"][0]["value"], "v");
    }
}
