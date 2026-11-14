//! LinkedIn authentication (port of `core/auth.py`).

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use tracing::{debug, info, warn};

use super::page::{ElementState, Page, WaitUntil};
use super::utils::detect_rate_limit;
use crate::error::{Error, Result};

/// Visits a few common sites first to look more like a normal browser.
pub async fn warm_up_browser(page: &Page) {
    info!("Warming up browser by visiting normal sites...");
    for site in [
        "https://www.google.com",
        "https://www.wikipedia.org",
        "https://www.github.com",
    ] {
        match page
            .goto(site, WaitUntil::DomContentLoaded, Duration::from_secs(10))
            .await
        {
            Ok(()) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                debug!("Visited {site}");
            }
            Err(e) => debug!("Could not visit {site}: {e}"),
        }
    }
    info!("Browser warm-up complete");
}

/// Reads `LINKEDIN_EMAIL` (or `LINKEDIN_USERNAME`) and `LINKEDIN_PASSWORD`,
/// loading a `.env` file first if present.
pub fn load_credentials_from_env() -> (Option<String>, Option<String>) {
    let _ = dotenvy::dotenv();
    let email = std::env::var("LINKEDIN_EMAIL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("LINKEDIN_USERNAME")
                .ok()
                .filter(|s| !s.is_empty())
        });
    let password = std::env::var("LINKEDIN_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    (email, password)
}

/// Logs in with email/password (falls back to the environment when `None`).
///
/// `timeout` is used for the login form and the post-submit navigation.
pub async fn login_with_credentials(
    page: &Page,
    email: Option<&str>,
    password: Option<&str>,
    timeout: Duration,
    warm_up: bool,
) -> Result<()> {
    let (mut email, mut password) = (
        email.filter(|s| !s.is_empty()).map(str::to_owned),
        password.filter(|s| !s.is_empty()).map(str::to_owned),
    );
    if email.is_none() || password.is_none() {
        let (env_email, env_password) = load_credentials_from_env();
        email = email.or(env_email);
        password = password.or(env_password);
    }
    let (Some(email), Some(password)) = (email, password) else {
        return Err(Error::Authentication(
            "LinkedIn credentials not provided. Either pass email/password parameters \
             or set LINKEDIN_EMAIL and LINKEDIN_PASSWORD in your .env file."
                .into(),
        ));
    };

    if warm_up {
        warm_up_browser(page).await;
    }

    info!("Logging in to LinkedIn...");
    match do_login(page, &email, &password, timeout).await {
        Ok(()) => Ok(()),
        Err(e @ Error::Authentication(_)) => Err(e),
        Err(e) if e.is_timeout() => Err(Error::Authentication(format!(
            "Login timed out: {e}. This could indicate network issues or LinkedIn blocking the request."
        ))),
        Err(e) => Err(Error::Authentication(format!(
            "Unexpected error during login: {e}"
        ))),
    }
}

async fn do_login(page: &Page, email: &str, password: &str, timeout: Duration) -> Result<()> {
    page.goto(
        "https://www.linkedin.com/login",
        WaitUntil::DomContentLoaded,
        page.default_timeout(),
    )
    .await?;

    detect_rate_limit(page).await?;

    if let Err(e) = page
        .wait_for_selector("#username", timeout, ElementState::Visible)
        .await
    {
        if e.is_timeout() {
            return Err(Error::Authentication(
                "Login form not found. LinkedIn may have changed their page structure \
                 or the site is experiencing issues."
                    .into(),
            ));
        }
        return Err(e);
    }

    page.fill("#username", email).await?;
    page.fill("#password", password).await?;
    debug!("Credentials entered");

    page.click(r#"button[type="submit"]"#).await?;

    if let Err(e) = page
        .wait_for_url(
            |url| url.contains("feed") || url.contains("checkpoint") || url.contains("authwall"),
            timeout,
        )
        .await
    {
        if !e.is_timeout() {
            return Err(e);
        }
        if page.url().await.contains("login") {
            return Err(Error::Authentication(
                "Login failed. Please check your credentials. \
                 The page did not navigate after clicking sign in."
                    .into(),
            ));
        }
    }

    let current_url = page.url().await;
    if current_url.contains("checkpoint") || current_url.contains("challenge") {
        return Err(Error::Authentication(format!(
            "LinkedIn security checkpoint detected. You may need to verify your identity manually. \
             Consider using session persistence after manual verification. Current URL: {current_url}"
        )));
    }
    if current_url.contains("authwall") {
        return Err(Error::Authentication(format!(
            "Authentication wall encountered. LinkedIn may be blocking automated access. \
             Current URL: {current_url}"
        )));
    }

    if poll_logged_in(page, Duration::from_secs(5)).await {
        info!("✓ Successfully logged in to LinkedIn");
    } else {
        warn!("Could not verify login by finding navigation element. Proceeding anyway...");
    }
    Ok(())
}

/// Polls [`is_logged_in`] every 500 ms for up to `limit`.
pub async fn poll_logged_in(page: &Page, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if is_logged_in(page).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// Logs in with the `li_at` session cookie.
pub async fn login_with_cookie(page: &Page, cookie_value: &str) -> Result<()> {
    info!("Logging in with cookie...");
    let res: Result<()> = async {
        let cookie = CookieParam::builder()
            .name("li_at")
            .value(cookie_value)
            .domain(".linkedin.com")
            .path("/")
            .build()
            .map_err(Error::Runtime)?;
        page.inner().set_cookies(vec![cookie]).await?;

        page.goto(
            "https://www.linkedin.com/feed/",
            WaitUntil::DomContentLoaded,
            page.default_timeout(),
        )
        .await?;

        let url = page.url().await;
        if url.contains("login") || url.contains("authwall") {
            return Err(Error::Authentication(
                "Cookie authentication failed. The cookie may be expired or invalid.".into(),
            ));
        }

        if poll_logged_in(page, Duration::from_secs(5)).await {
            info!("✓ Successfully authenticated with cookie");
        } else {
            warn!("Could not verify cookie login. Proceeding anyway...");
        }
        Ok(())
    }
    .await;

    match res {
        Err(e @ Error::Authentication(_)) => Err(e),
        Err(e) => Err(Error::Authentication(format!(
            "Cookie authentication error: {e}"
        ))),
        ok => ok,
    }
}

/// Heuristic check that the current page is an authenticated LinkedIn page.
pub async fn is_logged_in(page: &Page) -> bool {
    let current_url = page.url().await;

    const AUTH_BLOCKERS: [&str; 6] = [
        "/login",
        "/authwall",
        "/checkpoint",
        "/challenge",
        "/uas/login",
        "/uas/consumer-email-challenge",
    ];
    if AUTH_BLOCKERS.iter().any(|p| current_url.contains(p)) {
        return false;
    }

    let old_count = page
        .locator(r#".global-nav__primary-link, [data-control-name="nav.settings"]"#)
        .count()
        .await;
    let new_count = page
        .locator(r#"nav a[href*="/feed"], nav button:has-text("Home"), nav a[href*="/mynetwork"]"#)
        .count()
        .await;
    let (Ok(old_count), Ok(new_count)) = (old_count, new_count) else {
        return false;
    };
    let has_nav_elements = old_count > 0 || new_count > 0;

    const AUTHENTICATED_ONLY: [&str; 4] = ["/feed", "/mynetwork", "/messaging", "/notifications"];
    let is_authenticated_page = AUTHENTICATED_ONLY.iter().any(|p| current_url.contains(p));

    has_nav_elements || is_authenticated_page
}

/// Waits for the user to log in by hand (2FA, CAPTCHA…). Default in Python: 5 minutes.
pub async fn wait_for_manual_login(page: &Page, timeout: Duration) -> Result<()> {
    info!(
        "⏳ Please complete the login process manually in the browser. Waiting up to 5 minutes..."
    );
    let start = Instant::now();
    loop {
        if is_logged_in(page).await {
            info!("✓ Manual login completed successfully");
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(Error::Authentication(
                "Manual login timeout. Please try again and complete login faster.".into(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
