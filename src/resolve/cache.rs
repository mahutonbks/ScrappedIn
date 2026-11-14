//! JSON cache of resolutions (`resolutions.json`).
//!
//! Key = normalized query ([`company_key`], [`person_key`]); value = the
//! [`Resolution`] and its date. A cached query is never searched again unless
//! the resolver runs with `refresh`.
//!
//! **Manual validation**: edit an entry and either
//! * set `"manual_url": "https://www.linkedin.com/in/…/"` — the query then
//!   resolves to that URL (score 1.0, "validé à la main"), or
//! * edit `"resolution"` yourself and set `"manual": true`.
//!
//! Manual entries are respected even with `refresh`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use super::normalize::{PersonName, domain_of, normalize_company, normalize_text};
use super::{Candidate, CompanyQuery, CompanyRef, PersonQuery, Query, Resolution};
use crate::error::Result;
use crate::scrapers::{CompanyHit, PersonHit};

/// Cache key of a company query.
pub fn company_key(q: &CompanyQuery) -> String {
    format!(
        "company|{}|domain={}|loc={}|ind={}",
        normalize_company(&q.name),
        q.domain.as_deref().and_then(domain_of).unwrap_or_default(),
        normalize_text(q.location.as_deref().unwrap_or("")),
        normalize_text(q.industry.as_deref().unwrap_or(""))
    )
}

/// Cache key of a person query (name order-insensitive).
pub fn person_key(q: &PersonQuery) -> String {
    let company = match &q.company {
        None => String::new(),
        Some(CompanyRef::Url(u)) => format!(
            "url:{}",
            u.trim()
                .trim_end_matches('/')
                .to_lowercase()
                .replace("http://", "https://")
        ),
        Some(CompanyRef::Name(c)) => company_key(c),
    };
    format!(
        "person|{}|company={}|title={}|loc={}",
        PersonName::parse(&q.name).key(),
        company,
        normalize_text(q.title.as_deref().unwrap_or("")),
        normalize_text(q.location.as_deref().unwrap_or(""))
    )
}

/// Cache key of any query.
pub fn query_key(q: &Query) -> String {
    match q {
        Query::Company(c) => company_key(c),
        Query::Person(p) => person_key(p),
    }
}

/// One cached resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub query: Value,
    pub resolution: Value,
    pub resolved_at: String,
    /// Forced choice, written by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_url: Option<String>,
    /// The resolution was edited by hand: keep it even with `refresh`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

impl CacheEntry {
    pub fn is_manual(&self) -> bool {
        self.manual || self.manual_url.is_some()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, CacheEntry>,
}

/// Resolution cache, optionally backed by a file (saved after every change).
#[derive(Debug, Clone, Default)]
pub struct ResolveCache {
    path: Option<PathBuf>,
    file: CacheFile,
}

impl ResolveCache {
    /// Cache kept in memory only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Loads `path` (an absent file is an empty cache).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => CacheFile::default(),
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CacheFile::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(ResolveCache {
            path: Some(path),
            file,
        })
    }

    /// Writes the cache file (atomically: temp file then rename).
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut file = self.file.clone();
        file.version = 1;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.file.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.file.entries.is_empty()
    }

    pub fn entry(&self, key: &str) -> Option<&CacheEntry> {
        self.file.entries.get(key)
    }

    /// Fingerprint of an entry (changes whenever it is edited), used by the
    /// batch to retry ambiguous lines only after the cache was modified.
    pub fn stamp(&self, key: &str) -> Option<String> {
        let e = self.file.entries.get(key)?;
        let s = serde_json::to_string(e).ok()?;
        // FNV-1a 64
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        Some(format!("{h:016x}"))
    }

    fn get<T: DeserializeOwned>(
        &self,
        key: &str,
        refresh: bool,
        manual_item: impl Fn(&str) -> Option<T>,
    ) -> Option<Resolution<T>> {
        let e = self.file.entries.get(key)?;
        if let Some(url) = &e.manual_url {
            let item = manual_item(url)?;
            return Some(Resolution::Resolved {
                best: Candidate {
                    item,
                    score: 1.0,
                    reasons: vec!["validé à la main (manual_url)".into()],
                },
                others: vec![],
            });
        }
        if refresh && !e.manual {
            return None;
        }
        match serde_json::from_value(e.resolution.clone()) {
            Ok(r) => Some(r),
            Err(err) => {
                warn!("Ignoring unreadable cache entry {key}: {err}");
                None
            }
        }
    }

    fn put<T: Serialize, Q: Serialize>(&mut self, key: String, query: &Q, r: &Resolution<T>) {
        let entry = CacheEntry {
            query: serde_json::to_value(query).unwrap_or(Value::Null),
            resolution: serde_json::to_value(r).unwrap_or(Value::Null),
            resolved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            manual_url: None,
            manual: false,
        };
        self.file.entries.insert(key, entry);
        if let Err(e) = self.save() {
            warn!("Could not save resolution cache: {e}");
        }
    }

    pub fn get_company(&self, q: &CompanyQuery, refresh: bool) -> Option<Resolution<CompanyHit>> {
        self.get(&company_key(q), refresh, |url| {
            let (url, slug) = crate::scrapers::canonical_linkedin_url(url, "company")?;
            Some(CompanyHit {
                name: q.name.clone(),
                url,
                slug,
                ..Default::default()
            })
        })
    }

    pub fn put_company(&mut self, q: &CompanyQuery, r: &Resolution<CompanyHit>) {
        self.put(company_key(q), q, r);
    }

    pub fn get_person(&self, q: &PersonQuery, refresh: bool) -> Option<Resolution<PersonHit>> {
        self.get(&person_key(q), refresh, |url| {
            let (url, slug) = crate::scrapers::canonical_linkedin_url(url, "in")?;
            Some(PersonHit {
                name: q.name.clone(),
                url,
                slug,
                ..Default::default()
            })
        })
    }

    pub fn put_person(&mut self, q: &PersonQuery, r: &Resolution<PersonHit>) {
        self.put(person_key(q), q, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{ResolveConfig, decide};

    #[test]
    fn keys() {
        let a = CompanyQuery {
            name: "Les Alchimistes SAS".into(),
            domain: Some("https://www.alchimistes.fr/".into()),
            ..Default::default()
        };
        let b = CompanyQuery {
            name: "les alchimistes".into(),
            domain: Some("alchimistes.fr".into()),
            ..Default::default()
        };
        assert_eq!(company_key(&a), company_key(&b));
        assert_eq!(
            company_key(&a),
            "company|alchimistes|domain=alchimistes.fr|loc=|ind="
        );

        let p1 = PersonQuery {
            name: "Jean Martin".into(),
            company: Some(CompanyRef::Name(CompanyQuery::new("Acme SA"))),
            ..Default::default()
        };
        let p2 = PersonQuery {
            name: "MARTIN Jean".into(),
            company: Some(CompanyRef::Name(CompanyQuery::new("acme"))),
            ..Default::default()
        };
        assert_eq!(person_key(&p1), person_key(&p2));
        let p3 = PersonQuery {
            title: Some("CTO".into()),
            ..p1.clone()
        };
        assert_ne!(person_key(&p1), person_key(&p3));
    }

    #[test]
    fn roundtrip_refresh_and_manual() {
        let dir = std::env::temp_dir().join(format!("lkcache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resolutions.json");
        let _ = std::fs::remove_file(&path);

        let q = CompanyQuery::new("Acme Compost");
        let hit = CompanyHit {
            name: "Acme Compost".into(),
            url: "https://www.linkedin.com/company/acme-compost/".into(),
            slug: "acme-compost".into(),
            ..Default::default()
        };
        let r = decide(
            vec![Candidate {
                item: hit,
                score: 0.7,
                reasons: vec![],
            }],
            &ResolveConfig::default(),
        );
        assert_eq!(r.status(), "ambiguous");

        let mut c = ResolveCache::load(&path).unwrap();
        assert!(c.is_empty());
        c.put_company(&q, &r);
        let stamp1 = c.stamp(&company_key(&q)).unwrap();

        let c = ResolveCache::load(&path).unwrap();
        assert_eq!(c.get_company(&q, false), Some(r.clone()));
        assert_eq!(
            c.get_company(&q, true),
            None,
            "refresh ignores automatic entries"
        );

        // Manual choice, edited in the file.
        let mut json: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["entries"][company_key(&q)]["manual_url"] =
            "https://www.linkedin.com/company/acme-compost-sas/?x=1".into();
        std::fs::write(&path, json.to_string()).unwrap();
        let c = ResolveCache::load(&path).unwrap();
        assert_ne!(
            c.stamp(&company_key(&q)).unwrap(),
            stamp1,
            "edit changes the stamp"
        );
        for refresh in [false, true] {
            let got = c.get_company(&q, refresh).unwrap();
            assert_eq!(
                got.best().unwrap().item.url,
                "https://www.linkedin.com/company/acme-compost-sas/"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
