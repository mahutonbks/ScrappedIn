//! Shared helpers for LinkedIn search result pages (companies and people).
//!
//! Observed DOM (2026-11, "SDUI" UI): one `[role="listitem"]` per result;
//! inside it, the result name is the `/company/…` or `/in/…` link placed in a
//! `<p>`, and the sibling `<p>`s of that name block are the subtitle (industry
//! or headline) then the location. A follower count is the last `<p>` outside
//! that block starting with a digit. Private members have no `/in/` link.
//!
//! JS only collects these texts; parsing, URL building and limit detection
//! are pure Rust functions, unit-tested below.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// What the extraction JS returns for one result.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawSearchItem {
    pub url: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub lines: Vec<String>,
    pub followers: Option<String>,
}

/// What the extraction JS returns for a page.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawSearchPage {
    #[serde(default)]
    pub items: Vec<RawSearchItem>,
    /// Start of the main text (used only to detect the search limit page).
    #[serde(default)]
    pub text: String,
    /// A Premium upsell link of the "commercial use limit" kind is present.
    #[serde(default)]
    pub limit_link: bool,
}

/// JS function `(linkSegment) => RawSearchPage`, where `linkSegment` is
/// `"/company/"` or `"/in/"`.
const SEARCH_FN_JS: &str = r#"((seg) => {
  const t = s => (s || '').replace(/\s+/g, ' ').trim();
  const main = document.querySelector('main') || document.body;
  const items = [...main.querySelectorAll('[role="listitem"]')].filter(li => !li.closest('aside'));
  const out = items.map(li => {
    const link = [...li.querySelectorAll('p a[href]')]
      .find(a => (a.getAttribute('href') || '').includes(seg) && t(a.innerText));
    if (!link) {
      const p = li.querySelector('p');
      return { url: null, name: p ? t(p.innerText) : '', lines: [], followers: null };
    }
    const p = link.closest('p');
    const box = p.parentElement;
    const lines = [...box.querySelectorAll('p')].filter(x => x !== p).map(x => t(x.innerText)).filter(Boolean);
    const rest = [...li.querySelectorAll('p')].filter(x => !box.contains(x)).map(x => t(x.innerText));
    const followers = rest.reverse().find(x => /^\d/.test(x)) || null;
    return { url: link.href, name: t(link.innerText), lines, followers };
  });
  const limitLink = !!document.querySelector(
    'a[href*="commercial_use_limit"], a[href*="commercial-use-limit"], a[href*="search_limit"]');
  return { items: out, text: t((main.innerText || '').slice(0, 3000)), limit_link: limitLink };
})"#;

/// JS expression extracting the results of a search page.
pub(crate) fn search_page_js(link_segment: &str) -> String {
    format!("{SEARCH_FN_JS}({link_segment:?})")
}

/// Raw evidence for a company's numeric id, collected on its page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyIdEvidence {
    /// Id from the top-card "N employees" link
    /// (`currentCompany=["<id>"]` + `origin=COMPANY_PAGE_CANNED_SEARCH`,
    /// excluding `schoolFilter` links).
    pub header: Option<u64>,
    /// `urn:li:fsd_company:<id>` / `urn:li:company:<id>` occurrence counts
    /// in the page HTML.
    #[serde(default)]
    pub urn_counts: HashMap<String, u32>,
    /// Company name shown on the page (`h1`).
    #[serde(default)]
    pub name: Option<String>,
}

pub(crate) const COMPANY_ID_JS: &str = r#"(() => {
  const dec = h => { try { return decodeURIComponent(h); } catch (e) { return h; } };
  const header = [...document.querySelectorAll('a[href*="currentCompany"]')]
    .map(a => dec(a.getAttribute('href') || ''))
    .filter(h => h.includes('COMPANY_PAGE_CANNED_SEARCH') && !h.includes('schoolFilter'))
    .map(h => (h.match(/currentCompany=\["?(\d+)/) || [])[1])
    .find(Boolean) || null;
  const counts = {};
  for (const m of document.documentElement.innerHTML.matchAll(/urn:li:(?:fsd_)?company:(\d+)/g)) {
    counts[m[1]] = (counts[m[1]] || 0) + 1;
  }
  const h1 = document.querySelector('main h1, h1');
  return { header: header ? Number(header) : null, urn_counts: counts,
           name: h1 ? (h1.innerText || '').trim() || null : null };
})()"#;

/// Where a company id comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdSource {
    /// The top-card "employees" link: reliable.
    HeaderLink,
    /// Most frequent company URN in the page data: a weaker clue (related
    /// companies appear too), to be confirmed by comparing searches.
    UrnMajority,
}

/// A numeric company id and how it was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyId {
    pub id: u64,
    pub source: IdSource,
}

impl CompanyIdEvidence {
    /// The id by the header link if present, else the URN majority — only
    /// when it is strictly more frequent than any other URN.
    pub fn pick(&self) -> Option<CompanyId> {
        if let Some(id) = self.header {
            return Some(CompanyId {
                id,
                source: IdSource::HeaderLink,
            });
        }
        self.urn_majority().map(|id| CompanyId {
            id,
            source: IdSource::UrnMajority,
        })
    }

    /// Most frequent URN id, if unique.
    pub fn urn_majority(&self) -> Option<u64> {
        let mut v: Vec<(&String, &u32)> = self.urn_counts.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        match (v.first(), v.get(1)) {
            (Some((id, n)), Some((_, m))) if n > m => id.parse().ok(),
            (Some((id, _)), None) => id.parse().ok(),
            _ => None,
        }
    }
}

/// Percent-encodes a query value (spaces as `%20`).
pub(crate) fn encode(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// `{base}/search/results/{vertical}/?keywords=…[&k=v…][&page=N]`.
pub(crate) fn search_url(
    base: &str,
    vertical: &str,
    keywords: &str,
    extra: &[(&str, String)],
    page: u32,
) -> String {
    let mut u = format!(
        "{}/search/results/{vertical}/?keywords={}",
        base.trim_end_matches('/'),
        encode(keywords)
    );
    for (k, v) in extra {
        let _ = write!(u, "&{k}={}", encode(v));
    }
    if page > 1 {
        let _ = write!(u, "&page={page}");
    }
    u
}

/// `https://…/in/<slug>/?x` (or `/company/`) → canonical
/// `https://www.linkedin.com/<kind>/<slug>/` and the decoded slug.
pub(crate) fn canonical_url(href: &str, kind: &str) -> Option<(String, String)> {
    let u = url::Url::parse(href).ok()?;
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let i = segs.iter().position(|s| *s == kind)?;
    let raw_slug = segs.get(i + 1)?;
    let slug = super::percent_decode(raw_slug);
    Some((format!("https://www.linkedin.com/{kind}/{raw_slug}/"), slug))
}

/// Texts that identify LinkedIn's search limit page (EN/FR/ES/DE). Not
/// observed on a real account yet: kept broad, and only applied when the page
/// has no result.
const LIMIT_PHRASES: &[&str] = &[
    "commercial use limit",
    "monthly limit for profile searches",
    "reached the monthly limit",
    "limite d'utilisation commerciale",
    "limite d’utilisation commerciale",
    "limite mensuelle de recherche",
    "límite de uso comercial",
    "limite de búsqueda",
    "limit für die kommerzielle nutzung",
];

/// `true` if the page is the "commercial use limit" page.
pub(crate) fn is_search_limit(page: &RawSearchPage) -> bool {
    if !page.items.is_empty() {
        return false;
    }
    let text = page.text.to_lowercase();
    page.limit_link || LIMIT_PHRASES.iter().any(|p| text.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            search_url(
                "https://www.linkedin.com",
                "companies",
                "Les Alchimistes",
                &[],
                1
            ),
            "https://www.linkedin.com/search/results/companies/?keywords=Les%20Alchimistes"
        );
        assert_eq!(
            search_url(
                "https://www.linkedin.com/",
                "people",
                "Jean Martin",
                &[("currentCompany", "[\"123\"]".into())],
                2
            ),
            "https://www.linkedin.com/search/results/people/?keywords=Jean%20Martin&currentCompany=%5B%22123%22%5D&page=2"
        );
        assert_eq!(encode("a+b&c"), "a%2Bb%26c");
    }

    #[test]
    fn canonical() {
        assert_eq!(
            canonical_url(
                "https://www.linkedin.com/in/j%C3%A9r%C3%B4me-x-5e/?miniProfileUrn=abc",
                "in"
            ),
            Some((
                "https://www.linkedin.com/in/j%C3%A9r%C3%B4me-x-5e/".to_string(),
                "jérôme-x-5e".to_string()
            ))
        );
        assert_eq!(
            canonical_url(
                "http://127.0.0.1:9/fake.linkedin.com/company/acme/about/",
                "company"
            )
            .map(|x| x.0),
            Some("https://www.linkedin.com/company/acme/".to_string())
        );
        assert_eq!(
            canonical_url("https://www.linkedin.com/search/results/people/", "in"),
            None
        );
    }

    #[test]
    fn company_id() {
        let mut e = CompanyIdEvidence {
            header: Some(12345678),
            ..Default::default()
        };
        e.urn_counts.insert("999001".into(), 9);
        assert_eq!(
            e.pick(),
            Some(CompanyId {
                id: 12345678,
                source: IdSource::HeaderLink
            })
        );
        e.header = None;
        e.urn_counts.insert("12345678".into(), 4);
        assert_eq!(
            e.pick(),
            Some(CompanyId {
                id: 999001,
                source: IdSource::UrnMajority
            })
        );
        e.urn_counts.insert("12345678".into(), 9);
        assert_eq!(e.pick(), None, "tie → no id");
        assert_eq!(CompanyIdEvidence::default().pick(), None);
    }

    #[test]
    fn limit() {
        let mut p = RawSearchPage {
            text: "Vous avez atteint la limite d’utilisation commerciale de la recherche".into(),
            ..Default::default()
        };
        assert!(is_search_limit(&p));
        p.text = "Aucun résultat".into();
        assert!(!is_search_limit(&p));
        p.limit_link = true;
        assert!(is_search_limit(&p));
        p.items.push(RawSearchItem::default());
        assert!(!is_search_limit(&p), "results present → not the limit page");
    }
}
