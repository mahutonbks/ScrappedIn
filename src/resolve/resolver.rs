//! Browser-driven resolver: runs the searches, verifies candidates, scores
//! them and caches the outcome.

use std::collections::HashSet;
use std::time::Duration;

use tracing::{debug, info};

use super::score::{CompanyEvidence, headline_mentions_company, score_company, score_person};
use super::{
    Candidate, CompanyQuery, CompanyRef, PersonQuery, Resolution, ResolveCache, ResolveConfig,
    decide,
};
use crate::core::page::Page;
use crate::core::utils::jitter;
use crate::error::Result;
use crate::scrapers::{
    CompanyHit, CompanyId, CompanyScraper, CompanySearch, IdSource, LINKEDIN_ORIGIN, PeopleSearch,
    PeopleSearchParams, PersonHit, rebase_origin,
};

/// Resolves names to LinkedIn URLs. See the [module docs](super).
///
/// ```no_run
/// # async fn demo(page: linkedin_scraper::Page) -> linkedin_scraper::Result<()> {
/// use linkedin_scraper::resolve::{CompanyQuery, ResolveCache, ResolveConfig, Resolver};
/// let mut r = Resolver::new(page, ResolveConfig::default())
///     .with_cache(ResolveCache::load("resolutions.json")?);
/// let q = CompanyQuery { domain: Some("alchimistes.co".into()), ..CompanyQuery::new("Les Alchimistes") };
/// if let Some(best) = r.resolve_company(&q).await?.best() {
///     println!("{} ({:.2})", best.item.url, best.score);
/// }
/// # Ok(()) }
/// ```
pub struct Resolver {
    page: Page,
    cfg: ResolveConfig,
    origin: String,
    cache: ResolveCache,
    refresh: bool,
    pages_loaded: u32,
    /// The next page opens without the pause (start of a new unit of work).
    fresh: bool,
}

/// People found by one strategy, with whether their company is trusted.
type Pool = Vec<(PersonHit, bool)>;

impl Resolver {
    pub fn new(page: Page, config: ResolveConfig) -> Self {
        Resolver {
            page,
            cfg: config,
            origin: LINKEDIN_ORIGIN.to_string(),
            cache: ResolveCache::in_memory(),
            refresh: false,
            pages_loaded: 0,
            fresh: true,
        }
    }

    /// Uses another origin than `https://www.linkedin.com` (tests).
    pub fn with_base_url(mut self, origin: impl Into<String>) -> Self {
        self.origin = origin.into().trim_end_matches('/').to_string();
        self
    }

    /// Persistent cache (default: in memory only).
    pub fn with_cache(mut self, cache: ResolveCache) -> Self {
        self.cache = cache;
        self
    }

    /// Ignore cached resolutions (manual ones excepted) and search again.
    pub fn with_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    pub fn config(&self) -> &ResolveConfig {
        &self.cfg
    }

    pub fn cache(&self) -> &ResolveCache {
        &self.cache
    }

    /// Pages opened so far (searches + `/about/` checks + company pages).
    pub fn pages_loaded(&self) -> u32 {
        self.pages_loaded
    }

    /// The next page is the first of a new unit of work: the caller already
    /// paused before it, so the resolver does not pause again.
    pub fn reset_pace(&mut self) {
        self.fresh = true;
    }

    /// Pause before every page but the first of a unit (`ResolveConfig::pause_secs`).
    async fn pace(&mut self) {
        if std::mem::take(&mut self.fresh) {
            self.pages_loaded += 1;
            return;
        }
        if let Some((min, max)) = self.cfg.pause_secs {
            let secs = jitter(min, max.max(min));
            debug!("Resolver pause {secs:.1}s");
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
        }
        self.pages_loaded += 1;
    }

    fn company_search(&self) -> CompanySearch {
        CompanySearch::new(self.page.clone()).with_base_url(self.origin.clone())
    }

    fn people_search(&self) -> PeopleSearch {
        PeopleSearch::new(self.page.clone()).with_base_url(self.origin.clone())
    }

    // ------------------------------------------------------------------
    // Companies
    // ------------------------------------------------------------------

    pub async fn resolve_company(&mut self, q: &CompanyQuery) -> Result<Resolution<CompanyHit>> {
        if let Some(r) = self.cache.get_company(q, self.refresh) {
            debug!("Company \"{}\" from cache: {}", q.name, r.status());
            return Ok(r);
        }
        let r = self.resolve_company_uncached(q).await?;
        info!(
            "Company \"{}\" → {}",
            q.name,
            describe(&r, |h| h.url.as_str())
        );
        self.cache.put_company(q, &r);
        Ok(r)
    }

    async fn resolve_company_uncached(
        &mut self,
        q: &CompanyQuery,
    ) -> Result<Resolution<CompanyHit>> {
        self.pace().await;
        let mut hits = self.company_search().search_page(&q.name, 1).await?;
        let close = |hits: &[CompanyHit]| {
            hits.iter()
                .any(|h| super::score::company_name_similarity(&q.name, &h.name) >= 0.9)
        };
        if self.cfg.max_search_pages >= 2 && hits.len() >= 10 && !close(&hits) {
            self.pace().await;
            for h in self.company_search().search_page(&q.name, 2).await? {
                if !hits.iter().any(|x| x.url == h.url) {
                    hits.push(h);
                }
            }
        }
        let mut cands = score_all_companies(q, hits);

        // Open /about/ of the best candidates when a hint needs it: always for
        // a domain (strongest signal), else only to settle an ambiguity.
        let verify = q.domain.is_some()
            || ((q.location.is_some() || q.industry.is_some())
                && decide(cands.clone(), &self.cfg).best().is_none());
        if verify {
            let n = self.cfg.verify_top;
            for cand in cands.iter_mut().take(n) {
                if cand.score < 0.25 {
                    continue;
                }
                self.pace().await;
                let url = rebase_origin(&self.origin, &cand.item.url);
                if let Some(d) = CompanyScraper::new(self.page.clone())
                    .about_details(&url)
                    .await?
                {
                    let h = &mut cand.item;
                    h.website = d.company.website;
                    h.industry = d.company.industry;
                    h.headquarters = d.company.headquarters;
                    h.company_id = d.company_id;
                    let (s, r) = score_company(q, h);
                    cand.score = s;
                    cand.reasons = r;
                    cand.reasons.push("vérifié sur la page /about/".into());
                    // A confirmed domain settles it: no need to open more pages.
                    if q.domain.is_some() && s >= self.cfg.accept_score {
                        break;
                    }
                }
            }
        }
        Ok(decide(cands, &self.cfg))
    }

    // ------------------------------------------------------------------
    // People
    // ------------------------------------------------------------------

    pub async fn resolve_person(&mut self, q: &PersonQuery) -> Result<Resolution<PersonHit>> {
        if let Some(r) = self.cache.get_person(q, self.refresh) {
            debug!("Person \"{}\" from cache: {}", q.name, r.status());
            return Ok(r);
        }
        let r = self.resolve_person_uncached(q).await?;
        info!(
            "Person \"{}\" → {}",
            q.name,
            describe(&r, |h| h.url.as_str())
        );
        self.cache.put_person(q, &r);
        Ok(r)
    }

    /// Company name + id for a person query, and notes for `reasons`.
    async fn company_context(
        &mut self,
        q: &PersonQuery,
    ) -> Result<(Option<String>, Option<CompanyId>, Vec<String>)> {
        let mut notes = Vec::new();
        match &q.company {
            None => Ok((None, None, notes)),
            Some(CompanyRef::Url(u)) => {
                self.pace().await;
                let ev = self.company_search().company_id_evidence(u).await?;
                let name = ev.name.clone().or_else(|| {
                    crate::scrapers::canonical_linkedin_url(u, "company")
                        .map(|(_, slug)| slug.replace('-', " "))
                });
                Ok((name, ev.pick(), notes))
            }
            Some(CompanyRef::Name(cq)) => match self.resolve_company(cq).await? {
                Resolution::Resolved { best, .. } => {
                    notes.push(format!(
                        "entreprise résolue: {} ({:.2})",
                        best.item.url, best.score
                    ));
                    let id = match best.item.company_id {
                        Some(id) => Some(id),
                        None => {
                            self.pace().await;
                            self.company_search()
                                .company_id_evidence(&best.item.url)
                                .await?
                                .pick()
                        }
                    };
                    Ok((Some(best.item.name.clone()), id, notes))
                }
                other => {
                    notes.push(format!(
                        "entreprise « {} » non résolue ({}): recherche sans filtre currentCompany",
                        cq.name,
                        other.status()
                    ));
                    Ok((Some(cq.name.clone()), None, notes))
                }
            },
        }
    }

    async fn people(
        &mut self,
        keywords: String,
        company_id: Option<u64>,
    ) -> Result<Vec<PersonHit>> {
        self.pace().await;
        let params = PeopleSearchParams {
            company_id,
            ..PeopleSearchParams::new(keywords)
        };
        self.people_search().search_page(&params, 1).await
    }

    async fn resolve_person_uncached(&mut self, q: &PersonQuery) -> Result<Resolution<PersonHit>> {
        let (company_name, company_id, mut notes) = self.company_context(q).await?;
        let budget = u32::from(self.cfg.max_search_pages.max(1));
        let mut searches = 0u32;
        let mut pool: Pool = Vec::new();
        let mut trusted_id = None;

        match company_id {
            Some(CompanyId {
                id,
                source: IdSource::HeaderLink,
            }) => {
                let hits = self.people(q.name.clone(), Some(id)).await?;
                searches += 1;
                notes.push(format!(
                    "recherche filtrée currentCompany={id} (ID sûr: lien de l'en-tête)"
                ));
                pool.extend(hits.into_iter().map(|h| (h, true)));
                trusted_id = Some(id);
            }
            Some(CompanyId {
                id,
                source: IdSource::UrnMajority,
            }) => {
                // Uncertain id: run both searches and compare them.
                let cname = company_name.clone().unwrap_or_default();
                let filtered = self.people(q.name.clone(), Some(id)).await?;
                let plain = if budget >= 2 {
                    self.people(format!("{} {}", q.name, cname).trim().to_string(), None)
                        .await?
                } else {
                    Vec::new()
                };
                searches += 1 + u32::from(budget >= 2);
                let plain_urls: HashSet<&str> = plain.iter().map(|h| h.url.as_str()).collect();
                let common = filtered
                    .iter()
                    .filter(|h| plain_urls.contains(h.url.as_str()))
                    .count();
                let mention = filtered
                    .iter()
                    .filter(|h| {
                        headline_mentions_company(h.headline.as_deref().unwrap_or(""), &cname)
                    })
                    .count();
                let confirmed = !filtered.is_empty() && (common > 0 || mention > 0);
                let note = format!(
                    "ID {id} incertain (URN majoritaire), comparaison: {} filtré(s), {} sans filtre, {} commun(s), {} mentionnant « {cname} » → ID {}",
                    filtered.len(),
                    plain.len(),
                    common,
                    mention,
                    if confirmed { "confirmé" } else { "écarté" }
                );
                info!("{note}");
                notes.push(note);
                if confirmed {
                    trusted_id = Some(id);
                    pool.extend(filtered.into_iter().map(|h| (h, true)));
                }
                for h in plain {
                    if !pool.iter().any(|(x, _)| x.url == h.url) {
                        pool.push((h, false));
                    }
                }
            }
            None => {
                let first = match &company_name {
                    Some(c) => format!("{} {c}", q.name),
                    None => q.name.clone(),
                };
                let hits = self.people(first, None).await?;
                searches += 1;
                if hits.is_empty() && company_name.is_some() && searches < budget {
                    notes.push(
                        "aucun résultat avec le nom de l'entreprise: recherche par nom seul".into(),
                    );
                    let hits = self.people(q.name.clone(), None).await?;
                    searches += 1;
                    pool.extend(hits.into_iter().map(|h| (h, false)));
                } else {
                    pool.extend(hits.into_iter().map(|h| (h, false)));
                }
            }
        }

        let score_pool = |pool: &Pool, notes: &[String]| -> Vec<Candidate<PersonHit>> {
            pool.iter()
                .map(|(h, trusted)| {
                    let ev = company_name.as_ref().map(|n| CompanyEvidence {
                        name: n.clone(),
                        filtered: *trusted,
                    });
                    let (score, mut reasons) = score_person(q, h, ev.as_ref());
                    reasons.extend(notes.iter().cloned());
                    Candidate {
                        item: h.clone(),
                        score,
                        reasons,
                    }
                })
                .collect()
        };
        let mut res = decide(score_pool(&pool, &notes), &self.cfg);

        // Narrowing search with the title words, only for crowded ambiguous
        // results and while the page budget allows it.
        if let (Resolution::Ambiguous { .. }, Some(title)) = (&res, q.title.as_deref())
            && pool.len() >= 10
            && searches < budget
        {
            notes.push(format!("recherche affinée « {} {title} »", q.name));
            let hits = self
                .people(format!("{} {title}", q.name), trusted_id)
                .await?;
            for h in hits {
                if !pool.iter().any(|(x, _)| x.url == h.url) {
                    pool.push((h, trusted_id.is_some()));
                }
            }
            res = decide(score_pool(&pool, &notes), &self.cfg);
        }
        Ok(res)
    }
}

fn score_all_companies(q: &CompanyQuery, hits: Vec<CompanyHit>) -> Vec<Candidate<CompanyHit>> {
    let mut v: Vec<_> = hits
        .into_iter()
        .map(|h| {
            let (score, reasons) = score_company(q, &h);
            Candidate {
                item: h,
                score,
                reasons,
            }
        })
        .collect();
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    v
}

fn describe<T>(r: &Resolution<T>, url: impl Fn(&T) -> &str) -> String {
    match r {
        Resolution::Resolved { best, .. } => {
            format!("resolved {} ({:.2})", url(&best.item), best.score)
        }
        Resolution::Ambiguous { candidates } => {
            format!("ambiguous ({} candidates)", candidates.len())
        }
        Resolution::NotFound => "not found".into(),
    }
}
