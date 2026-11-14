//! Name → LinkedIn URL resolution (opt-in).
//!
//! Given a [`CompanyQuery`] or a [`PersonQuery`] (a name plus optional hints),
//! [`Resolver`] runs LinkedIn's own search pages in the logged-in browser,
//! scores every hit with pure, documented functions ([`score`]) and returns a
//! [`Resolution`]:
//!
//! * [`Resolution::Resolved`] when the best candidate scores at least
//!   [`ResolveConfig::accept_score`] **and** beats the runner-up by
//!   [`ResolveConfig::min_margin`];
//! * [`Resolution::Ambiguous`] when some candidates reach
//!   [`ResolveConfig::min_score`] but none is clearly the right one;
//! * [`Resolution::NotFound`] otherwise.
//!
//! The first search result is never picked by default: a wrong match would
//! pollute the knowledge graph, so `Ambiguous` is preferred to a guess.

pub mod cache;
pub mod input;
pub mod normalize;
mod resolver;
pub mod score;

use serde::{Deserialize, Serialize};

pub use cache::ResolveCache;
pub use input::{Query, parse_query_line};
pub use resolver::Resolver;

/// A company to find.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyQuery {
    pub name: String,
    /// Website domain ("acme.fr" or a full URL): the strongest signal,
    /// checked on the `/about/` page of the best candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub industry: Option<String>,
}

impl CompanyQuery {
    pub fn new(name: impl Into<String>) -> Self {
        CompanyQuery {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// A person to find.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonQuery {
    pub name: String,
    /// Current company, by LinkedIn URL or by name (resolved first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<CompanyRef>,
    /// Job title words; used for scoring only (LinkedIn ignores the
    /// `titleFreeText` URL filter), plus a narrowing search when ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

impl PersonQuery {
    pub fn new(name: impl Into<String>) -> Self {
        PersonQuery {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// How the company of a [`PersonQuery`] is given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyRef {
    Url(String),
    Name(CompanyQuery),
}

/// A scored candidate. `reasons` explains the score in plain words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate<T> {
    pub item: T,
    pub score: f64,
    pub reasons: Vec<String>,
}

/// Outcome of a resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Resolution<T> {
    Resolved {
        best: Candidate<T>,
        others: Vec<Candidate<T>>,
    },
    Ambiguous {
        candidates: Vec<Candidate<T>>,
    },
    NotFound,
}

impl<T> Resolution<T> {
    /// `"resolved"`, `"ambiguous"` or `"not_found"`.
    pub fn status(&self) -> &'static str {
        match self {
            Resolution::Resolved { .. } => "resolved",
            Resolution::Ambiguous { .. } => "ambiguous",
            Resolution::NotFound => "not_found",
        }
    }

    pub fn best(&self) -> Option<&Candidate<T>> {
        match self {
            Resolution::Resolved { best, .. } => Some(best),
            _ => None,
        }
    }

    /// All candidates, best first.
    pub fn candidates(&self) -> Vec<&Candidate<T>> {
        match self {
            Resolution::Resolved { best, others } => {
                std::iter::once(best).chain(others.iter()).collect()
            }
            Resolution::Ambiguous { candidates } => candidates.iter().collect(),
            Resolution::NotFound => Vec::new(),
        }
    }
}

/// Decision thresholds and search budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveConfig {
    /// Minimum score of the chosen candidate (default 0.85).
    pub accept_score: f64,
    /// Minimum gap between the best and the second candidate (default 0.15).
    pub min_margin: f64,
    /// Minimum score for a candidate to be listed as ambiguous (default 0.50).
    pub min_score: f64,
    /// Search result pages per query (1 = 10 results). Default 2: the second
    /// page / narrowing search is only used when the first one is not enough.
    pub max_search_pages: u8,
    /// How many top company candidates get their `/about/` page opened to
    /// check the website domain (default 3).
    pub verify_top: usize,
    /// Random pause (seconds) before each page the resolver opens, except the
    /// first one. `None` = no pause (the caller paces requests).
    pub pause_secs: Option<(f64, f64)>,
}

impl Default for ResolveConfig {
    fn default() -> Self {
        ResolveConfig {
            accept_score: 0.85,
            min_margin: 0.15,
            min_score: 0.50,
            max_search_pages: 2,
            verify_top: 3,
            pause_secs: None,
        }
    }
}

/// Applies the decision rule to scored candidates (any order).
pub fn decide<T>(mut candidates: Vec<Candidate<T>>, cfg: &ResolveConfig) -> Resolution<T> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let Some(best_score) = candidates.first().map(|c| c.score) else {
        return Resolution::NotFound;
    };
    let second = candidates.get(1).map_or(0.0, |c| c.score);
    if best_score >= cfg.accept_score && best_score - second >= cfg.min_margin - 1e-9 {
        let mut it = candidates.into_iter();
        let best = it.next().expect("non-empty");
        let others = it.filter(|c| c.score >= cfg.min_score).collect();
        return Resolution::Resolved { best, others };
    }
    let listed: Vec<_> = candidates
        .into_iter()
        .filter(|c| c.score >= cfg.min_score)
        .collect();
    if listed.is_empty() {
        Resolution::NotFound
    } else {
        Resolution::Ambiguous { candidates: listed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, score: f64) -> Candidate<String> {
        Candidate {
            item: name.into(),
            score,
            reasons: vec![],
        }
    }

    #[test]
    fn decision_rule() {
        let cfg = ResolveConfig::default();
        match decide(vec![c("b", 0.60), c("a", 0.95)], &cfg) {
            Resolution::Resolved { best, others } => {
                assert_eq!(best.item, "a");
                assert_eq!(others.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        // Two homonyms with the same score: margin too small.
        let r = decide(vec![c("a", 0.95), c("b", 0.90)], &cfg);
        assert_eq!(r.status(), "ambiguous");
        assert_eq!(r.candidates().len(), 2);
        // Exactly the margin is enough.
        assert_eq!(
            decide(vec![c("a", 0.95), c("b", 0.80)], &cfg).status(),
            "resolved"
        );
        // Good margin but below accept_score.
        assert_eq!(decide(vec![c("a", 0.80)], &cfg).status(), "ambiguous");
        // Nothing above min_score.
        assert_eq!(
            decide(vec![c("a", 0.30), c("b", 0.10)], &cfg).status(),
            "not_found"
        );
        assert_eq!(
            decide(Vec::<Candidate<String>>::new(), &cfg).status(),
            "not_found"
        );
        // Ambiguous lists only candidates above min_score.
        let r = decide(vec![c("a", 0.7), c("b", 0.65), c("z", 0.2)], &cfg);
        assert_eq!(r.candidates().len(), 2);
    }

    #[test]
    fn resolution_serde() {
        let r = decide(vec![c("a", 0.95)], &ResolveConfig::default());
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["status"], "resolved");
        assert_eq!(j["best"]["item"], "a");
        let back: Resolution<String> = serde_json::from_value(j).unwrap();
        assert_eq!(back, r);
        assert_eq!(
            serde_json::to_value(Resolution::<String>::NotFound).unwrap()["status"],
            "not_found"
        );
    }
}
