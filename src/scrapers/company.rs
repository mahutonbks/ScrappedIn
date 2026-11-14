//! Company page scraper (port of `scrapers/company.py`).

use serde::Deserialize;
use tracing::{debug, info, warn};

use super::base::{BaseScraper, impl_with_callback};
use super::search_dom::{COMPANY_ID_JS, CompanyId, CompanyIdEvidence};
use crate::core::page::Page;
use crate::core::utils;
use crate::error::{Error, Result};
use crate::models::{Company, Employee, sort_by_seniority};

/// Scrapes `https://www.linkedin.com/company/<id>/` pages.
///
/// ```no_run
/// # async fn demo(browser: linkedin_scraper::BrowserManager) -> linkedin_scraper::Result<()> {
/// use linkedin_scraper::CompanyScraper;
/// let company = CompanyScraper::new(browser.page()?)
///     .scrape("https://www.linkedin.com/company/microsoft/")
///     .await?;
/// println!("{}", company.to_json()?);
/// # Ok(()) }
/// ```
pub struct CompanyScraper {
    base: BaseScraper,
    /// Maximum number of employees to collect in [`Self::scrape`] (0 = none).
    max_employees: usize,
}

impl_with_callback!(CompanyScraper);

/// Overview fields extracted from the page.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Overview {
    pub website: Option<String>,
    pub phone: Option<String>,
    pub headquarters: Option<String>,
    pub founded: Option<String>,
    pub industry: Option<String>,
    pub company_type: Option<String>,
    pub company_size: Option<String>,
    pub specialties: Option<String>,
}

impl Overview {
    fn is_empty(&self) -> bool {
        *self == Overview::default()
    }

    /// Classifies one `.org-top-card-summary-info-list__info-item` text
    /// (industry · headquarters · followers · size), in any UI language.
    pub(crate) fn apply_info_item(&mut self, text: &str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let lower = text.to_lowercase();
        const SIZE: [&str; 6] = [
            "employ",
            "salarié",
            "mitarbeiter",
            "emplead",
            "dipendent",
            "k+",
        ];
        const FOLLOWERS: [&str; 5] = ["follower", "abonné", "seguidor", "abonnent", "iscritt"];
        if text.is_empty() {
            return;
        }
        if SIZE.iter().any(|w| lower.contains(w)) {
            self.company_size.get_or_insert(text);
        } else if FOLLOWERS.iter().any(|w| lower.contains(w)) {
            // not stored in the model
        } else if text.contains(',') && !text.chars().any(|c| c.is_ascii_digit()) {
            self.headquarters.get_or_insert(text);
        } else if self.industry.is_none() {
            self.industry = Some(text);
        }
    }

    /// Classifies one `<dt>label</dt><dd>value</dd>` pair of the About page,
    /// in English, French, Spanish, German or Italian.
    pub(crate) fn apply_dt_dd(&mut self, label: &str, value: &str) {
        let label = label.trim().to_lowercase().replace('’', "'");
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let has = |words: &[&str]| words.iter().any(|w| label.contains(w));
        let slot = if has(&["website", "site web", "sitio web", "webseite", "sito web"]) {
            &mut self.website
        } else if has(&["phone", "téléphone", "telefon", "teléfono", "telefono"]) {
            &mut self.phone
        } else if has(&["headquarters", "siège", "sede", "hauptsitz", "location"]) {
            &mut self.headquarters
        } else if has(&[
            "founded",
            "fondée",
            "fondé",
            "fundada",
            "fundación",
            "gegründet",
            "fondata",
        ]) {
            &mut self.founded
        } else if has(&["industr", "secteur", "sector", "branche", "settore"]) {
            &mut self.industry
        } else if has(&["size", "taille", "tamaño", "größe", "dimensioni"]) {
            &mut self.company_size
        } else if has(&["type", "tipo", "unternehmensart"]) {
            &mut self.company_type
        } else if has(&[
            "specialt",
            "spécialisation",
            "domaines",
            "especialidades",
            "spezialgebiete",
            "specializzazion",
        ]) {
            &mut self.specialties
        } else {
            return;
        };
        *slot = Some(value.to_string());
    }

    /// Fills the fields still empty in `self` from `other`.
    fn or(self, other: Overview) -> Overview {
        Overview {
            website: self.website.or(other.website),
            phone: self.phone.or(other.phone),
            headquarters: self.headquarters.or(other.headquarters),
            founded: self.founded.or(other.founded),
            industry: self.industry.or(other.industry),
            company_type: self.company_type.or(other.company_type),
            company_size: self.company_size.or(other.company_size),
            specialties: self.specialties.or(other.specialties),
        }
    }
}

/// Result of [`CompanyScraper::about_details`].
#[derive(Debug, Clone, PartialEq)]
pub struct CompanyDetails {
    pub company: Company,
    /// Numeric id, with its source (see [`CompanyIdEvidence::pick`]).
    pub company_id: Option<CompanyId>,
    /// Raw id clues (header link and URN counts), for comparisons.
    pub id_evidence: CompanyIdEvidence,
}

/// `https://www.linkedin.com/company/acme/…` → `https://www.linkedin.com/company/acme/<sub>/`.
/// Keeps the port and any path prefix before `/company/` (local test servers).
fn company_subpage(linkedin_url: &str, sub: &str) -> Option<String> {
    let u = url::Url::parse(linkedin_url).ok()?;
    let host = u.host_str()?;
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let i = segs.iter().position(|s| *s == "company")?;
    let slug = segs.get(i + 1)?;
    let prefix: String = segs[..i].iter().map(|s| format!("/{s}")).collect();
    Some(format!(
        "{}://{host}{port}{prefix}/company/{slug}/{sub}/",
        u.scheme()
    ))
}

const EMPLOYEE_CARD: &str = ".org-people-profile-card__profile-info";
const LOAD_MORE: &str = "button.scaffold-finite-scroll__load-button";

#[derive(Debug, Deserialize)]
struct RawEmployee {
    name: String,
    headline: Option<String>,
    url: Option<String>,
}

/// Employee cards of the "People" tab. Private members have no `/in/` link.
const EXTRACT_EMPLOYEES_JS: &str = r#"(() => {
  const t = s => (s || '').replace(/\s+/g, ' ').trim();
  const scope = document.querySelector('main') || document;
  return [...scope.querySelectorAll('.org-people-profile-card__profile-info')].map(c => {
    const a = c.querySelector('a[href*="/in/"]');
    return {
      name: t(c.querySelector('.artdeco-entity-lockup__title')?.innerText),
      headline: t(c.querySelector('.artdeco-entity-lockup__subtitle')?.innerText) || null,
      url: a ? a.href.split('?')[0].replace(/\/?$/, '/') : null,
    };
  });
})()"#;

/// Content of the `/about/` page: description and `(label, value, index)`
/// rows of its `<dl>` (index = position of the `<dd>` under its `<dt>`).
#[derive(Debug, Default, Deserialize)]
struct AboutPage {
    about: Option<String>,
    rows: Vec<(String, String, usize)>,
}

const EXTRACT_ABOUT_PAGE_JS: &str = r#"(() => {
  const dl = document.querySelector('main dl');
  if (!dl) return null;
  const clean = s => (s || '').replace(/[ \t ]+/g, ' ').replace(/\n\s*\n+/g, '\n').trim();
  const p = dl.parentElement && [...dl.parentElement.querySelectorAll('p')].find(x => !dl.contains(x));
  const about = p ? clean(p.innerText) || null : null;
  const rows = [];
  let label = null, idx = 0;
  for (const el of dl.children) {
    if (el.tagName === 'DT') { label = clean((el.querySelector('h3') || el).innerText); idx = 0; continue; }
    if (el.tagName !== 'DD' || !label) continue;
    const a = el.querySelector('a[href]');
    const href = a ? a.getAttribute('href') : '';
    let v;
    if (/^https?:/i.test(href) && !/linkedin\.com/i.test(href)) v = href;
    else if (/^tel:/i.test(href)) v = href.slice(4);
    else {
      const c = el.cloneNode(true);
      c.querySelectorAll('.visually-hidden, button, [role="tooltip"]').forEach(n => n.remove());
      v = clean(c.innerText || c.textContent);
    }
    if (v) rows.push([label, v, idx]);
    idx++;
  }
  return { about, rows };
})()"#;

/// `"19 membres associés"` / `"1,234 associated members"` → 19 / 1234.
pub(crate) fn parse_leading_count(text: &str) -> Option<i64> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | ' ' | '\u{a0}' | '\u{202f}'))
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

impl CompanyScraper {
    pub fn new(page: Page) -> Self {
        Self {
            base: BaseScraper::new(page),
            max_employees: 0,
        }
    }

    /// Also collect up to `max` employees from the "People" tab during
    /// [`Self::scrape`], sorted by seniority. Each batch of ~12 people costs one
    /// "Show more" click; keep `max` reasonable.
    pub fn with_employees(mut self, max: usize) -> Self {
        self.max_employees = max;
        self
    }

    /// Collects up to `max` employees of a company ("People" tab), sorted by
    /// seniority (highest first). Private "LinkedIn Member" cards are skipped.
    pub async fn scrape_employees(&self, linkedin_url: &str, max: usize) -> Result<Vec<Employee>> {
        let b = &self.base;
        let Some(url) = company_subpage(linkedin_url, "people") else {
            return Ok(Vec::new());
        };
        b.navigate_and_wait(&url).await?;
        if !b
            .element_exists(".org-people-profile-card__profile-info", 10_000)
            .await
        {
            debug!("No employee cards on {url}");
            return Ok(Vec::new());
        }
        b.wait_and_focus(1.0).await;

        // Load more: click "Show more results" (or scroll) until enough cards.
        let (mut last, mut stalled) = (0usize, 0u32);
        loop {
            let n = b.count_elements(EMPLOYEE_CARD).await;
            if n >= max || stalled >= 2 {
                break;
            }
            stalled = if n == last { stalled + 1 } else { 0 };
            last = n;
            b.scroll_page_to_bottom(0.5, 1).await?;
            if b.count_elements(LOAD_MORE).await > 0 && !b.safe_click(LOAD_MORE, 3_000).await {
                debug!("Could not click 'show more'");
            }
            tokio::time::sleep(std::time::Duration::from_secs_f64(utils::jitter(2.0, 4.0))).await;
            b.check_rate_limit().await?;
            b.progress(&format!("Loaded {n} employee cards"), 90).await;
        }

        let raw: Vec<RawEmployee> = b.page.evaluate(EXTRACT_EMPLOYEES_JS).await?;
        let total = raw.len();
        let mut seen = std::collections::HashSet::new();
        let mut employees: Vec<Employee> = raw
            .into_iter()
            .filter(|r| !r.name.is_empty() && r.url.is_some())
            .filter(|r| seen.insert(r.url.clone()))
            .take(max)
            .map(|r| Employee::new(r.name, r.headline.filter(|h| !h.is_empty()), r.url))
            .collect();
        let hidden = total.saturating_sub(seen.len());
        if hidden > 0 {
            debug!("{hidden} private or duplicate member cards skipped");
        }
        sort_by_seniority(&mut employees);
        Ok(employees)
    }

    pub async fn scrape(&self, linkedin_url: &str) -> Result<Company> {
        let b = &self.base;
        info!("Starting company scraping: {linkedin_url}");
        b.callback.on_start("company", linkedin_url).await;

        b.navigate_and_wait(linkedin_url).await?;
        b.progress("Navigated to company page", 10).await;
        b.check_rate_limit().await?;

        let name = self.get_name().await;
        b.progress(&format!("Got company name: {name}"), 20).await;

        let mut about_us = self.get_about().await;
        b.progress("Got about section", 30).await;

        let mut o = self.get_overview().await;
        b.progress("Got overview details", 50).await;

        // The full overview (website, phone, founded, specialties…) is on /about/.
        let mut headcount = None;
        if let Some(page) = self.get_about_page(linkedin_url).await? {
            let mut details = Overview::default();
            for (label, value, idx) in &page.rows {
                if *idx == 0 {
                    details.apply_dt_dd(label, value);
                } else if headcount.is_none() {
                    // Second <dd> under "Company size": "19 associated members".
                    headcount = parse_leading_count(value);
                }
            }
            o = details.or(o);
            about_us = page.about.or(about_us);
            b.progress("Got about page", 80).await;
        }
        let mut employees = Vec::new();
        if self.max_employees > 0 {
            match self
                .scrape_employees(linkedin_url, self.max_employees)
                .await
            {
                Ok(v) => employees = v,
                Err(e @ Error::RateLimit { .. }) => return Err(e),
                Err(e) => warn!("Could not get employees: {e}"),
            }
            b.progress(&format!("Got {} employees", employees.len()), 95)
                .await;
        }
        if let Some(rl) = b.take_rate_limit() {
            return Err(rl);
        }

        let company = Company {
            employees,
            headcount,
            linkedin_url: linkedin_url.to_string(),
            name: Some(name.clone()),
            about_us,
            website: o.website,
            phone: o.phone,
            headquarters: o.headquarters,
            founded: o.founded,
            industry: o.industry,
            company_type: o.company_type,
            company_size: o.company_size,
            specialties: o.specialties,
            ..Company::default()
        };
        company.validate()?;

        b.progress("Scraping complete", 100).await;
        b.callback.on_complete("company", &company.to_dict()).await;
        info!("Successfully scraped company: {name}");
        Ok(company)
    }

    /// Opens `<company>/about/` and reads its overview. A missing page or
    /// layout gives `None`; a LinkedIn block is returned as an error.
    async fn get_about_page(&self, linkedin_url: &str) -> Result<Option<AboutPage>> {
        let url = company_subpage(linkedin_url, "about");
        let Some(url) = url else {
            return Ok(None);
        };
        match self.base.navigate_and_wait(&url).await {
            Ok(()) => {}
            Err(e @ Error::RateLimit { .. }) => return Err(e),
            Err(e) => {
                debug!("Could not open about page: {e}");
                return Ok(None);
            }
        }
        if self.base.element_exists("main dl", 8_000).await {
            match self
                .base
                .page
                .evaluate::<Option<AboutPage>>(EXTRACT_ABOUT_PAGE_JS)
                .await
            {
                Ok(p) => return Ok(p),
                Err(e) => debug!("About page extraction failed: {e}"),
            }
        }
        Ok(None)
    }

    /// Reads only the company's `/about/` page (one page load): name,
    /// website, industry, headquarters… and the numeric company id. Used to
    /// verify search candidates. `None` if the URL is not a company URL or the
    /// page could not be opened.
    pub async fn about_details(&self, linkedin_url: &str) -> Result<Option<CompanyDetails>> {
        let Some(url) = company_subpage(linkedin_url, "about") else {
            return Ok(None);
        };
        match self.base.navigate_and_wait(&url).await {
            Ok(()) => {}
            Err(e @ (Error::RateLimit { .. } | Error::Authentication(_))) => return Err(e),
            Err(e) => {
                debug!("Could not open about page: {e}");
                return Ok(None);
            }
        }
        let about: Option<AboutPage> = if self.base.element_exists("main dl", 8_000).await {
            self.base
                .page
                .evaluate(EXTRACT_ABOUT_PAGE_JS)
                .await
                .unwrap_or_else(|e| {
                    debug!("About page extraction failed: {e}");
                    None
                })
        } else {
            None
        };
        let evidence: CompanyIdEvidence = self
            .base
            .page
            .evaluate(COMPANY_ID_JS)
            .await
            .unwrap_or_default();
        let name = match evidence.name.clone() {
            Some(n) => n,
            None => self.get_name().await,
        };
        let mut details = Overview::default();
        let mut headcount = None;
        if let Some(a) = &about {
            for (label, value, idx) in &a.rows {
                if *idx == 0 {
                    details.apply_dt_dd(label, value);
                } else if headcount.is_none() {
                    headcount = parse_leading_count(value);
                }
            }
        }
        let o = details.or(self.get_overview().await);
        let company = Company {
            linkedin_url: linkedin_url.to_string(),
            name: Some(name),
            about_us: about.and_then(|a| a.about),
            website: o.website,
            phone: o.phone,
            headquarters: o.headquarters,
            founded: o.founded,
            industry: o.industry,
            company_type: o.company_type,
            company_size: o.company_size,
            specialties: o.specialties,
            headcount,
            ..Company::default()
        };
        Ok(Some(CompanyDetails {
            company_id: evidence.pick(),
            company,
            id_evidence: evidence,
        }))
    }

    async fn get_name(&self) -> String {
        match self
            .base
            .page
            .locator("main h1, h1")
            .first()
            .inner_text()
            .await
        {
            Ok(n) if !n.trim().is_empty() => n.trim().to_string(),
            Ok(_) => "Unknown Company".to_string(),
            Err(e) => {
                warn!("Error getting company name: {e}");
                "Unknown Company".to_string()
            }
        }
    }

    async fn get_about(&self) -> Option<String> {
        let res: Result<Option<String>> = async {
            for section in self.base.page.locator("section").all().await? {
                let text = section.inner_text().await?;
                let head: String = text.chars().take(50).collect();
                if head.contains("About us") {
                    let paragraphs = section.locator("p").all().await?;
                    if let Some(p) = paragraphs.first() {
                        return Ok(Some(p.inner_text().await?.trim().to_string()));
                    }
                }
            }
            Ok(None)
        }
        .await;
        res.unwrap_or_else(|e| {
            debug!("Error getting about section: {e}");
            None
        })
    }

    async fn get_overview(&self) -> Overview {
        let mut o = Overview::default();
        if let Err(e) = self.try_get_overview(&mut o).await {
            debug!("Error getting company overview: {e}");
        }
        o
    }

    async fn try_get_overview(&self, o: &mut Overview) -> Result<()> {
        let page = &self.base.page;

        for text in page
            .locator(".org-top-card-summary-info-list__info-item")
            .all_inner_texts()
            .await?
        {
            o.apply_info_item(&text);
        }

        #[derive(Deserialize)]
        struct Link {
            href: Option<String>,
            text: String,
        }
        match page
            .locator("a")
            .evaluate_all::<Vec<Link>>(
                "els => els.map(e => ({ href: e.getAttribute('href'), text: e.innerText || '' }))",
            )
            .await
        {
            Ok(links) => {
                let pairs: Vec<_> = links
                    .iter()
                    .map(|l| (l.href.as_deref(), l.text.as_str()))
                    .collect();
                if let Some(website) = find_website(&pairs) {
                    o.website = Some(website);
                }
            }
            Err(e) => debug!("Error finding website: {e}"),
        }

        if o.is_empty() {
            for dt in page.locator("dt").all().await? {
                let label = dt.inner_text().await?;
                let dd = dt.locator("xpath=following-sibling::dd[1]");
                if dd.count().await? > 0 {
                    let value = dd.inner_text().await?;
                    if !value.is_empty() {
                        o.apply_dt_dd(&label, &value);
                    }
                }
            }
        }
        Ok(())
    }
}

/// First external link whose text looks like a website call-to-action.
pub(crate) fn find_website(links: &[(Option<&str>, &str)]) -> Option<String> {
    links.iter().find_map(|(href, text)| {
        let href = (*href)?;
        let external =
            !href.contains("linkedin") && (href.contains("http") || href.contains("www."));
        let lower = text.to_lowercase();
        let cta = !text.is_empty()
            && ["learn more", "website", "visit"]
                .iter()
                .any(|w| lower.contains(w));
        (external && cta).then(|| href.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_items() {
        let mut o = Overview::default();
        o.apply_info_item("Software Development");
        o.apply_info_item("Redmond, Washington");
        o.apply_info_item("10K+ employees");
        o.apply_info_item("25M followers");
        assert_eq!(o.industry.as_deref(), Some("Software Development"));
        assert_eq!(o.headquarters.as_deref(), Some("Redmond, Washington"));
        assert_eq!(o.company_size.as_deref(), Some("10K+ employees"));
    }

    #[test]
    fn dt_dd() {
        let mut o = Overview::default();
        o.apply_dt_dd("Website", " https://x.com ");
        o.apply_dt_dd("Founded", "1975");
        o.apply_dt_dd("Specialties", "Cloud");
        assert_eq!(o.website.as_deref(), Some("https://x.com"));
        assert_eq!(o.founded.as_deref(), Some("1975"));
        assert_eq!(o.specialties.as_deref(), Some("Cloud"));
    }

    #[test]
    fn info_items_fr() {
        let mut o = Overview::default();
        o.apply_info_item("Services de conseil en environnement");
        o.apply_info_item("Toulon, Provence-Alpes-Côte d’Azur");
        o.apply_info_item("4\u{a0}K abonnés");
        o.apply_info_item("11-50\u{a0}employés");
        assert_eq!(
            o.industry.as_deref(),
            Some("Services de conseil en environnement")
        );
        assert_eq!(
            o.headquarters.as_deref(),
            Some("Toulon, Provence-Alpes-Côte d’Azur")
        );
        assert_eq!(o.company_size.as_deref(), Some("11-50 employés"));
    }

    #[test]
    fn dt_dd_fr() {
        let mut o = Overview::default();
        o.apply_dt_dd("Site web", "https://alchimistes.co/");
        o.apply_dt_dd("Téléphone", "0422146504");
        o.apply_dt_dd("Page vérifiée", "5 août 2026");
        o.apply_dt_dd("Secteur", "Services de conseil en environnement");
        o.apply_dt_dd("Taille de l’entreprise", "11-50 employés");
        o.apply_dt_dd("Siège social", "Toulon");
        o.apply_dt_dd(
            "Type",
            "Société civile/Société commerciale/Autres types de sociétés",
        );
        o.apply_dt_dd("Fondée en", "2020");
        o.apply_dt_dd("Domaines", "compostage, biodéchets");
        assert_eq!(o.website.as_deref(), Some("https://alchimistes.co/"));
        assert_eq!(o.phone.as_deref(), Some("0422146504"));
        assert_eq!(
            o.industry.as_deref(),
            Some("Services de conseil en environnement")
        );
        assert_eq!(o.company_size.as_deref(), Some("11-50 employés"));
        assert_eq!(o.headquarters.as_deref(), Some("Toulon"));
        assert!(o.company_type.is_some());
        assert_eq!(o.founded.as_deref(), Some("2020"));
        assert_eq!(o.specialties.as_deref(), Some("compostage, biodéchets"));
    }

    #[test]
    fn counts() {
        assert_eq!(parse_leading_count("19\u{a0}membres associés"), Some(19));
        assert_eq!(parse_leading_count("1,234 associated members"), Some(1234));
        assert_eq!(parse_leading_count("12\u{202f}345 membres"), Some(12345));
        assert_eq!(parse_leading_count("membres"), None);
    }

    #[test]
    fn website() {
        let links = [
            (Some("https://www.linkedin.com/x"), "Visit"),
            (Some("https://news.microsoft.com"), "Read"),
            (Some("https://microsoft.com"), "Visit website"),
        ];
        assert_eq!(
            find_website(&links).as_deref(),
            Some("https://microsoft.com")
        );
    }
}
