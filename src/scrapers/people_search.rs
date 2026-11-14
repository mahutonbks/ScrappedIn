//! People search (`/search/results/people/?keywords=…&currentCompany=["<id>"]`).
//! Opt-in: used by [`crate::resolve::Resolver`].

use serde::{Deserialize, Serialize};
use tracing::info;

use super::base::{BaseScraper, impl_with_callback};
use super::company_search::{LINKEDIN_ORIGIN, load_search_page};
use super::search_dom::{RawSearchPage, canonical_url, search_url};
use crate::core::page::Page;
use crate::error::Result;

/// One people search result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonHit {
    pub name: String,
    /// Canonical `https://www.linkedin.com/in/<slug>/` (slug kept
    /// percent-encoded, as LinkedIn serves it).
    pub url: String,
    /// Decoded slug.
    pub slug: String,
    pub headline: Option<String>,
    pub location: Option<String>,
}

/// Search parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeopleSearchParams {
    pub keywords: String,
    /// `currentCompany` filter (numeric company id).
    pub company_id: Option<u64>,
    /// Sent as `titleFreeText`, but LinkedIn currently drops this parameter
    /// (observed 2026-11): rely on scoring the headlines instead.
    pub title: Option<String>,
    /// Optional `geoUrn` filter (numeric geo id).
    pub geo_urn: Option<String>,
    /// Max results; 10 per page, 2 pages at most.
    pub limit: usize,
}

impl PeopleSearchParams {
    pub fn new(keywords: impl Into<String>) -> Self {
        PeopleSearchParams {
            keywords: keywords.into(),
            limit: 10,
            ..Default::default()
        }
    }

    pub(crate) fn extra(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(id) = self.company_id {
            v.push(("currentCompany", format!("[\"{id}\"]")));
        }
        if let Some(t) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            v.push(("titleFreeText", t.trim().to_string()));
        }
        if let Some(g) = self.geo_urn.as_deref().filter(|g| !g.trim().is_empty()) {
            v.push(("geoUrn", format!("[\"{}\"]", g.trim())));
        }
        v
    }
}

/// Turns raw search results into hits; private "LinkedIn Member" results
/// (no `/in/` link) are dropped.
pub(crate) fn person_hits(page: &RawSearchPage) -> Vec<PersonHit> {
    let mut seen = std::collections::HashSet::new();
    page.items
        .iter()
        .filter_map(|r| {
            let (url, slug) = canonical_url(r.url.as_deref()?, "in")?;
            if r.name.is_empty() || !seen.insert(url.clone()) {
                return None;
            }
            let clean = |s: &String| (!s.trim().is_empty()).then(|| s.trim().to_string());
            Some(PersonHit {
                name: r.name.clone(),
                url,
                slug,
                headline: r.lines.first().and_then(clean),
                location: r.lines.get(1).and_then(clean),
            })
        })
        .collect()
}

/// Searches people.
pub struct PeopleSearch {
    base: BaseScraper,
    origin: String,
}

impl_with_callback!(PeopleSearch);

impl PeopleSearch {
    pub fn new(page: Page) -> Self {
        PeopleSearch {
            base: BaseScraper::new(page),
            origin: LINKEDIN_ORIGIN.to_string(),
        }
    }

    /// Uses another origin than `https://www.linkedin.com` (tests).
    pub fn with_base_url(mut self, origin: impl Into<String>) -> Self {
        self.origin = origin.into().trim_end_matches('/').to_string();
        self
    }

    /// Up to `params.limit` hits (one page; a second only if `limit > 10`
    /// and the first page was full).
    pub async fn search(&self, params: PeopleSearchParams) -> Result<Vec<PersonHit>> {
        let mut hits = self.search_page(&params, 1).await?;
        if params.limit > 10 && hits.len() >= 10 {
            for h in self.search_page(&params, 2).await? {
                if !hits.iter().any(|x| x.url == h.url) {
                    hits.push(h);
                }
            }
        }
        hits.truncate(params.limit.max(1));
        Ok(hits)
    }

    /// One page of results (`page` starts at 1).
    pub async fn search_page(
        &self,
        params: &PeopleSearchParams,
        page: u32,
    ) -> Result<Vec<PersonHit>> {
        let url = search_url(
            &self.origin,
            "people",
            &params.keywords,
            &params.extra(),
            page,
        );
        let raw = load_search_page(&self.base, &url, "/in/").await?;
        let hits = person_hits(&raw);
        info!(
            "People search \"{}\"{} (page {page}): {} result(s)",
            params.keywords,
            params
                .company_id
                .map(|id| format!(" [company {id}]"))
                .unwrap_or_default(),
            hits.len()
        );
        self.base
            .progress(
                &format!(
                    "People search \"{}\": {} result(s)",
                    params.keywords,
                    hits.len()
                ),
                60,
            )
            .await;
        Ok(hits)
    }
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
                    url: Some(
                        "https://www.linkedin.com/in/j%C3%A9r%C3%B4me-d-5e/?miniProfileUrn=x"
                            .into(),
                    ),
                    name: "Jérôme D".into(),
                    lines: vec!["Responsable @Acme".into(), "Toulon".into()],
                    followers: None,
                },
                RawSearchItem {
                    url: None,
                    name: "Membre LinkedIn".into(),
                    lines: vec!["Chargée".into()],
                    followers: None,
                },
            ],
            ..Default::default()
        };
        let h = person_hits(&page);
        assert_eq!(h.len(), 1);
        assert_eq!(
            h[0].url,
            "https://www.linkedin.com/in/j%C3%A9r%C3%B4me-d-5e/"
        );
        assert_eq!(h[0].slug, "jérôme-d-5e");
        assert_eq!(h[0].headline.as_deref(), Some("Responsable @Acme"));
    }

    #[test]
    fn params() {
        let mut p = PeopleSearchParams::new("Jean Martin");
        p.company_id = Some(42);
        p.title = Some("CTO".into());
        assert_eq!(
            p.extra(),
            vec![
                ("currentCompany", "[\"42\"]".to_string()),
                ("titleFreeText", "CTO".to_string())
            ]
        );
    }
}
