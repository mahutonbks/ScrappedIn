//! Error types (port of `core/exceptions.py`).

use thiserror::Error;

/// Every error the scraper can return. Mirrors the Python exception hierarchy
/// (`LinkedInScraperException` and its subclasses) plus the low-level errors
/// coming from the browser, I/O and JSON layers.
#[derive(Debug, Error)]
pub enum Error {
    /// Authentication failed (`AuthenticationError`).
    #[error("{0}")]
    Authentication(String),

    /// Rate limiting detected (`RateLimitError`).
    #[error("{message}")]
    RateLimit {
        message: String,
        /// Suggested wait time in seconds before retrying.
        suggested_wait_time: u64,
    },

    /// LinkedIn's monthly search limit for free accounts ("commercial use
    /// limit") was reached: searches are blocked until it resets. Batch
    /// resolution stops on it instead of waiting.
    #[error("{0}")]
    SearchLimitReached(String),

    /// An expected element was not found (`ElementNotFoundError`).
    #[error("{0}")]
    ElementNotFound(String),

    /// A profile/page returned 404 (`ProfileNotFoundError`).
    #[error("{0}")]
    ProfileNotFound(String),

    /// Network-related issue (`NetworkError`).
    #[error("{0}")]
    Network(String),

    /// Scraping failed (`ScrapingError`).
    #[error("{0}")]
    Scraping(String),

    /// A wait exceeded its timeout (equivalent of Playwright's `TimeoutError`).
    #[error("Timeout: {0}")]
    Timeout(String),

    /// A model failed validation (equivalent of Pydantic's `ValidationError`).
    #[error("Validation error: {0}")]
    Validation(String),

    /// Misuse of the API, e.g. browser not started (`RuntimeError` in Python).
    #[error("{0}")]
    Runtime(String),

    /// Error from the Chrome DevTools Protocol layer.
    #[error("Browser error: {0}")]
    Browser(#[from] chromiumoxide::error::CdpError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Short name of the variant, used e.g. by [`crate::JsonLogCallback`]
    /// (Python logs `type(error).__name__`).
    pub fn type_name(&self) -> &'static str {
        match self {
            Error::Authentication(_) => "AuthenticationError",
            Error::RateLimit { .. } => "RateLimitError",
            Error::SearchLimitReached(_) => "SearchLimitReachedError",
            Error::ElementNotFound(_) => "ElementNotFoundError",
            Error::ProfileNotFound(_) => "ProfileNotFoundError",
            Error::Network(_) => "NetworkError",
            Error::Scraping(_) => "ScrapingError",
            Error::Timeout(_) => "TimeoutError",
            Error::Validation(_) => "ValidationError",
            Error::Runtime(_) => "RuntimeError",
            Error::Browser(_) => "BrowserError",
            Error::Io(_) => "IoError",
            Error::Json(_) => "JsonError",
        }
    }

    /// `true` for [`Error::Timeout`].
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
