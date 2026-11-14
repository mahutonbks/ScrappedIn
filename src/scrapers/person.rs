//! Person/profile scraper (port of `scrapers/person.py`).

use std::collections::HashSet;

use tracing::{debug, warn};

use super::base::{BaseScraper, impl_with_callback};
use super::profile_dom::{
    EXTRACT_CONTACTS_JS, EXTRACT_TOP_CARD_JS, RawContact, RawEntry, TopCard, extract_entries_js,
    interest_chips_js, parse_accomplishment_entry, parse_contact_row, parse_education_entry,
    parse_experience_entry, parse_interest_entry, select_interest_chip_js,
};
use super::urljoin;
use crate::core::page::{Locator, Page};
use crate::error::{Error, Result};
use crate::models::{Accomplishment, Contact, Education, Experience, Interest, Person};

/// Scrapes LinkedIn profiles (`https://www.linkedin.com/in/<id>/`).
pub struct PersonScraper {
    base: BaseScraper,
}

impl_with_callback!(PersonScraper);

const ARIA_SPAN: &str = r#"span[aria-hidden="true"]"#;

impl PersonScraper {
    pub fn new(page: Page) -> Self {
        Self {
            base: BaseScraper::new(page),
        }
    }

    fn page(&self) -> &Page {
        &self.base.page
    }

    /// Scrapes a full profile.
    ///
    /// [`Error::RateLimit`] and [`Error::Authentication`] are returned as is
    /// (callers must react to them: wait / re-login); any other failure is
    /// reported as [`Error::Scraping`]. A block hit in any section makes the
    /// whole scrape fail with `RateLimit` instead of returning partial data.
    pub async fn scrape(&self, linkedin_url: &str) -> Result<Person> {
        self.base.callback.on_start("person", linkedin_url).await;
        let _ = self.base.take_rate_limit();
        let res = self.scrape_inner(linkedin_url).await;
        let res = match (res, self.base.take_rate_limit()) {
            (Ok(_), Some(rl)) => Err(rl),
            (res, _) => res,
        };
        match res {
            Ok(person) => Ok(person),
            Err(e) => {
                self.base.callback.on_error(&e).await;
                Err(match e {
                    e @ (Error::RateLimit { .. } | Error::Authentication(_)) => e,
                    e => Error::Scraping(format!("Failed to scrape person profile: {e}")),
                })
            }
        }
    }

    async fn scrape_inner(&self, linkedin_url: &str) -> Result<Person> {
        let b = &self.base;
        b.navigate_and_wait(linkedin_url).await?;
        b.progress("Navigated to profile", 10).await;

        b.ensure_logged_in().await?;

        b.wait_for_selector("main", 10_000).await?;
        // New UI: the top card is rendered client-side, wait for its contact-info link.
        let _ = b
            .wait_for_selector(r#"a[href*="/overlay/contact-info"], main h1"#, 8_000)
            .await;
        b.wait_and_focus(1.0).await;

        let top: TopCard = self
            .page()
            .evaluate(EXTRACT_TOP_CARD_JS)
            .await
            .unwrap_or_else(|e| {
                debug!("Top card extraction failed: {e}");
                TopCard::default()
            });

        let (name, location) = match (top.name.clone(), top.location.clone()) {
            (Some(n), loc @ Some(_)) => (n, loc),
            (n, loc) => {
                let (old_name, old_loc) = self.get_name_and_location().await;
                (n.unwrap_or(old_name), loc.or(old_loc))
            }
        };
        b.progress(&format!("Got name: {name}"), 20).await;

        let open_to_work = top.open_to_work || self.check_open_to_work().await;

        let about = match top.about.clone() {
            Some(a) => Some(a),
            None => self.get_about().await,
        };
        b.progress("Got about section", 30).await;

        // New UI: the contact dialog only opens by clicking the top-card link,
        // so read it now while we are on the profile page.
        let early_contacts = self.get_contacts_new_ui().await;

        b.scroll_page_to_half().await?;
        b.scroll_page_to_bottom(0.5, 3).await?;

        let experiences = self.get_experiences(linkedin_url).await;
        b.progress(&format!("Got {} experiences", experiences.len()), 40)
            .await;

        let educations = self.get_educations(linkedin_url).await;
        b.progress(&format!("Got {} educations", educations.len()), 50)
            .await;

        let interests = self.get_interests(linkedin_url).await;
        b.progress(&format!("Got {} interests", interests.len()), 65)
            .await;

        let accomplishments = self.get_accomplishments(linkedin_url).await;
        b.progress(
            &format!("Got {} accomplishments", accomplishments.len()),
            85,
        )
        .await;

        let contacts = if early_contacts.is_empty() {
            self.get_contacts(linkedin_url).await
        } else {
            early_contacts
        };
        b.progress(&format!("Got {} contacts", contacts.len()), 95)
            .await;

        let person = Person {
            linkedin_url: linkedin_url.to_string(),
            name: Some(name),
            headline: top.headline,
            location,
            about,
            open_to_work,
            experiences,
            educations,
            interests,
            accomplishments,
            contacts,
        };
        person.validate()?;

        b.progress("Scraping complete", 100).await;
        b.callback.on_complete("person", &person.to_dict()).await;
        Ok(person)
    }

    async fn get_name_and_location(&self) -> (String, Option<String>) {
        let mut name = self.base.safe_extract_text("h1", "", 2000).await;
        if name.is_empty() {
            // New LinkedIn UI has no <h1>: the name is in the title "Name | LinkedIn".
            name = self
                .page()
                .evaluate::<String>(
                    r"(document.title || '').replace(/^\(\d+\)\s*/, '').split(' | ')[0].trim()",
                )
                .await
                .unwrap_or_default();
        }
        if name.is_empty() {
            name = "Unknown".to_string();
        }
        let location = self
            .base
            .safe_extract_text(
                ".text-body-small.inline.t-black--light.break-words",
                "",
                2000,
            )
            .await;
        (name, (!location.is_empty()).then_some(location))
    }

    async fn check_open_to_work(&self) -> bool {
        let title = self
            .base
            .get_attribute_safe(".pv-top-card-profile-picture img", "title", "", 2000)
            .await;
        title.to_uppercase().contains("#OPEN_TO_WORK")
    }

    async fn get_about(&self) -> Option<String> {
        match self.try_get_about().await {
            Ok(v) => v,
            Err(e) => {
                debug!("Error getting about section: {e}");
                None
            }
        }
    }

    async fn try_get_about(&self) -> Result<Option<String>> {
        let cards = self
            .page()
            .locator(r#"[data-view-name="profile-card"]"#)
            .all()
            .await?;
        for card in cards {
            let card_text = card.inner_text().await?;
            if card_text.trim().starts_with("About") {
                let spans = card.locator(ARIA_SPAN).all().await?;
                if spans.len() > 1 {
                    let about = spans[1].text_content().await?;
                    return Ok(about
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty()));
                }
            }
        }
        Ok(None)
    }

    // ----------------------------------------------------------------------
    // Experiences
    // ----------------------------------------------------------------------

    async fn get_experiences(&self, base_url: &str) -> Vec<Experience> {
        let mut experiences = Vec::new();
        if let Err(e) = self.try_get_experiences(base_url, &mut experiences).await {
            warn!(
                "Error getting experiences: {e}. The experience section may not be available or the page structure has changed."
            );
        }
        experiences
    }

    /// Opens `base_url/<section>` and returns its list entries (new LinkedIn UI).
    async fn details_entries(&self, base_url: &str, section: &str) -> Result<Vec<RawEntry>> {
        let b = &self.base;
        let base = format!("{}/", base_url.trim_end_matches('/'));
        b.navigate_and_wait(&urljoin(&base, section)).await?;
        b.wait_for_selector("main", 10_000).await?;
        if b.wait_for_selector("main figure", 8_000).await.is_err() {
            return Ok(Vec::new());
        }
        b.wait_and_focus(1.0).await;
        b.scroll_page_to_bottom(0.6, 8).await?;
        self.page().evaluate(&extract_entries_js("figure")).await
    }

    async fn try_get_experiences(&self, base_url: &str, out: &mut Vec<Experience>) -> Result<()> {
        match self.details_entries(base_url, "details/experience/").await {
            Ok(entries) => {
                out.extend(entries.iter().flat_map(parse_experience_entry));
                if !out.is_empty() {
                    return Ok(());
                }
            }
            Err(e) => debug!("New-UI experience extraction failed: {e}"),
        }
        // Legacy layout: its main-page parser needs the profile page back.
        if self.is_new_ui().await {
            return Ok(());
        }
        self.base.navigate_and_wait(base_url).await?;
        self.base.wait_for_selector("main", 10_000).await?;
        let heading = self.page().locator(r#"h2:has-text("Experience")"#).first();
        if heading.count().await? > 0 {
            let section =
                section_for_heading(&heading, "xpath=ancestor::*[.//ul or .//ol][1]").await?;
            if section.count().await? > 0 {
                for item in section.locator("ul > li, ol > li").all().await? {
                    push_parsed(
                        out,
                        self.parse_main_page_experience(&item).await,
                        "experience from main page",
                    );
                }
            }
        }

        if out.is_empty() {
            let b = &self.base;
            b.navigate_and_wait(&urljoin(base_url, "details/experience"))
                .await?;
            b.wait_for_selector("main", 10_000).await?;
            b.wait_and_focus(1.5).await;
            b.scroll_page_to_half().await?;
            b.scroll_page_to_bottom(0.5, 5).await?;

            let items = self.details_list_items("list > listitem, ul > li").await?;
            for item in items {
                match self.parse_experience_item(&item).await {
                    Ok(list) => out.extend(list),
                    Err(e) => debug!("Error parsing experience item: {e}"),
                }
            }
        }
        Ok(())
    }

    /// List items on a `details/...` page: `main` items first, then the legacy
    /// `.pvs-list__container` layout.
    async fn details_list_items(&self, main_selector: &str) -> Result<Vec<Locator>> {
        let main = self.page().locator("main");
        if main.count().await? > 0 {
            let items = main.locator(main_selector).all().await?;
            if !items.is_empty() {
                return Ok(items);
            }
        }
        let old = self.page().locator(".pvs-list__container").first();
        if old.count().await? > 0 {
            return old.locator(".pvs-list__paged-list-item").all().await;
        }
        Ok(Vec::new())
    }

    async fn parse_main_page_experience(&self, item: &Locator) -> Result<Option<Experience>> {
        let links = item.locator("a").all().await?;
        if links.len() < 2 {
            return Ok(None);
        }
        let company_url = links[0].get_attribute("href").await?;
        let texts = extract_unique_texts_from_element(&links[1]).await?;
        if texts.len() < 2 {
            return Ok(None);
        }
        let work_times = texts.get(2).map_or("", String::as_str);
        let (from_date, to_date, duration) = parse_work_times(work_times);
        Ok(Some(Experience {
            position_title: Some(texts[0].clone()),
            institution_name: Some(texts[1].clone()),
            employment_type: None,
            linkedin_url: company_url,
            from_date,
            to_date,
            duration,
            location: None,
            description: None,
        }))
    }

    /// Returns zero, one, or several experiences (nested positions).
    async fn parse_experience_item(&self, item: &Locator) -> Result<Vec<Experience>> {
        let links = item.locator("a, link").all().await?;
        if links.len() >= 2 {
            let company_url = links[0].get_attribute("href").await?;
            let texts = generic_texts(&links[1]).await?;
            if texts.len() >= 2 {
                let work_times = texts.get(2).map_or("", String::as_str);
                let location = texts.get(3).cloned().unwrap_or_default();
                let (from_date, to_date, duration) = parse_work_times(work_times);
                return Ok(vec![Experience {
                    position_title: Some(texts[0].clone()),
                    institution_name: Some(texts[1].clone()),
                    employment_type: None,
                    linkedin_url: company_url,
                    from_date,
                    to_date,
                    duration,
                    location: Some(location),
                    description: None,
                }]);
            }
        }

        let entity = item
            .locator(r#"div[data-view-name="profile-component-entity"]"#)
            .first();
        if entity.count().await? == 0 {
            return Ok(vec![]);
        }
        let children = entity.locator("> *").all().await?;
        if children.len() < 2 {
            return Ok(vec![]);
        }
        let company_url = children[0]
            .locator("a")
            .first()
            .get_attribute("href")
            .await?;

        let detail_children = children[1].locator("> *").all().await?;
        if detail_children.is_empty() {
            return Ok(vec![]);
        }

        let has_nested = detail_children.len() > 1
            && detail_children[1]
                .locator(".pvs-list__container")
                .count()
                .await?
                > 0;
        if has_nested {
            return Ok(self
                .parse_nested_experience(company_url.as_deref(), &detail_children)
                .await);
        }

        let nested = detail_children[0].locator("> *").all().await?;
        if nested.is_empty() {
            return Ok(vec![]);
        }
        let spans = nested[0].locator("> *").all().await?;
        let position_title = aria_text(&spans, 0).await?;
        let company_name = aria_text(&spans, 1).await?;
        let work_times = aria_text(&spans, 2).await?;
        let location = aria_text(&spans, 3).await?;

        let (from_date, to_date, duration) = parse_work_times(&work_times);
        let description = if detail_children.len() > 1 {
            detail_children[1].inner_text().await?
        } else {
            String::new()
        };

        Ok(vec![Experience {
            position_title: Some(position_title.trim().to_string()),
            institution_name: Some(company_name.trim().to_string()),
            employment_type: None,
            linkedin_url: company_url,
            from_date,
            to_date,
            duration,
            location: Some(location.trim().to_string()),
            description: non_empty(description.trim()),
        }])
    }

    /// Several roles at the same company.
    async fn parse_nested_experience(
        &self,
        company_url: Option<&str>,
        detail_children: &[Locator],
    ) -> Vec<Experience> {
        let mut out = Vec::new();
        if let Err(e) = self
            .try_parse_nested_experience(company_url, detail_children, &mut out)
            .await
        {
            debug!("Error parsing nested experience: {e}");
        }
        out
    }

    async fn try_parse_nested_experience(
        &self,
        company_url: Option<&str>,
        detail_children: &[Locator],
        out: &mut Vec<Experience>,
    ) -> Result<()> {
        let nested = detail_children[0].locator("> *").all().await?;
        if nested.is_empty() {
            return Ok(());
        }
        let outer = nested[0].locator("> *").all().await?;
        let company_name = aria_text(&outer, 0).await?;

        let container = detail_children[1].locator(".pvs-list__container").first();
        for nested_item in container
            .locator(".pvs-list__paged-list-item")
            .all()
            .await?
        {
            let res: Result<Option<Experience>> = async {
                let link = nested_item.locator("a").first();
                let link_children = link.locator("> *").all().await?;
                if link_children.is_empty() {
                    return Ok(None);
                }
                let els = link_children[0].locator("> *").all().await?;
                if els.is_empty() {
                    return Ok(None);
                }
                let spans = els[0].locator("> *").all().await?;
                let position_title = aria_text(&spans, 0).await?;
                let work_times = aria_text(&spans, 1).await?;
                let location = aria_text(&spans, 2).await?;
                let (from_date, to_date, duration) = parse_work_times(&work_times);
                let description = if link_children.len() > 1 {
                    link_children[1].inner_text().await?
                } else {
                    String::new()
                };
                Ok(Some(Experience {
                    position_title: Some(position_title.trim().to_string()),
                    institution_name: Some(company_name.trim().to_string()),
                    employment_type: None,
                    linkedin_url: company_url.map(str::to_owned),
                    from_date,
                    to_date,
                    duration,
                    location: Some(location.trim().to_string()),
                    description: non_empty(description.trim()),
                }))
            }
            .await;
            match res {
                Ok(Some(exp)) => out.push(exp),
                Ok(None) => {}
                Err(e) => debug!("Error parsing nested position: {e}"),
            }
        }
        Ok(())
    }

    // ----------------------------------------------------------------------
    // Education
    // ----------------------------------------------------------------------

    async fn get_educations(&self, base_url: &str) -> Vec<Education> {
        let mut educations = Vec::new();
        if let Err(e) = self.try_get_educations(base_url, &mut educations).await {
            warn!(
                "Error getting educations: {e}. The education section may not be publicly visible or the page structure has changed."
            );
        }
        educations
    }

    async fn try_get_educations(&self, base_url: &str, out: &mut Vec<Education>) -> Result<()> {
        match self.details_entries(base_url, "details/education/").await {
            Ok(entries) => {
                out.extend(entries.iter().filter_map(parse_education_entry));
                if !out.is_empty() {
                    return Ok(());
                }
            }
            Err(e) => debug!("New-UI education extraction failed: {e}"),
        }
        // Legacy layout: its main-page parser needs the profile page back.
        if self.is_new_ui().await {
            return Ok(());
        }
        self.base.navigate_and_wait(base_url).await?;
        self.base.wait_for_selector("main", 10_000).await?;
        let heading = self.page().locator(r#"h2:has-text("Education")"#).first();
        if heading.count().await? > 0 {
            let section =
                section_for_heading(&heading, "xpath=ancestor::*[.//ul or .//ol][1]").await?;
            if section.count().await? > 0 {
                for item in section.locator("ul > li, ol > li").all().await? {
                    push_parsed(
                        out,
                        self.parse_main_page_education(&item).await,
                        "education from main page",
                    );
                }
            }
        }

        if out.is_empty() {
            let b = &self.base;
            b.navigate_and_wait(&urljoin(base_url, "details/education"))
                .await?;
            b.wait_for_selector("main", 10_000).await?;
            b.wait_and_focus(2.0).await;
            b.scroll_page_to_half().await?;
            b.scroll_page_to_bottom(0.5, 5).await?;

            for item in self.details_list_items("ul > li, ol > li").await? {
                push_parsed(
                    out,
                    self.parse_education_item(&item).await,
                    "education item",
                );
            }
        }
        Ok(())
    }

    async fn parse_main_page_education(&self, item: &Locator) -> Result<Option<Education>> {
        let links = item.locator("a").all().await?;
        if links.is_empty() {
            return Ok(None);
        }
        let institution_url = links[0].get_attribute("href").await?;
        let detail = links.get(1).unwrap_or(&links[0]);
        let texts = extract_unique_texts_from_element(detail).await?;
        if texts.is_empty() {
            return Ok(None);
        }
        let (degree, times) = split_degree_and_times(&texts);
        let (from_date, to_date) = parse_education_times(&times);
        Ok(Some(Education {
            institution_name: Some(texts[0].clone()),
            degree: degree.as_deref().and_then(|d| non_empty(d.trim())),
            linkedin_url: institution_url,
            from_date,
            to_date,
            description: None,
        }))
    }

    async fn parse_education_item(&self, item: &Locator) -> Result<Option<Education>> {
        let links = item.locator("a, link").all().await?;
        if !links.is_empty() {
            let institution_url = links[0].get_attribute("href").await?;
            let detail = links.get(1).unwrap_or(&links[0]);
            let texts = generic_texts(detail).await?;
            if !texts.is_empty() {
                let (degree, times) = split_degree_and_times(&texts);
                let (from_date, to_date) = parse_education_times(&times);
                return Ok(Some(Education {
                    institution_name: Some(texts[0].clone()),
                    degree: degree.as_deref().and_then(|d| non_empty(d.trim())),
                    linkedin_url: institution_url,
                    from_date,
                    to_date,
                    description: None,
                }));
            }
        }

        let entity = item
            .locator(r#"div[data-view-name="profile-component-entity"]"#)
            .first();
        if entity.count().await? == 0 {
            return Ok(None);
        }
        let children = entity.locator("> *").all().await?;
        if children.len() < 2 {
            return Ok(None);
        }
        let institution_url = children[0]
            .locator("a")
            .first()
            .get_attribute("href")
            .await?;
        let detail_children = children[1].locator("> *").all().await?;
        if detail_children.is_empty() {
            return Ok(None);
        }
        let nested = detail_children[0].locator("> *").all().await?;
        if nested.is_empty() {
            return Ok(None);
        }
        let spans = nested[0].locator("> *").all().await?;

        let institution_name = aria_text(&spans, 0).await?;
        let mut degree = None;
        let mut times = String::new();
        if spans.len() == 3 {
            degree = Some(aria_text(&spans, 1).await?);
            times = aria_text(&spans, 2).await?;
        } else if spans.len() == 2 {
            times = aria_text(&spans, 1).await?;
        }
        let (from_date, to_date) = parse_education_times(&times);
        let description = if detail_children.len() > 1 {
            detail_children[1].inner_text().await?
        } else {
            String::new()
        };

        Ok(Some(Education {
            institution_name: Some(institution_name.trim().to_string()),
            degree: degree.as_deref().and_then(|d| non_empty(d.trim())),
            linkedin_url: institution_url,
            from_date,
            to_date,
            description: non_empty(description.trim()),
        }))
    }

    // ----------------------------------------------------------------------
    // Interests
    // ----------------------------------------------------------------------

    async fn get_interests(&self, base_url: &str) -> Vec<Interest> {
        let mut interests = Vec::new();
        if let Err(e) = self.try_get_interests(base_url, &mut interests).await {
            warn!("Error getting interests: {e}");
        }
        interests
    }

    /// `true` on the 2025+ server-driven UI.
    async fn is_new_ui(&self) -> bool {
        self.page()
            .evaluate::<bool>(
                r#"!!document.querySelector('[data-sdui-screen], [componentkey^="com.linkedin.sdui"]')"#,
            )
            .await
            .unwrap_or(false)
    }

    /// New UI: one category chip (checkbox + label) per interest type; each
    /// chip is selected alone in turn, then its logo list is read.
    async fn interests_new_ui(&self, base_url: &str) -> Result<Vec<Interest>> {
        let b = &self.base;
        b.navigate_and_wait(&urljoin(&with_slash(base_url), "details/interests/"))
            .await?;
        b.wait_for_selector("main", 10_000).await?;
        if !self.is_new_ui().await || b.wait_for_selector("main figure", 8_000).await.is_err() {
            return Ok(Vec::new());
        }
        b.wait_and_focus(1.0).await;

        let chips: Vec<String> = self.page().evaluate(&interest_chips_js()).await?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let rounds = chips.len().max(1);
        for i in 0..rounds {
            let label = chips.get(i).cloned().unwrap_or_default();
            if !chips.is_empty() {
                let js = select_interest_chip_js(i);
                if let Err(e) = self.page().run_js(&js).await {
                    debug!("Could not select interest chip {label}: {e}");
                    continue;
                }
                b.wait_and_focus(1.5).await;
                b.scroll_page_to_bottom(0.5, 4).await?;
            }
            let category = map_interest_tab_to_category(&label);
            let entries: Vec<RawEntry> =
                self.page().evaluate(&extract_entries_js("figure")).await?;
            for e in &entries {
                if let Some(it) = parse_interest_entry(e, &category)
                    && seen.insert((it.category.clone(), it.name.clone()))
                {
                    out.push(it);
                }
            }
        }
        Ok(out)
    }

    /// New UI: clicks the top-card "Contact info" link and reads the dialog.
    /// Must be called on the profile page. Returns an empty list on the old UI.
    async fn get_contacts_new_ui(&self) -> Vec<Contact> {
        if !self.is_new_ui().await {
            return Vec::new();
        }
        let link = self
            .page()
            .locator(r#"a[href*="/overlay/contact-info"]"#)
            .first();
        if link.count().await.unwrap_or(0) == 0 {
            return Vec::new();
        }
        if let Err(e) = link.click().await {
            debug!("Could not open contact info: {e}");
            return Vec::new();
        }
        if self
            .base
            .wait_for_selector(r#"[componentkey$="ContactInfoDetailSection"]"#, 8_000)
            .await
            .is_err()
        {
            debug!("Contact info dialog did not open");
            return Vec::new();
        }
        self.base.wait_and_focus(1.0).await;
        let rows: Option<Vec<RawContact>> = self
            .page()
            .evaluate(EXTRACT_CONTACTS_JS)
            .await
            .unwrap_or_else(|e| {
                debug!("Contact extraction failed: {e}");
                None
            });
        let _ = self.page().press_key("Escape").await;
        rows.unwrap_or_default()
            .iter()
            .flat_map(parse_contact_row)
            .collect()
    }

    async fn try_get_interests(&self, base_url: &str, out: &mut Vec<Interest>) -> Result<()> {
        match self.interests_new_ui(base_url).await {
            Ok(v) if !v.is_empty() => {
                out.extend(v);
                return Ok(());
            }
            Ok(_) => {}
            Err(e @ Error::RateLimit { .. }) => return Err(e),
            Err(e) => debug!("New-UI interests extraction failed: {e}"),
        }
        // Legacy layout: its main-page parser needs the profile page back.
        if self.is_new_ui().await {
            return Ok(());
        }
        self.base.navigate_and_wait(base_url).await?;
        self.base.wait_for_selector("main", 10_000).await?;
        let heading = self.page().locator(r#"h2:has-text("Interests")"#).first();
        if heading.count().await? > 0 {
            let section = section_for_heading(
                &heading,
                r#"xpath=ancestor::*[.//tablist or .//*[@role="tablist"]][1]"#,
            )
            .await?;
            let tabs = if section.count().await? > 0 {
                section.locator(r#"[role="tab"], tab"#).all().await?
            } else {
                Vec::new()
            };
            let panel = section.locator(r#"[role="tabpanel"]"#);
            self.read_interest_tabs(tabs, &panel, "li, listitem", 0.5, out)
                .await;
        }

        if out.is_empty() {
            let b = &self.base;
            b.navigate_and_wait(&urljoin(base_url, "details/interests/"))
                .await?;
            b.wait_for_selector("main", 10_000).await?;
            b.wait_and_focus(1.5).await;

            let tabs = self.page().locator(r#"[role="tab"], tab"#).all().await?;
            if tabs.is_empty() {
                debug!("No interests tabs found on profile");
                return Ok(());
            }
            let panel = self.page().locator(r#"[role="tabpanel"], tabpanel"#);
            let items = "listitem, li, .pvs-list__paged-list-item";
            self.read_interest_tabs(tabs, &panel, items, 0.8, out).await;
        }
        Ok(())
    }

    /// Legacy UI: clicks each interest tab, then parses the items of the
    /// first `panel` match.
    async fn read_interest_tabs(
        &self,
        tabs: Vec<Locator>,
        panel: &Locator,
        item_selector: &str,
        pause: f64,
        out: &mut Vec<Interest>,
    ) {
        for tab in tabs {
            let res: Result<()> = async {
                let Some(tab_name) = tab.text_content().await?.filter(|t| !t.is_empty()) else {
                    return Ok(());
                };
                let category = map_interest_tab_to_category(tab_name.trim());
                tab.click().await?;
                self.base.wait_and_focus(pause).await;
                for item in panel.first().locator(item_selector).all().await? {
                    let parsed = self.parse_interest_item(&item, &category).await;
                    push_parsed(out, parsed, "interest item");
                }
                Ok(())
            }
            .await;
            if let Err(e) = res {
                debug!("Error processing interest tab: {e}");
            }
        }
    }

    async fn parse_interest_item(
        &self,
        item: &Locator,
        category: &str,
    ) -> Result<Option<Interest>> {
        let link = item.locator("a, link").first();
        if link.count().await? == 0 {
            return Ok(None);
        }
        let href = link.get_attribute("href").await?;
        let texts = extract_unique_texts_from_element(item).await?;
        match (texts.into_iter().next(), href) {
            (Some(name), Some(href)) if !name.is_empty() && !href.is_empty() => {
                Ok(Some(Interest {
                    name,
                    category: category.to_string(),
                    linkedin_url: Some(href),
                }))
            }
            _ => Ok(None),
        }
    }

    // ----------------------------------------------------------------------
    // Accomplishments
    // ----------------------------------------------------------------------

    async fn get_accomplishments(&self, base_url: &str) -> Vec<Accomplishment> {
        const SECTIONS: [(&str, &str); 8] = [
            ("certifications", "certification"),
            ("honors", "honor"),
            ("publications", "publication"),
            ("patents", "patent"),
            ("courses", "course"),
            ("projects", "project"),
            ("languages", "language"),
            ("organizations", "organization"),
        ];
        let mut out = Vec::new();
        for (url_path, category) in SECTIONS {
            if let Err(e) = self
                .scrape_accomplishment_section(base_url, url_path, category, &mut out)
                .await
            {
                debug!("Error getting {category}s: {e}");
            }
        }
        out
    }

    async fn scrape_accomplishment_section(
        &self,
        base_url: &str,
        url_path: &str,
        category: &str,
        out: &mut Vec<Accomplishment>,
    ) -> Result<()> {
        let b = &self.base;
        b.navigate_and_wait(&urljoin(
            &with_slash(base_url),
            &format!("details/{url_path}/"),
        ))
        .await?;
        b.wait_for_selector("main", 10_000).await?;
        b.wait_and_focus(1.0).await;

        if self.is_new_ui().await {
            let _ = b.wait_for_selector("main section p", 8_000).await;
            b.wait_and_focus(0.5).await;
            b.scroll_page_to_bottom(0.5, 4).await?;
            let entries: Vec<RawEntry> = self.page().evaluate(&extract_entries_js("list")).await?;
            let mut seen_titles = HashSet::new();
            for e in &entries {
                if let Some(a) = parse_accomplishment_entry(e, category)
                    && seen_titles.insert(a.title.clone())
                {
                    out.push(a);
                }
            }
            return Ok(());
        }

        if self
            .page()
            .locator(r#"text="Nothing to see for now""#)
            .count()
            .await?
            > 0
        {
            return Ok(());
        }

        let main_list = self
            .page()
            .locator(".pvs-list__container, main ul, main ol")
            .first();
        if main_list.count().await? == 0 {
            return Ok(());
        }
        let mut items = main_list
            .locator(".pvs-list__paged-list-item")
            .all()
            .await?;
        if items.is_empty() {
            items = main_list.locator("> li").all().await?;
        }

        let mut seen_titles = HashSet::new();
        for item in items {
            match self.parse_accomplishment_item(&item, category).await {
                Ok(Some(a)) => {
                    if seen_titles.insert(a.title.clone()) {
                        out.push(a);
                    }
                }
                Ok(None) => {}
                Err(e) => debug!("Error parsing {category} item: {e}"),
            }
        }
        Ok(())
    }

    async fn parse_accomplishment_item(
        &self,
        item: &Locator,
        category: &str,
    ) -> Result<Option<Accomplishment>> {
        let entity = item
            .locator(r#"div[data-view-name="profile-component-entity"]"#)
            .first();
        let spans = if entity.count().await? > 0 {
            entity.locator(ARIA_SPAN).all().await?
        } else {
            item.locator(ARIA_SPAN).all().await?
        };

        let mut texts = Vec::new();
        for span in spans.iter().take(5) {
            texts.push(span.text_content().await?);
        }
        let mut acc = parse_accomplishment_texts(&texts, category);

        let link = item
            .locator(r#"a[href*="credential"], a[href*="verify"]"#)
            .first();
        if link.count().await? > 0 {
            acc.credential_url = link.get_attribute("href").await?;
        }

        if acc.title.is_empty() || acc.title.chars().count() > 200 {
            return Ok(None);
        }
        Ok(Some(acc))
    }

    // ----------------------------------------------------------------------
    // Contacts
    // ----------------------------------------------------------------------

    async fn get_contacts(&self, base_url: &str) -> Vec<Contact> {
        let mut contacts = Vec::new();
        if let Err(e) = self.try_get_contacts(base_url, &mut contacts).await {
            warn!("Error getting contacts: {e}");
        }
        contacts
    }

    async fn try_get_contacts(&self, base_url: &str, out: &mut Vec<Contact>) -> Result<()> {
        let b = &self.base;
        b.navigate_and_wait(&urljoin(base_url, "overlay/contact-info/"))
            .await?;
        b.wait_and_focus(1.0).await;

        let dialog = self.page().locator(r#"dialog, [role="dialog"]"#).first();
        if dialog.count().await? == 0 {
            warn!("Contact info dialog not found");
            return Ok(());
        }

        for heading in dialog.locator("h3").all().await? {
            let res: Result<()> = async {
                let Some(heading_text) = heading.text_content().await?.filter(|t| !t.is_empty())
                else {
                    return Ok(());
                };
                let heading_text = heading_text.trim().to_lowercase();
                let section = heading.locator("xpath=ancestor::*[1]");
                if section.count().await? == 0 {
                    return Ok(());
                }
                let Some(contact_type) = map_contact_heading_to_type(&heading_text) else {
                    return Ok(());
                };

                let links = section.locator("a").all().await?;
                for link in &links {
                    let href = link.get_attribute("href").await?;
                    let text = link.text_content().await?;
                    let (Some(href), Some(text)) = (href, text) else {
                        continue;
                    };
                    if href.is_empty() || text.is_empty() {
                        continue;
                    }
                    let text = text.trim().to_string();

                    let mut label = None;
                    for sib in section.locator("span, generic").all_text_contents().await? {
                        let s = sib.trim();
                        if s.starts_with('(') && s.ends_with(')') && s.len() >= 2 {
                            label = Some(s[1..s.len() - 1].to_string());
                            break;
                        }
                    }

                    let value = match contact_type {
                        "linkedin" => href.clone(),
                        "email" if href.contains("mailto:") => href.replace("mailto:", ""),
                        _ => text,
                    };
                    out.push(Contact {
                        contact_type: contact_type.to_string(),
                        value,
                        label,
                    });
                }

                if links.is_empty()
                    && let Some(word) = match contact_type {
                        "birthday" => Some("Birthday"),
                        "phone" => Some("Phone"),
                        "address" => Some("Address"),
                        _ => None,
                    }
                    && let Some(raw) = section.text_content().await?
                {
                    let value = raw.replace(&heading_text, "").replace(word, "");
                    let value = value.trim();
                    if !value.is_empty() {
                        out.push(Contact {
                            contact_type: contact_type.to_string(),
                            value: value.to_string(),
                            label: None,
                        });
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = res {
                debug!("Error parsing contact section: {e}");
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// DOM helpers
// --------------------------------------------------------------------------

/// Nearest ancestor matching `xpath`, falling back to the 4th ancestor.
async fn section_for_heading(heading: &Locator, xpath: &str) -> Result<Locator> {
    let section = heading.locator(xpath);
    if section.count().await? == 0 {
        return Ok(heading.locator("xpath=ancestor::*[4]"));
    }
    Ok(section)
}

/// Text of the `span[aria-hidden="true"]` inside `spans[i]`, or "" if `i` is out of range.
async fn aria_text(spans: &[Locator], i: usize) -> Result<String> {
    match spans.get(i) {
        Some(s) => Ok(s
            .locator(ARIA_SPAN)
            .first()
            .text_content()
            .await?
            .unwrap_or_default()),
        None => Ok(String::new()),
    }
}

/// Unique trimmed texts (< 200 chars) of `generic, span, div` descendants.
async fn generic_texts(el: &Locator) -> Result<Vec<String>> {
    let raw = el.locator("generic, span, div").all_text_contents().await?;
    let mut seen = HashSet::new();
    Ok(raw
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && t.chars().count() < 200)
        .filter(|t| seen.insert(t.clone()))
        .collect())
}

/// Texts of nested elements without parent/child duplicates.
async fn extract_unique_texts_from_element(el: &Locator) -> Result<Vec<String>> {
    let mut raw = el
        .locator(r#"span[aria-hidden="true"], div > span"#)
        .all_text_contents()
        .await?;
    if raw.is_empty() {
        raw = el.locator("span, div").all_text_contents().await?;
    }
    Ok(dedupe_overlapping_texts(raw))
}

/// Port of the de-duplication loop of `_extract_unique_texts_from_element`.
pub(crate) fn dedupe_overlapping_texts(raw: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for text in raw {
        let text = text.trim();
        if text.is_empty() || text.chars().count() >= 200 {
            continue;
        }
        let duplicate = seen.iter().any(|t| t == text);
        let overlaps = seen
            .iter()
            .filter(|t| t.chars().count() > 3)
            .any(|t| t.contains(text) || text.contains(t.as_str()));
        if !duplicate && !overlaps {
            seen.push(text.to_string());
        }
    }
    seen
}

/// `https://…/in/x` → `https://…/in/x/` (so that `urljoin` keeps the last segment).
fn with_slash(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    format!("{}/", base.trim_end_matches('/'))
}

/// Keeps `Ok(Some(v))`, skips `Ok(None)` and logs errors.
fn push_parsed<T>(out: &mut Vec<T>, res: Result<Option<T>>, what: &str) {
    match res {
        Ok(Some(v)) => out.push(v),
        Ok(None) => {}
        Err(e) => debug!("Error parsing {what}: {e}"),
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

// --------------------------------------------------------------------------
// Pure parsing helpers
// --------------------------------------------------------------------------

/// `"Jan 2020 - Dec 2022 · 2 yrs"` → `(from, to, duration)`.
pub(crate) fn parse_work_times(
    work_times: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    if work_times.is_empty() {
        return (None, None, None);
    }
    let work_times = work_times.replace(" – ", " - ");
    let mut parts = work_times.split('·');
    let times = parts.next().unwrap_or("").trim();
    let duration = parts.next().map(|d| d.trim().to_string());

    let (from, to) = if times.contains(" - ") {
        let mut dp = times.split(" - ");
        let from = dp.next().unwrap_or("").trim().to_string();
        let to = dp.next().map(|t| t.trim().to_string()).unwrap_or_default();
        (from, to)
    } else {
        (times.to_string(), String::new())
    };
    (Some(from), Some(to), duration)
}

/// `"1973 - 1977"` → `("1973", "1977")`, `"2015"` → `("2015", "2015")`.
pub(crate) fn parse_education_times(times: &str) -> (Option<String>, Option<String>) {
    let times = times.replace(" – ", " - ");
    let times = times.as_str();
    if times.is_empty() {
        return (None, None);
    }
    if times.contains(" - ") {
        let mut parts = times.split(" - ");
        let from = parts.next().unwrap_or("").trim().to_string();
        let to = parts
            .next()
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        (Some(from), Some(to))
    } else {
        let t = times.trim().to_string();
        (Some(t.clone()), Some(t))
    }
}

/// From `[institution, degree?, times?]` decide which is the degree and which the dates.
fn split_degree_and_times(texts: &[String]) -> (Option<String>, String) {
    match texts.len() {
        3 => (Some(texts[1].clone()), texts[2].clone()),
        2 => {
            let second = &texts[1];
            if second.contains(" - ") || second.chars().any(|c| c.is_ascii_digit()) {
                (None, second.clone())
            } else {
                (Some(second.clone()), String::new())
            }
        }
        _ => (None, String::new()),
    }
}

pub(crate) fn map_interest_tab_to_category(tab_name: &str) -> String {
    let t = tab_name.trim().to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    let c = if has(&["compan", "entreprise", "empresa", "unternehmen", "aziend"]) {
        "company"
    } else if has(&["group", "grupo", "gruppe", "gruppi"]) {
        "group"
    } else if has(&[
        "school",
        "école",
        "ecole",
        "escuela",
        "schule",
        "scuol",
        "universit",
    ]) {
        "school"
    } else if has(&["newsletter"]) {
        "newsletter"
    } else if has(&["voice", "voix", "voces", "stimme", "influencer"]) {
        "influencer"
    } else {
        return t;
    };
    c.to_string()
}

/// Contact-info heading → contact type, in EN / FR / ES / DE / IT.
pub(crate) fn map_contact_heading_to_type(heading: &str) -> Option<&'static str> {
    let h = heading.trim().to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| h.contains(w));
    if has(&["profile", "profil", "perfil", "profilo"]) {
        Some("linkedin")
    } else if has(&["website", "site web", "sitio web", "webseite", "sito web"]) {
        Some("website")
    } else if has(&["email", "e-mail", "courriel", "correo"]) {
        Some("email")
    } else if has(&["phone", "téléphone", "teléfono", "telefon"]) {
        Some("phone")
    } else if has(&["twitter", "x.com"]) {
        Some("twitter")
    } else if has(&[
        "birthday",
        "anniversaire",
        "cumpleaños",
        "geburtstag",
        "compleanno",
    ]) {
        Some("birthday")
    } else if has(&["address", "adresse", "dirección", "anschrift", "indirizzo"]) {
        Some("address")
    } else {
        None
    }
}

/// Interprets the first (up to 5) aria spans of an accomplishment item.
pub(crate) fn parse_accomplishment_texts(
    texts: &[Option<String>],
    category: &str,
) -> Accomplishment {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (mut title, mut issuer, mut issued_date, mut credential_id) =
        (String::new(), String::new(), String::new(), String::new());

    for (i, text) in texts.iter().enumerate().take(5) {
        let Some(text) = text.as_deref().filter(|t| !t.is_empty()) else {
            continue;
        };
        let text = text.trim();
        if text.chars().count() > 500 {
            continue;
        }
        if i == 0 {
            title = text.to_string();
        } else if text.contains("Issued by") {
            let mut parts = text.split('·');
            issuer = parts
                .next()
                .unwrap_or("")
                .replace("Issued by", "")
                .trim()
                .to_string();
            if let Some(d) = parts.next() {
                issued_date = d.trim().to_string();
            }
        } else if text.contains("Issued ") && issued_date.is_empty() {
            issued_date = text.replace("Issued ", "");
        } else if text.contains("Credential ID") {
            credential_id = text.replace("Credential ID ", "");
        } else if i == 1 && issuer.is_empty() {
            issuer = text.to_string();
        } else if MONTHS.iter().any(|m| text.contains(m)) && issued_date.is_empty() {
            issued_date = match text.split_once('·') {
                Some((d, _)) => d.trim().to_string(),
                None => text.to_string(),
            };
        }
    }

    Accomplishment {
        category: category.to_string(),
        title,
        issuer: non_empty(&issuer),
        issued_date: non_empty(&issued_date),
        credential_id: non_empty(&credential_id),
        credential_url: None,
        description: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn work_times() {
        assert_eq!(
            parse_work_times("2000 - Present · 26 yrs 1 mo"),
            (s("2000"), s("Present"), s("26 yrs 1 mo"))
        );
        assert_eq!(
            parse_work_times("Jan 2020 - Dec 2022 · 2 yrs"),
            (s("Jan 2020"), s("Dec 2022"), s("2 yrs"))
        );
        assert_eq!(
            parse_work_times("2015 - Present"),
            (s("2015"), s("Present"), None)
        );
        assert_eq!(parse_work_times("2015"), (s("2015"), s(""), None));
        assert_eq!(parse_work_times(""), (None, None, None));
    }

    #[test]
    fn education_times() {
        assert_eq!(parse_education_times("1973 - 1977"), (s("1973"), s("1977")));
        assert_eq!(parse_education_times("2015"), (s("2015"), s("2015")));
        assert_eq!(parse_education_times(""), (None, None));
    }

    #[test]
    fn degree_split() {
        let t = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            split_degree_and_times(&t(&["Harvard", "BSc", "1973 - 1975"])),
            (s("BSc"), "1973 - 1975".into())
        );
        assert_eq!(
            split_degree_and_times(&t(&["Harvard", "1973 - 1975"])),
            (None, "1973 - 1975".into())
        );
        assert_eq!(
            split_degree_and_times(&t(&["Harvard", "BSc"])),
            (s("BSc"), "".into())
        );
    }

    #[test]
    fn unique_texts() {
        let raw = [
            "Software Engineer",
            "Software Engineer",
            "Acme · Full-time",
            "Acme",
            "2020 - Present",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            dedupe_overlapping_texts(raw),
            vec!["Software Engineer", "Acme · Full-time", "2020 - Present"]
        );
    }

    #[test]
    fn categories() {
        assert_eq!(map_interest_tab_to_category("Companies"), "company");
        assert_eq!(map_interest_tab_to_category("Top Voices"), "influencer");
        assert_eq!(map_interest_tab_to_category("Other"), "other");
        assert_eq!(
            map_contact_heading_to_type("your profile"),
            Some("linkedin")
        );
        assert_eq!(map_contact_heading_to_type("email"), Some("email"));
        assert_eq!(map_contact_heading_to_type("connected"), None);
    }

    #[test]
    fn accomplishments() {
        let a = parse_accomplishment_texts(
            &[
                s("AWS Solutions Architect"),
                s("Amazon Web Services (AWS)"),
                s("Issued Jan 2024"),
                s("Credential ID ABC123"),
            ],
            "certification",
        );
        assert_eq!(a.title, "AWS Solutions Architect");
        assert_eq!(a.issuer.as_deref(), Some("Amazon Web Services (AWS)"));
        assert_eq!(a.issued_date.as_deref(), Some("Jan 2024"));
        assert_eq!(a.credential_id.as_deref(), Some("ABC123"));

        let b = parse_accomplishment_texts(&[s("Prize"), s("Issued by ACM · Mar 2020")], "honor");
        assert_eq!(b.issuer.as_deref(), Some("ACM"));
        assert_eq!(b.issued_date.as_deref(), Some("Mar 2020"));
    }
}
