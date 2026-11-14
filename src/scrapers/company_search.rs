//! Company search (`/search/results/companies/?keywords=…`) and company id
//! lookup. Opt-in: used by [`crate::resolve::Resolver`].

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::base::{BaseScraper, impl_with_callback};
use super::company::parse_leading_count;
use super::search_dom::{
    COMPANY_ID_JS, RawSearchPage, canonical_url, is_search_limit, search_page_js, search_url,
};
pub use super::search_dom::{CompanyId, CompanyIdEvidence, IdSource};
use crate::core::page::Page;
use crate::error::{Error, Result};

/// Default LinkedIn origin. Tests point scrapers to a local server instead.
pub const LINKEDIN_ORIGIN: &str = "https://www.linkedin.com";

/// One company search result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyHit {
    pub name: String,
    /// Canonical `https://www.linkedin.com/company/<slug>/`.
    pub url: String,
    /// Decoded slug.
    pub slug: String,
    /// Industry line.
    pub subtitle: Option<String>,
    pub location: Option<String>,
    pub followers: Option<u64>,
    /// Filled when the candidate's `/about/` page was checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub industry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headquarters: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<CompanyId>,
}

impl CompanyHit {
    /// `true` once the `/about/` page was read.
    pub fn verified(&self) -> bool {
        self.website.is_some()
            || self.industry.is_some()
            || self.headquarters.is_some()
            || self.company_id.is_some()
    }
}

/// Turns raw search results into hits (private / unparsable ones dropped).
pub(crate) fn company_hits(page: &RawSearchPage) -> Vec<CompanyHit> {
    let mut seen = std::collections::HashSet::new();
    page.items
        .iter()
        .filter_map(|r| {
            let (url, slug) = canonical_url(r.url.as_deref()?, "company")?;
            if r.name.is_empty() || !seen.insert(url.clone()) {
                return None;
            }
            let clean = |s: &String| {
                let s = s.trim().trim_end_matches([',', ' ']).trim().to_string();
                (!s.is_empty()).then_some(s)
            };
            Some(CompanyHit {
                name: r.name.clone(),
                url,
                slug,
                subtitle: r.lines.first().and_then(clean),
                location: r.lines.get(1).and_then(clean),
                followers: r
                    .followers
                    .as_deref()
                    .and_then(parse_leading_count)
                    .map(|n| n as u64),
                ..Default::default()
            })
        })
        .collect()
}

/// Searches companies by name.
pub struct CompanySearch {
    base: BaseScraper,
    origin: String,
}

impl_with_callback!(CompanySearch);

impl CompanySearch {
    pub fn new(page: Page) -> Self {
        CompanySearch {
            base: BaseScraper::new(page),
            origin: LINKEDIN_ORIGIN.to_string(),
        }
    }

    /// Uses another origin than `https://www.linkedin.com` (tests).
    pub fn with_base_url(mut self, origin: impl Into<String>) -> Self {
        self.origin = origin.into().trim_end_matches('/').to_string();
        self
    }

    /// `https://www.linkedin.com/x` → `<origin>/x` (identity in production).
    pub(crate) fn rebase(&self, url: &str) -> String {
        rebase(&self.origin, url)
    }

    /// Up to `limit` hits; one result page (10 hits), two at most when
    /// `limit > 10` and the first page was full.
    pub async fn search(&self, name: &str, limit: usize) -> Result<Vec<CompanyHit>> {
        let mut hits = self.search_page(name, 1).await?;
        if limit > 10 && hits.len() >= 10 {
            for h in self.search_page(name, 2).await? {
                if !hits.iter().any(|x| x.url == h.url) {
                    hits.push(h);
                }
            }
        }
        hits.truncate(limit.max(1));
        Ok(hits)
    }

    /// One page of results (`page` starts at 1).
    pub async fn search_page(&self, name: &str, page: u32) -> Result<Vec<CompanyHit>> {
        let url = search_url(&self.origin, "companies", name, &[], page);
        let raw = load_search_page(&self.base, &url, "/company/").await?;
        let hits = company_hits(&raw);
        info!(
            "Company search \"{name}\" (page {page}): {} result(s)",
            hits.len()
        );
        self.base
            .progress(
                &format!("Company search \"{name}\": {} result(s)", hits.len()),
                50,
            )
            .await;
        Ok(hits)
    }

    /// Numeric id of a company (needed for `currentCompany` people filters).
    pub async fn company_id(&self, url: &str) -> Result<Option<u64>> {
        Ok(self.company_id_evidence(url).await?.pick().map(|c| c.id))
    }

    /// Both id clues of a company page (header link and URN majority), for
    /// callers that want to know how reliable the id is.
    pub async fn company_id_evidence(&self, url: &str) -> Result<CompanyIdEvidence> {
        // No ensure_logged_in() here: company pages still use the old navbar,
        // which is_logged_in() does not recognise. A logged-out session is
        // redirected to the authwall, caught by navigate_and_wait().
        self.base.navigate_and_wait(&self.rebase(url)).await?;
        let _ = self.base.wait_for_selector("main h1, h1", 8_000).await;
        let ev: CompanyIdEvidence = self.base.page.evaluate(COMPANY_ID_JS).await?;
        debug!("Company id evidence for {url}: {ev:?}");
        Ok(ev)
    }
}

/// `https://www.linkedin.com/x` → `<origin>/x`.
pub(crate) fn rebase(origin: &str, url: &str) -> String {
    match url.strip_prefix(LINKEDIN_ORIGIN) {
        Some(rest) if origin != LINKEDIN_ORIGIN => format!("{origin}{rest}"),
        _ => url.to_string(),
    }
}

/// Opens a search URL, waits for results (or their absence) and extracts
/// them. Fails with [`Error::SearchLimitReached`] on the monthly limit page.
pub(crate) async fn load_search_page(
    base: &BaseScraper,
    url: &str,
    segment: &str,
) -> Result<RawSearchPage> {
    base.navigate_and_wait(url).await?;
    base.ensure_logged_in().await?;
    if base
        .wait_for_selector(r#"main [role="listitem"]"#, 8_000)
        .await
        .is_err()
    {
        debug!("No result list on {url}");
    }
    base.wait_and_focus(0.8).await;
    let raw: RawSearchPage = base.page.evaluate(&search_page_js(segment)).await?;
    if is_search_limit(&raw) {
        return Err(Error::SearchLimitReached(
            "LinkedIn search limit reached (monthly \"commercial use limit\" of free accounts). \
             Searches are blocked until the limit resets."
                .into(),
        ));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::super::search_dom::RawSearchItem;
    use super::*;

    #[test]
    fn hits() {
        let page = RawSearchPage {
            items: vec![
                RawSearchItem {
                    url: Some("https://www.linkedin.com/company/acme-compost/".into()),
                    name: "Acme Compost".into(),
                    lines: vec!["Collecte des déchets".into(), "Toulon, Var".into()],
                    followers: Some("1\u{202f}204\u{a0}abonnés".into()),
                },
                RawSearchItem {
                    url: Some("https://www.linkedin.com/company/acmefilms/?trk=x".into()),
                    name: "Acme Films".into(),
                    lines: vec!["Production".into(), "France , ".into()],
                    followers: None,
                },
                RawSearchItem {
                    url: Some("https://www.linkedin.com/company/acme-compost/".into()),
                    name: "Acme Compost (dup)".into(),
                    ..Default::default()
                },
                RawSearchItem {
                    url: None,
                    name: "Private".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let h = company_hits(&page);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].slug, "acme-compost");
        assert_eq!(h[0].followers, Some(1204));
        assert_eq!(h[0].location.as_deref(), Some("Toulon, Var"));
        assert_eq!(h[1].url, "https://www.linkedin.com/company/acmefilms/");
        assert_eq!(h[1].location.as_deref(), Some("France"));
    }

    #[test]
    fn rebasing() {
        assert_eq!(
            rebase(
                "http://127.0.0.1:9/fake.linkedin.com",
                "https://www.linkedin.com/company/a/"
            ),
            "http://127.0.0.1:9/fake.linkedin.com/company/a/"
        );
        assert_eq!(
            rebase(LINKEDIN_ORIGIN, "https://www.linkedin.com/company/a/"),
            "https://www.linkedin.com/company/a/"
        );
    }
}
