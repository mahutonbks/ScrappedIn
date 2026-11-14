//! # linkedin_scraper (Rust)
//!
//! Async LinkedIn scraper, a port of the Python library
//! [`joeyism/linkedin_scraper`](https://github.com/joeyism/linkedin_scraper) v3
//! from Playwright to [`chromiumoxide`] (Chrome DevTools Protocol).
//!
//! ```no_run
//! use linkedin_scraper::{BrowserManager, BrowserOptions, PersonScraper};
//!
//! # async fn demo() -> linkedin_scraper::Result<()> {
//! let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
//! browser.load_session("linkedin_session.json").await?;
//!
//! let person = PersonScraper::new(browser.page()?)
//!     .scrape("https://www.linkedin.com/in/williamhgates/")
//!     .await?;
//! println!("{}", person.to_json_pretty()?);
//!
//! browser.close().await;
//! # Ok(())
//! # }
//! ```

pub mod callbacks;
pub mod core;
pub mod error;
pub mod graph;
pub mod models;
pub mod resolve;
pub mod scrapers;
pub mod seniority;

/// Version of the Python library this crate ports.
pub const UPSTREAM_VERSION: &str = "3.1.2";

pub use callbacks::{
    ConsoleCallback, JsonLogCallback, MultiCallback, ProgressCallback, SilentCallback,
};
pub use core::{
    BrowserManager, BrowserOptions, ElementState, Locator, Page, WaitUntil, is_logged_in,
    load_credentials_from_env, login_with_cookie, login_with_credentials, wait_for_manual_login,
};
pub use error::{Error, Result};
pub use graph::KnowledgeGraph;
pub use models::{
    Accomplishment, Company, CompanySummary, Contact, Education, Employee, Experience, Interest,
    Job, Person, Post, sort_by_seniority,
};
pub use resolve::{
    Candidate, CompanyQuery, CompanyRef, PersonQuery, Resolution, ResolveCache, ResolveConfig,
    Resolver,
};
pub use scrapers::{
    CompanyDetails, CompanyHit, CompanyId, CompanyIdEvidence, CompanyPostsScraper, CompanyScraper,
    CompanySearch, IdSource, JobScraper, JobSearchScraper, PeopleSearch, PeopleSearchParams,
    PersonHit, PersonScraper,
};
pub use seniority::{Seniority, classify as classify_seniority};
