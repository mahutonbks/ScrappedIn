//! End-to-end tests against a local fake of LinkedIn served over HTTP on
//! `127.0.0.1`. URLs are prefixed with `/fake.linkedin.com` so they pass the
//! model validators (`linkedin.com/in/`, `linkedin.com/company/`, …).
//!
//! Requires a local Chrome/Chromium. Tests print "SKIP" and pass if none is found.

use std::time::Duration;

use linkedin_scraper::{
    BrowserManager, BrowserOptions, CompanyPostsScraper, CompanyQuery, CompanyRef, CompanyScraper,
    CompanySearch, Error, IdSource, JobScraper, PeopleSearch, PeopleSearchParams, PersonQuery,
    PersonScraper, Resolution, ResolveConfig, Resolver, is_logged_in,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NAV: &str = r#"<nav><a href="/feed/">Home</a><a href="/mynetwork/">Network</a></nav>"#;

fn page(body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>t</title></head><body>{NAV}{body}</body></html>"
    )
}

fn profile() -> String {
    page(
        r##"
<main>
  <section>
    <div class="pv-top-card-profile-picture"><img title="John Doe #OPEN_TO_WORK" width="50" height="50"></div>
    <h1>John Doe</h1>
    <span class="text-body-small inline t-black--light break-words">Paris, Île-de-France</span>
  </section>
  <section data-view-name="profile-card">
    <div><h2><span aria-hidden="true">About</span></h2></div>
    <div><span aria-hidden="true">I build distributed systems.</span></div>
  </section>
  <section>
    <div><div><h2>Experience</h2></div></div>
    <ul>
      <li>
        <a href="https://fakelinkedin.com/company/acme/">logo</a>
        <a href="#x">
          <div><span aria-hidden="true">Staff Engineer</span></div>
          <div><span aria-hidden="true">Acme · Full-time</span></div>
          <div><span aria-hidden="true">Jan 2020 - Present · 4 yrs 10 mos</span></div>
        </a>
      </li>
      <li>
        <a href="https://fakelinkedin.com/company/globex/">logo</a>
        <a href="#y">
          <div><span aria-hidden="true">Engineer</span></div>
          <div><span aria-hidden="true">Globex</span></div>
          <div><span aria-hidden="true">2015 - 2019 · 4 yrs</span></div>
        </a>
      </li>
    </ul>
  </section>
  <section>
    <div><div><h2>Education</h2></div></div>
    <ul>
      <li>
        <a href="https://fakelinkedin.com/school/mit/">logo</a>
        <a href="#z">
          <div><span aria-hidden="true">MIT</span></div>
          <div><span aria-hidden="true">BSc Computer Science</span></div>
          <div><span aria-hidden="true">2011 - 2015</span></div>
        </a>
      </li>
    </ul>
  </section>
  <section>
    <div><div><h2>Interests</h2></div></div>
    <div role="tablist">
      <button role="tab" onclick="show('c')">Companies</button>
      <button role="tab" onclick="show('g')">Groups</button>
    </div>
    <div role="tabpanel" id="panel"></div>
  </section>
</main>
<script>
  const data = {
    c: '<ul><li><a href="/company/rust/"><span aria-hidden="true">Rust Foundation</span></a></li></ul>',
    g: '<ul><li><a href="/groups/42/"><span aria-hidden="true">Embedded Rust</span></a></li></ul>',
  };
  function show(k) { document.getElementById('panel').innerHTML = data[k]; }
</script>"##,
    )
}

fn certifications() -> String {
    page(
        r##"
<main><div class="pvs-list__container"><ul>
  <li class="pvs-list__paged-list-item">
    <div data-view-name="profile-component-entity">
      <span aria-hidden="true">Certified Kubernetes Administrator</span>
      <span aria-hidden="true">The Linux Foundation</span>
      <span aria-hidden="true">Issued Mar 2024</span>
      <span aria-hidden="true">Credential ID CKA-123</span>
    </div>
    <a href="https://verify.example.com/credential/CKA-123">Show credential</a>
  </li>
</ul></div></main>"##,
    )
}

fn contact_info() -> String {
    page(
        r##"
<main></main>
<div role="dialog">
  <section><h3>Email</h3><a href="mailto:john@example.com">john@example.com</a></section>
  <section><h3>Website</h3><a href="https://john.dev">john.dev</a><span>(Personal)</span></section>
  <section><h3>Phone</h3><span>+33 1 23 45 67 89</span></section>
</div>"##,
    )
}

fn company() -> String {
    page(
        r##"
<main>
  <h1>Acme Corp</h1>
  <ul>
    <li class="org-top-card-summary-info-list__info-item">Software Development</li>
    <li class="org-top-card-summary-info-list__info-item">Austin, Texas</li>
    <li class="org-top-card-summary-info-list__info-item">1,001-5,000 employees</li>
    <li class="org-top-card-summary-info-list__info-item">120K followers</li>
  </ul>
  <a href="https://acme.example">Visit website</a>
  <section><h2>About us</h2><p>We make everything.</p></section>
</main>"##,
    )
}

fn job() -> String {
    page(
        r##"
<main>
  <h1>Rust Developer</h1>
  <a href="/company/acme/life/?trk=x"><img alt="logo"></a>
  <a href="/company/acme/life/?trk=x">Acme Corp</a>
  <div class="job-details-jobs-unified-top-card__primary-description-container">Lyon, France · 3 days ago · 27 applicants</div>
  <article><h2>About the job</h2><p>Write fast and safe code.</p></article>
</main>"##,
    )
}

fn posts() -> String {
    page(
        r##"
<main>
  <div data-urn="urn:li:activity:111">
    <div class="update-components-actor__sub-description">2d • Edited •</div>
    <div class="update-components-text">We just shipped our new embedded Rust toolkit, check it out!</div>
    <button aria-label="1,234 reactions">1,234</button>
    <button aria-label="56 comments">56 comments</button>
    <button aria-label="7 reposts">7 reposts</button>
  </div>
  <div data-urn="urn:li:activity:222">
    <div class="update-components-actor__sub-description">1w •</div>
    <div class="update-components-text">Hiring: two firmware engineers in Toulon and Marseille.</div>
  </div>
</main>"##,
    )
}

fn nothing() -> String {
    page("<main><h2>Nothing to see for now</h2></main>")
}

fn route(path: &str) -> (u16, String) {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.strip_prefix("/fake.linkedin.com").unwrap_or(path);
    match path {
        "/in/john/" => (200, profile()),
        "/in/john/details/certifications/" => (200, certifications()),
        "/in/john/overlay/contact-info/" => (200, contact_info()),
        p if p.starts_with("/in/john/details/") => (200, nothing()),
        "/company/acme/" => (200, company()),
        "/company/acme/posts/" => (200, posts()),
        "/jobs/view/1/" => (200, job()),
        "/feed/" => (200, page("<main>feed</main>")),
        "/login" => (200, page("<main>login</main>")),
        "/cookies" => (
            200,
            page(
                "<main id=ok>ok</main><script>document.cookie='li_at=SECRET; path=/; max-age=3600'; localStorage.setItem('theme','dark');</script>",
            ),
        ),
        "/check" => (
            200,
            page(
                "<main id=out></main><script>document.getElementById('out').textContent = document.cookie + '|' + localStorage.getItem('theme');</script>",
            ),
        ),
        _ => (404, page("<main>404</main>")),
    }
}

async fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = route_search(&path).unwrap_or_else(|| route(&path));
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

async fn launch(port: u16) -> Option<BrowserManager> {
    let _ = port;
    let opts = BrowserOptions::default().arg("--no-proxy-server");
    match BrowserManager::launch(opts).await {
        Ok(b) => Some(b),
        // Only skip when no Chrome is installed; any other launch error is a failure.
        Err(e) if e.to_string().contains("Could not auto detect") => {
            eprintln!("SKIP: no Chrome/Chromium found: {e}");
            None
        }
        Err(e) => panic!("cannot launch Chrome: {e}"),
    }
}

fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}/fake.linkedin.com")
}

#[tokio::test(flavor = "multi_thread")]
async fn locator_engine() {
    let port = serve().await;
    let Some(mut browser) = launch(port).await else {
        return;
    };
    let page = browser.page().unwrap();
    page.goto(
        &format!("{}/in/john/", base(port)),
        Default::default(),
        Duration::from_secs(20),
    )
    .await
    .unwrap();

    // :has-text + xpath ancestor + relative child selectors
    let heading = page.locator(r#"h2:has-text("experience")"#).first();
    assert_eq!(heading.count().await.unwrap(), 1);
    let section = heading.locator("xpath=ancestor::*[.//ul or .//ol][1]");
    assert_eq!(section.locator("ul > li").count().await.unwrap(), 2);
    let ul = section.locator("ul").first();
    assert_eq!(ul.locator("> li").count().await.unwrap(), 2);
    assert_eq!(ul.locator("> *").count().await.unwrap(), 2);

    // comma lists are de-duplicated and kept in document order
    let aria = r#"span[aria-hidden="true"]"#;
    let single = page.locator(aria).count().await.unwrap();
    let doubled = page
        .locator(&format!("{aria}, {aria}"))
        .count()
        .await
        .unwrap();
    assert_eq!(single, doubled);
    let texts = page
        .locator(&format!("h1, {aria}"))
        .all_text_contents()
        .await
        .unwrap();
    assert_eq!(texts[0], "John Doe");
    assert_eq!(texts[1], "About");

    // nth / last / attributes / visibility
    let items = page.locator("main ul > li").all().await.unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[2]
            .locator("a")
            .first()
            .get_attribute("href")
            .await
            .unwrap()
            .as_deref(),
        Some("https://fakelinkedin.com/school/mit/")
    );
    assert!(page.locator("h1").is_visible().await.unwrap());
    assert!(!page.locator("#does-not-exist").is_visible().await.unwrap());

    // text= exact
    assert_eq!(page.locator(r#"text="John Doe""#).count().await.unwrap(), 1);

    // timeout behaviour
    let err = page
        .locator("#nope")
        .timeout(Duration::from_millis(300))
        .text_content()
        .await
        .unwrap_err();
    assert!(err.is_timeout(), "{err}");

    // real mouse click
    page.locator(r#"[role="tab"]"#)
        .nth(1)
        .click()
        .await
        .unwrap();
    assert_eq!(
        page.locator(r#"[role="tabpanel"] li"#)
            .first()
            .inner_text()
            .await
            .unwrap(),
        "Embedded Rust"
    );

    // logged-in heuristic sees the nav
    assert!(is_logged_in(&page).await);

    browser.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn person_scraper_end_to_end() {
    let port = serve().await;
    let Some(mut browser) = launch(port).await else {
        return;
    };

    let person = PersonScraper::new(browser.page().unwrap())
        .scrape(&format!("{}/in/john/", base(port)))
        .await
        .unwrap();

    assert_eq!(person.name.as_deref(), Some("John Doe"));
    assert_eq!(person.location.as_deref(), Some("Paris, Île-de-France"));
    assert!(person.open_to_work);
    assert_eq!(
        person.about.as_deref(),
        Some("I build distributed systems.")
    );

    assert_eq!(person.experiences.len(), 2);
    let e = &person.experiences[0];
    assert_eq!(e.position_title.as_deref(), Some("Staff Engineer"));
    assert_eq!(e.institution_name.as_deref(), Some("Acme · Full-time"));
    assert_eq!(e.from_date.as_deref(), Some("Jan 2020"));
    assert_eq!(e.to_date.as_deref(), Some("Present"));
    assert_eq!(e.duration.as_deref(), Some("4 yrs 10 mos"));
    assert_eq!(
        e.linkedin_url.as_deref(),
        Some("https://fakelinkedin.com/company/acme/")
    );
    assert_eq!(person.company(), Some("Acme · Full-time"));
    assert_eq!(person.job_title(), Some("Staff Engineer"));

    assert_eq!(person.educations.len(), 1);
    let ed = &person.educations[0];
    assert_eq!(ed.institution_name.as_deref(), Some("MIT"));
    assert_eq!(ed.degree.as_deref(), Some("BSc Computer Science"));
    assert_eq!(
        (ed.from_date.as_deref(), ed.to_date.as_deref()),
        (Some("2011"), Some("2015"))
    );

    let interests: Vec<_> = person
        .interests
        .iter()
        .map(|i| (i.category.as_str(), i.name.as_str()))
        .collect();
    assert_eq!(
        interests,
        vec![("company", "Rust Foundation"), ("group", "Embedded Rust")]
    );

    assert_eq!(person.accomplishments.len(), 1);
    let a = &person.accomplishments[0];
    assert_eq!(a.category, "certification");
    assert_eq!(a.title, "Certified Kubernetes Administrator");
    assert_eq!(a.issuer.as_deref(), Some("The Linux Foundation"));
    assert_eq!(a.issued_date.as_deref(), Some("Mar 2024"));
    assert_eq!(a.credential_id.as_deref(), Some("CKA-123"));
    assert_eq!(
        a.credential_url.as_deref(),
        Some("https://verify.example.com/credential/CKA-123")
    );

    let contacts: Vec<_> = person
        .contacts
        .iter()
        .map(|c| {
            (
                c.contact_type.as_str(),
                c.value.as_str(),
                c.label.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        contacts,
        vec![
            ("email", "john@example.com", None),
            ("website", "john.dev", Some("Personal")),
            ("phone", "+33 1 23 45 67 89", None),
        ]
    );

    println!("{}", person.to_json_pretty().unwrap());
    browser.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn company_job_and_posts() {
    let port = serve().await;
    let Some(mut browser) = launch(port).await else {
        return;
    };
    let page = browser.page().unwrap();

    let company = CompanyScraper::new(page.clone())
        .scrape(&format!("{}/company/acme/", base(port)))
        .await
        .unwrap();
    assert_eq!(company.name.as_deref(), Some("Acme Corp"));
    assert_eq!(company.industry.as_deref(), Some("Software Development"));
    assert_eq!(company.headquarters.as_deref(), Some("Austin, Texas"));
    assert_eq!(
        company.company_size.as_deref(),
        Some("1,001-5,000 employees")
    );
    assert_eq!(company.website.as_deref(), Some("https://acme.example"));
    assert_eq!(company.about_us.as_deref(), Some("We make everything."));

    let job = JobScraper::new(page.clone())
        .scrape(&format!("{}/jobs/view/1/", base(port)))
        .await
        .unwrap();
    assert_eq!(job.job_title.as_deref(), Some("Rust Developer"));
    assert_eq!(job.company.as_deref(), Some("Acme Corp"));
    assert_eq!(job.location.as_deref(), Some("Lyon, France"));
    assert_eq!(job.posted_date.as_deref(), Some("3 days ago"));
    assert_eq!(job.applicant_count.as_deref(), Some("27 applicants"));
    assert_eq!(
        job.company_linkedin_url.as_deref(),
        Some("https://www.linkedin.com/company/acme/life/")
    );
    assert!(
        job.job_description
            .unwrap()
            .contains("Write fast and safe code.")
    );

    let posts = CompanyPostsScraper::new(page.clone())
        .scrape(&format!("{}/company/acme/", base(port)), 2)
        .await
        .unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].urn.as_deref(), Some("urn:li:activity:111"));
    assert_eq!(posts[0].posted_date.as_deref(), Some("2d"));
    assert_eq!(posts[0].reactions_count, Some(1234));
    assert_eq!(posts[0].comments_count, Some(56));
    assert_eq!(posts[0].reposts_count, Some(7));
    assert_eq!(posts[1].posted_date.as_deref(), Some("1w"));
    assert!(posts[1].text.as_deref().unwrap().starts_with("Hiring"));

    // Validation errors surface like Pydantic's.
    let err = CompanyScraper::new(page)
        .scrape(&format!("{}/jobs/view/1/", base(port)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");

    browser.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_roundtrip() {
    let port = serve().await;
    let path = std::env::temp_dir().join(format!("lsrs-session-{}.json", std::process::id()));

    {
        let Some(mut browser) = launch(port).await else {
            return;
        };
        let page = browser.page().unwrap();
        page.goto(
            &format!("{}/cookies", base(port)),
            Default::default(),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        browser.save_session(&path).await.unwrap();
        browser.close().await;
    }

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        saved["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "li_at")
    );
    assert_eq!(saved["origins"][0]["localStorage"][0]["name"], "theme");

    let Some(mut browser) = launch(port).await else {
        return;
    };
    browser.load_session(&path).await.unwrap();
    assert!(browser.is_authenticated());
    let page = browser.page().unwrap();
    page.goto(
        &format!("{}/check", base(port)),
        Default::default(),
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    let out = page.locator("#out").inner_text().await.unwrap();
    assert_eq!(out, "li_at=SECRET|dark");

    browser.close().await;
    let _ = std::fs::remove_file(&path);
}

// ==========================================================================
// Name resolution (search pages). Fixtures: tests/fixtures/*.html, built from
// the real DOM observed on 2026-11-13 with fictitious contents.
// ==========================================================================

const SEARCH_COMPANIES: &str = include_str!("fixtures/search_companies.html");
const SEARCH_PEOPLE: &str = include_str!("fixtures/search_people.html");
const COMPANY_PAGE_ID: &str = include_str!("fixtures/company_page_id.html");
const SEARCH_LIMIT: &str = include_str!("fixtures/search_limit.html");

/// People search restricted to the company 12345678: the fixture without the
/// homonym who works elsewhere.
fn people_filtered() -> String {
    let marker = r#"<div role="listitem"><div data-display-contents="true"><a href="https://www.linkedin.com/in/jeanne-dupont-99/""#;
    let end_marker = r#"<hr role="presentation"></div>"#;
    let start = SEARCH_PEOPLE.find(marker).expect("homonym item");
    let end = start + SEARCH_PEOPLE[start..].find(end_marker).unwrap() + end_marker.len();
    format!("{}{}", &SEARCH_PEOPLE[..start], &SEARCH_PEOPLE[end..])
}

/// `/about/` page of a search candidate (old Ember DOM, as observed).
fn about_page(name: &str, website: &str, id: u64) -> String {
    format!(
        r#"<main>
  <section class="org-top-card">
    <h1 class="org-top-card-summary__title">{name}</h1>
    <div class="org-top-card-summary-info-list">
      <div class="org-top-card-summary-info-list__info-item">Collecte des déchets</div>
      <a href="/search/results/people/?currentCompany=%5B%22{id}%22%5D&amp;origin=COMPANY_PAGE_CANNED_SEARCH" class="org-top-card-summary-info-list__info-item">11-50 employés</a>
    </div>
  </section>
  <section><h2>Vue d’ensemble</h2><p>Nous compostons.</p>
    <dl>
      <dt><h3>Site web</h3></dt><dd><a href="{website}"><span>{website}</span></a></dd>
      <dt><h3>Secteur</h3></dt><dd>Collecte des déchets</dd>
      <dt><h3>Siège social</h3></dt><dd>Toulon, Provence-Alpes-Côte d’Azur</dd>
    </dl>
  </section>
</main>"#
    )
}

/// Routes that depend on the query string (search pages), else `None`.
fn route_search(full: &str) -> Option<(u16, String)> {
    let full = full.strip_prefix("/fake.linkedin.com").unwrap_or(full);
    let (path, query) = full.split_once('?').unwrap_or((full, ""));
    let body = match path {
        "/search/results/companies/" if query.contains("keywords=Limit") => page(SEARCH_LIMIT),
        "/search/results/companies/" => page(SEARCH_COMPANIES),
        "/search/results/people/" if query.contains("currentCompany=%5B%2212345678%22%5D") => {
            page(&people_filtered())
        }
        "/search/results/people/" => page(SEARCH_PEOPLE),
        "/company/acme-compost/" => page(COMPANY_PAGE_ID),
        // Same page without the header link: only the URN majority is left.
        "/company/no-header/" => {
            page(&COMPANY_PAGE_ID.replace("COMPANY_PAGE_CANNED_SEARCH", "OTHER_ORIGIN"))
        }
        "/company/acme-compost/about/" => page(&about_page(
            "Acme Compost",
            "https://www.acme-compost.fr/",
            12345678,
        )),
        "/company/acme-compost-sas/about/" => page(&about_page(
            "Acme Compost",
            "https://www.acme-conseil.com/",
            22222222,
        )),
        "/company/acme-compost-lyon/about/" => page(&about_page(
            "Acme Compost Lyon",
            "https://lyon.example.org/",
            33333333,
        )),
        _ => return None,
    };
    Some((200, body))
}

#[tokio::test(flavor = "multi_thread")]
async fn search_pages() {
    let port = serve().await;
    let Some(mut browser) = launch(port).await else {
        return;
    };
    let page = browser.page().unwrap();

    // Company search: 4 results, sidebar decoy ignored, fields by structure.
    let hits = CompanySearch::new(page.clone())
        .with_base_url(base(port))
        .search("Acme Compost", 10)
        .await
        .unwrap();
    let slugs: Vec<&str> = hits.iter().map(|h| h.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec![
            "acme-compost",
            "acme-compost-sas",
            "acme-compost-lyon",
            "acmefilms"
        ]
    );
    assert_eq!(hits[0].name, "Acme Compost");
    assert_eq!(
        hits[0].url,
        "https://www.linkedin.com/company/acme-compost/"
    );
    assert_eq!(hits[0].subtitle.as_deref(), Some("Collecte des déchets"));
    assert_eq!(
        hits[0].location.as_deref(),
        Some("Toulon, Provence-Alpes-Côte d’Azur")
    );
    assert_eq!(hits[0].followers, Some(1204));
    assert_eq!(hits[1].followers, Some(26388));
    assert_eq!(hits[3].location.as_deref(), Some("France"));

    // Company id: the header link wins over the decoy link and the URNs…
    let search = CompanySearch::new(page.clone()).with_base_url(base(port));
    let ev = search
        .company_id_evidence("https://www.linkedin.com/company/acme-compost/")
        .await
        .unwrap();
    assert_eq!(ev.header, Some(12345678));
    assert_eq!(ev.name.as_deref(), Some("Acme Compost"));
    assert_eq!(ev.pick().unwrap().source, IdSource::HeaderLink);
    // …and without it, the URN majority is used (and flagged as such).
    let ev = search
        .company_id_evidence("https://www.linkedin.com/company/no-header/")
        .await
        .unwrap();
    assert_eq!(ev.header, None);
    let id = ev.pick().unwrap();
    assert_eq!((id.id, id.source), (12345678, IdSource::UrnMajority));

    // People search with the currentCompany filter: private member dropped,
    // encoded slug canonicalized.
    let people = PeopleSearch::new(page.clone()).with_base_url(base(port));
    let hits = people
        .search(PeopleSearchParams {
            company_id: Some(12345678),
            ..PeopleSearchParams::new("Dupont")
        })
        .await
        .unwrap();
    let urls: Vec<&str> = hits.iter().map(|h| h.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            "https://www.linkedin.com/in/jeanne-dupont-1a2b3c4d/",
            "https://www.linkedin.com/in/j%C3%A9r%C3%B4me-dupont-lef%C3%A8vre-5e6f7a8b/"
        ]
    );
    assert_eq!(hits[0].name, "Jeanne Dupont");
    assert_eq!(
        hits[0].headline.as_deref(),
        Some("Directrice générale chez Acme Compost")
    );
    assert_eq!(
        hits[0].location.as_deref(),
        Some("Toulon, Provence-Alpes-Côte d’Azur, France")
    );
    assert_eq!(hits[1].slug, "jérôme-dupont-lefèvre-5e6f7a8b");
    // Unfiltered: both homonyms (+ Jérôme), still no private member.
    let all = people
        .search(PeopleSearchParams::new("Jeanne Dupont"))
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    // Monthly search limit page → dedicated error.
    let err = CompanySearch::new(page)
        .with_base_url(base(port))
        .search("Limit", 10)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SearchLimitReached(_)), "{err:?}");

    browser.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_decisions() {
    let port = serve().await;
    let Some(mut browser) = launch(port).await else {
        return;
    };
    let mut r =
        Resolver::new(browser.page().unwrap(), ResolveConfig::default()).with_base_url(base(port));

    // Two companies with the same name and no hint → Ambiguous, never the first hit.
    let res = r
        .resolve_company(&CompanyQuery::new("Acme Compost"))
        .await
        .unwrap();
    let Resolution::Ambiguous { candidates } = &res else {
        panic!("{res:?}")
    };
    let slugs: Vec<&str> = candidates.iter().map(|c| c.item.slug.as_str()).collect();
    assert_eq!(slugs[..2], ["acme-compost", "acme-compost-sas"]);

    // The website domain settles it (checked on /about/, which also gives the id).
    let q = CompanyQuery {
        domain: Some("acme-compost.fr".into()),
        ..CompanyQuery::new("Acme Compost")
    };
    let pages_before = r.pages_loaded();
    let res = r.resolve_company(&q).await.unwrap();
    let best = res.best().unwrap_or_else(|| panic!("{res:?}"));
    assert_eq!(
        best.item.url,
        "https://www.linkedin.com/company/acme-compost/"
    );
    assert_eq!(best.score, 1.0);
    assert!(
        best.reasons.iter().any(|x| x.contains("acme-compost.fr")),
        "{:?}",
        best.reasons
    );
    assert_eq!(best.item.company_id.map(|c| c.id), Some(12345678));
    // 1 search + 1 /about/ (stops as soon as the domain matched).
    assert_eq!(r.pages_loaded() - pages_before, 2);

    // Cached: no new page.
    let before = r.pages_loaded();
    assert_eq!(r.resolve_company(&q).await.unwrap(), res);
    assert_eq!(r.pages_loaded(), before);

    // Person with a company resolved by name → currentCompany filter.
    let pq = PersonQuery {
        company: Some(CompanyRef::Name(q.clone())),
        title: Some("directrice".into()),
        ..PersonQuery::new("Jeanne Dupont")
    };
    let res = r.resolve_person(&pq).await.unwrap();
    let best = res.best().unwrap_or_else(|| panic!("{res:?}"));
    assert_eq!(
        best.item.url,
        "https://www.linkedin.com/in/jeanne-dupont-1a2b3c4d/"
    );
    assert!(
        best.reasons
            .iter()
            .any(|x| x.contains("currentCompany=12345678")),
        "{:?}",
        best.reasons
    );

    // Homonyms without any hint → Ambiguous.
    let res = r
        .resolve_person(&PersonQuery::new("Jeanne Dupont"))
        .await
        .unwrap();
    assert_eq!(res.status(), "ambiguous", "{res:?}");
    assert_eq!(res.candidates().len(), 2);

    // Company given by URL with only the URN id: both searches are run and
    // compared; the id is confirmed and the right Jeanne wins.
    let pq = PersonQuery {
        company: Some(CompanyRef::Url(
            "https://www.linkedin.com/company/no-header/".into(),
        )),
        ..PersonQuery::new("Jeanne Dupont")
    };
    let res = r.resolve_person(&pq).await.unwrap();
    let best = res.best().unwrap_or_else(|| panic!("{res:?}"));
    assert_eq!(
        best.item.url,
        "https://www.linkedin.com/in/jeanne-dupont-1a2b3c4d/"
    );
    assert!(
        best.reasons
            .iter()
            .any(|x| x.contains("URN majoritaire") && x.contains("confirmé")),
        "{:?}",
        best.reasons
    );

    browser.close().await;
}

/// Non-regression: a batch with URL lines only behaves exactly as before
/// (same planning, same records, no resolution cache, no browser when
/// everything is already done).
#[test]
fn batch_with_urls_only_is_unchanged() {
    let dir = std::env::temp_dir().join(format!("lkbatch-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("urls.txt");
    let output = dir.join("out.jsonl");
    std::fs::write(
        &input,
        "# comment\nhttps://www.linkedin.com/in/jane/\nhttps://fr.linkedin.com/company/acme?trk=x\nhttps://example.com/x\n",
    )
    .unwrap();
    let before = concat!(
        r#"{"data":{"linkedin_url":"https://www.linkedin.com/in/jane/"},"kind":"person","scraped_at":"2026-01-01T00:00:00Z","status":"ok","url":"https://www.linkedin.com/in/jane/"}"#,
        "\n",
        r#"{"data":{"linkedin_url":"https://www.linkedin.com/company/acme/"},"kind":"company","scraped_at":"2026-01-01T00:00:00Z","status":"ok","url":"https://www.linkedin.com/company/acme/"}"#,
        "\n"
    );
    std::fs::write(&output, before).unwrap();

    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_linkedin_batch"))
            .current_dir(&dir)
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .arg("--session")
            .arg(dir.join("no-session.json"))
            .output()
            .unwrap()
    };

    // 1st run: the invalid line is recorded once, with the historical shape.
    let out = run();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "📋 3 URLs in file · 2 already done · 0 given up after 3 attempts · 0 to scrape now"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("Nothing to do."), "{stderr}");
    let after = std::fs::read_to_string(&output).unwrap();
    assert!(after.starts_with(before));
    let added: serde_json::Value = serde_json::from_str(after[before.len()..].trim()).unwrap();
    let mut keys: Vec<&str> = added
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["error", "kind", "scraped_at", "status", "url"]);
    assert_eq!(added["url"], "https://example.com/x");
    assert_eq!(added["status"], "skipped");
    assert_eq!(added["error"], "unsupported URL");

    // 2nd run: nothing changes, and no resolution cache is created.
    let snapshot = std::fs::read_to_string(&output).unwrap();
    assert!(run().status.success());
    assert_eq!(std::fs::read_to_string(&output).unwrap(), snapshot);
    assert!(!dir.join("resolutions.json").exists());
    std::fs::remove_dir_all(&dir).ok();
}
