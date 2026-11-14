//! LinkedIn scrapers (port of `scrapers/`).

mod base;
mod company;
mod company_posts;
mod company_search;
mod job;
mod job_search;
mod people_search;
mod person;
mod profile_dom;
mod search_dom;

pub use base::BaseScraper;
pub use company::{CompanyDetails, CompanyScraper};
pub use company_posts::CompanyPostsScraper;
pub(crate) use company_search::rebase as rebase_origin;
pub use company_search::{
    CompanyHit, CompanyId, CompanyIdEvidence, CompanySearch, IdSource, LINKEDIN_ORIGIN,
};
pub use job::JobScraper;
pub use job_search::JobSearchScraper;
pub use people_search::{PeopleSearch, PeopleSearchParams, PersonHit};
pub use person::PersonScraper;
pub(crate) use search_dom::canonical_url as canonical_linkedin_url;

/// Python `urllib.parse.urljoin` (RFC 3986 resolution, like `url::Url::join`).
///
/// As in the Python library, a profile URL without a trailing slash loses its
/// last segment: always pass profile URLs ending with `/`.
pub(crate) fn urljoin(base: &str, relative: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(relative))
        .map_or_else(
            |_| format!("{}/{relative}", base.trim_end_matches('/')),
            String::from,
        )
}

/// Drops the query string and prefixes relative LinkedIn paths with the host.
pub(crate) fn absolutize_linkedin_url(href: &str) -> String {
    let clean = href.split('?').next().unwrap_or(href);
    if clean.starts_with("http") {
        clean.to_string()
    } else {
        format!("https://www.linkedin.com{clean}")
    }
}

/// Decodes `%XX` escapes (UTF-8); invalid escapes are kept as is.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(hex) = std::str::from_utf8(&b[i + 1..i + 3])
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urljoin_matches_python() {
        assert_eq!(
            urljoin("https://www.linkedin.com/in/john/", "details/experience"),
            "https://www.linkedin.com/in/john/details/experience"
        );
        // Same (surprising) behaviour as Python's urljoin without trailing slash.
        assert_eq!(
            urljoin("https://www.linkedin.com/in/john", "details/experience"),
            "https://www.linkedin.com/in/details/experience"
        );
    }

    #[test]
    fn absolutize() {
        assert_eq!(
            absolutize_linkedin_url("/company/acme/life?trk=x"),
            "https://www.linkedin.com/company/acme/life"
        );
        assert_eq!(absolutize_linkedin_url("https://a.b/c?d"), "https://a.b/c");
    }
}
