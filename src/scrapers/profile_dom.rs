//! Extraction for the 2025+ LinkedIn profile DOM ("SDUI").
//!
//! The new UI uses obfuscated CSS classes, has no `<h1>` and no
//! `data-view-name` attributes, and its text depends on the UI language. This
//! module therefore relies only on stable, language-independent hooks:
//!
//! * `figure` elements (company / school logos) to find list entries;
//! * `[data-testid="expandable-text-box"]` for descriptions;
//! * `[componentkey$="About"]` for the About card;
//! * the `/overlay/contact-info/` link to locate the top card.
//!
//! JavaScript only collects ordered text tokens; all the interpretation is
//! done in Rust (pure functions, unit-tested below).

use serde::Deserialize;

use super::person::{map_contact_heading_to_type, parse_education_times, parse_work_times};
use crate::models::{Accomplishment, Contact, Education, Experience, Interest};

/// One text token of a list entry, in document order.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "lowercase")]
pub(crate) enum Token {
    /// A `<p>` line (title, company, dates, location…).
    Line(String),
    /// An expandable description box.
    Desc(String),
}

/// One list entry on a `details/...` page.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawEntry {
    /// Company / school link of the entry.
    pub url: Option<String>,
    /// Every other link of the entry (profile, credential, publication…).
    #[serde(default)]
    pub links: Vec<String>,
    pub tokens: Vec<Token>,
}

/// One row of the contact-info dialog.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawContact {
    pub heading: String,
    #[serde(default)]
    pub links: Vec<RawLink>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLink {
    pub href: String,
    pub text: String,
    pub label: Option<String>,
}

/// Top card + About of the main profile page.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TopCard {
    pub name: Option<String>,
    pub headline: Option<String>,
    pub location: Option<String>,
    pub about: Option<String>,
    #[serde(default)]
    pub open_to_work: bool,
}

/// Shared JS: `extract(mode)` returns `RawEntry[]` for the main list of a
/// `details/...` page (the sidebar `<aside>` is ignored).
///
/// * `"figure"`: one entry per logo (experience, education, interests);
/// * `"list"`: entries separated by `<hr>` (languages, certifications…). An
///   empty list shows an `<h2>` ("Nothing to see for now"), while the page
///   title is a `<p>`: no `<hr>` + an `<h2>` means "empty", no `<hr>` and no
///   `<h2>` means a single entry (whose first line, the title, is dropped).
const ENTRIES_FN_JS: &str = r#"((mode) => {
  const main = document.querySelector('main') || document.body;
  const BOX = '[data-testid="expandable-text-box"]';
  const SKILLS = 'a[href*="skill-associations"]';
  const outside = el => el.closest('aside') || el.closest('dialog');
  let items = [], dropHeading = false;

  if (mode === 'figure') {
    const firstFig = [...main.querySelectorAll('figure')].find(f => !outside(f));
    if (!firstFig) return [];
    const root = firstFig.closest('section') || main;
    const seen = new Set();
    // Never climb into the page title (first <p> of the section) or the
    // category chips: with a single entry there is no other figure to stop us.
    const title = root.querySelector('p');
    const extra = (p, el) => (title && !el.contains(title) && p.contains(title))
      || [...p.querySelectorAll('input')].some(x => !el.contains(x));
    for (const fig of root.querySelectorAll('figure')) {
      let el = fig;
      while (el.parentElement && el !== root && !el.querySelector('p')) el = el.parentElement;
      while (el.parentElement && el.parentElement !== root
             && el.parentElement.querySelectorAll('figure').length === 1
             && !extra(el.parentElement, el)) el = el.parentElement;
      if (seen.has(el)) continue;
      seen.add(el);
      items.push(el);
    }
  } else {
    const root = [...main.querySelectorAll('section')].find(s =>
      !outside(s) && !s.querySelector('section') && s.querySelector('p')
      && !s.querySelector('[data-testid="toasts-title"]'));
    if (!root) return [];
    const hr = root.querySelector('hr');
    if (hr) {
      let list = hr.parentElement;
      if (list.firstElementChild === hr && list.children.length > 1) list = list.parentElement;
      items = [...list.children].filter(c => c.tagName !== 'HR' && (c.innerText || '').trim());
    } else if (!root.querySelector('h2')) {
      items = [root];
      dropHeading = true;
    }
  }

  return items.map(el => {
    const tokens = [];
    for (const n of el.querySelectorAll('p, ' + BOX)) {
      if (n.matches(BOX)) {
        const t = (n.innerText || '').trim();
        if (t) tokens.push({ kind: 'desc', text: t });
        continue;
      }
      if (n.closest(BOX) || n.querySelector(BOX)) continue;
      if (n.closest(SKILLS) || n.querySelector(SKILLS)) continue;
      const t = (n.innerText || '').replace(/\s+/g, ' ').trim();
      if (t && t !== '·') tokens.push({ kind: 'line', text: t });
    }
    if (dropHeading) tokens.shift();
    const a = el.querySelector('a[href*="/company/"], a[href*="/school/"]');
    const links = [...new Set([...el.querySelectorAll('a[href]')]
      .map(x => x.href.split('?')[0])
      .filter(h => /^https?:/.test(h) && !h.includes('skill-associations') && !h.includes('/overlay/')))];
    return { url: a ? a.href.split('?')[0] : null, links, tokens };
  });
})"#;

/// JS expression returning `RawEntry[]` for `mode` (`"figure"` or `"list"`).
pub(crate) fn extract_entries_js(mode: &str) -> String {
    format!("{ENTRIES_FN_JS}({mode:?})")
}

/// JS expression: the interest category chips (labels of checkboxes in the
/// main content, not the footer language picker nor the sidebar).
const CHIPS_EXPR_JS: &str = r"[...(document.querySelector('main') || document).querySelectorAll('label')]
  .filter(l => {
    const box = (l.htmlFor && document.getElementById(l.htmlFor)) || l.querySelector('input')
      || l.previousElementSibling;
    return box && box.type === 'checkbox' && !l.closest('aside, footer, dialog, nav');
  })";

/// Labels of the interest category chips.
pub(crate) fn interest_chips_js() -> String {
    format!("{CHIPS_EXPR_JS}.map(l => (l.innerText || '').trim())")
}

/// Selects chip `i` alone (clicks it if needed, unselects the others).
pub(crate) fn select_interest_chip_js(i: usize) -> String {
    format!(
        r"(() => {{
  const ls = {CHIPS_EXPR_JS};
  const box = l => (l.htmlFor && document.getElementById(l.htmlFor)) || l.querySelector('input') || l.previousElementSibling;
  if (ls[{i}] && !box(ls[{i}]).checked) ls[{i}].click();
  ls.forEach((l, j) => {{ if (j !== {i} && box(l).checked) l.click(); }});
}})()"
    )
}

/// Rows of the open contact-info dialog.
pub(crate) const EXTRACT_CONTACTS_JS: &str = r#"(() => {
  const sec = document.querySelector('[componentkey$="ContactInfoDetailSection"]');
  if (!sec) return null;
  const col = sec.querySelector('[data-testid="lazy-column"]') || sec;
  const clean = s => (s || '').replace(/\s+/g, ' ').trim();
  return [...col.children].map(row => {
    const ps = [...row.querySelectorAll('p')].map(p => clean(p.innerText)).filter(Boolean);
    const links = [...row.querySelectorAll('a[href]')].map(a => {
      const around = clean((a.closest('p') || a.parentElement).innerText);
      const m = around.match(/\(([^)]+)\)\s*$/);
      return { href: a.getAttribute('href'), text: clean(a.innerText), label: m ? m[1] : null };
    });
    return { heading: ps[0] || '', links, text: ps.slice(1).join(' ') };
  }).filter(r => r.heading || r.links.length);
})()"#;

/// Returns a [`TopCard`] for the main profile page.
pub(crate) const EXTRACT_TOP_CARD_JS: &str = r##"(() => {
  const txt = e => e ? (e.innerText || '').replace(/\s+/g, ' ').trim() || null : null;
  let name = txt(document.querySelector('[componentkey^="ProfileVerificationTriggerRef"] h2'))
          || txt(document.querySelector('main h1'));
  if (!name) name = (document.title || '').replace(/^\(\d+\)\s*/, '').split(' | ')[0].trim() || null;

  let headline = null, location = null;
  const ci = document.querySelector('a[href*="/overlay/contact-info"]');
  if (ci) {
    const locBox = (ci.closest('p') || ci).parentElement;
    location = txt(locBox && locBox.querySelector('p'));
    const card = locBox && locBox.parentElement;
    if (card) headline = txt(card.querySelector(':scope > p'));
  }

  let about = null;
  const aboutCard = document.querySelector('[componentkey$="About"]');
  if (aboutCard) {
    const box = aboutCard.querySelector('[data-testid="expandable-text-box"]');
    about = box ? (box.innerText || '').trim() || null : null;
  }

  const open_to_work = !!document.querySelector(
    'img[title*="OPEN_TO_WORK" i], img[alt*="#OPEN_TO_WORK" i], img[alt*="Open to work" i]');
  return { name, headline, location, about, open_to_work };
})()"##;

/// Badges displayed next to the current position ("Aujourd’hui", "Present"…).
fn is_badge(line: &str) -> bool {
    let l = line.trim().to_lowercase().replace('’', "'");
    matches!(
        l.as_str(),
        "aujourd'hui" | "present" | "today" | "actuel" | "heute" | "hoy" | "presente"
    )
}

/// Contains a standalone 4-digit year 19xx / 20xx.
fn has_year(line: &str) -> bool {
    let b = line.as_bytes();
    b.windows(4).enumerate().any(|(i, w)| {
        w.iter().all(u8::is_ascii_digit)
            && (w[0] == b'1' && w[1] == b'9' || w[0] == b'2' && w[1] == b'0')
            && !b.get(i + 4).is_some_and(u8::is_ascii_digit)
            && !(i > 0 && b[i - 1].is_ascii_digit())
    })
}

/// Looks like `"janv. 2024 - aujourd’hui · 2 ans"`, `"2020 – 2024"`, `"août 2022 · 1 mois"`.
pub(crate) fn is_date_line(line: &str) -> bool {
    if !has_year(line) {
        return false;
    }
    let short = line.chars().count() <= 60;
    short && (line.contains(" - ") || line.contains('–') || line.contains('·') || line.len() <= 12)
}

/// Removes the "… plus" / "…see more" toggle text glued to truncated descriptions.
fn clean_desc(d: &str) -> String {
    let mut d = d.trim().to_string();
    for suffix in [
        "… plus",
        "…plus",
        "… voir plus",
        "… see more",
        "…see more",
        "… more",
        "…more",
    ] {
        if let Some(stripped) = d.strip_suffix(suffix) {
            d = stripped.trim_end().to_string();
        }
    }
    d
}

/// Splits `"Wavestone · CDI"` into `("Wavestone", Some("CDI"))`.
fn split_company(line: &str) -> (String, Option<String>) {
    match line.split_once('·') {
        Some((c, t)) if !t.trim().is_empty() => (c.trim().to_string(), Some(t.trim().to_string())),
        _ => (line.trim().to_string(), None),
    }
}

fn lines_and_descs(tokens: &[Token]) -> Vec<Token> {
    tokens
        .iter()
        .filter(|t| !matches!(t, Token::Line(l) if is_badge(l)))
        .cloned()
        .collect()
}

/// Turns one entry into one experience, or several for grouped positions
/// (one company, several roles).
pub(crate) fn parse_experience_entry(entry: &RawEntry) -> Vec<Experience> {
    let tokens = lines_and_descs(&entry.tokens);
    let lines: Vec<(usize, &str)> = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, t)| match t {
            Token::Line(l) => Some((i, l.as_str())),
            Token::Desc(_) => None,
        })
        .collect();
    let date_idx: Vec<usize> = (0..lines.len())
        .filter(|&i| is_date_line(lines[i].1))
        .collect();
    if date_idx.is_empty() || lines.is_empty() {
        return vec![];
    }

    // Description tokens that follow line `li` (until the next date line's title).
    let desc_after = |token_pos: usize, until: usize| -> Option<String> {
        let d: Vec<&str> = tokens[token_pos..until]
            .iter()
            .filter_map(|t| match t {
                Token::Desc(d) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        (!d.is_empty()).then(|| clean_desc(&d.join("\n\n")))
    };

    let mk = |title: &str,
              company: &str,
              ty: Option<String>,
              date: &str,
              loc: Option<&str>,
              desc: Option<String>| {
        let (from_date, to_date, duration) = parse_work_times(date);
        Experience {
            position_title: Some(title.to_string()),
            institution_name: (!company.is_empty()).then(|| company.to_string()),
            employment_type: ty,
            linkedin_url: entry.url.clone(),
            from_date,
            to_date: to_date.filter(|t| !t.is_empty()),
            duration,
            location: loc.map(str::to_string),
            description: desc,
        }
    };

    if date_idx.len() == 1 {
        let k = date_idx[0];
        let title = lines[0].1;
        let (company, ty) = if k >= 2 {
            split_company(lines[1].1)
        } else {
            (String::new(), None)
        };
        let loc = lines.get(k + 1).map(|l| l.1);
        let start = lines[k].0 + 1;
        return vec![mk(
            title,
            &company,
            ty,
            lines[k].1,
            loc,
            desc_after(start, tokens.len()),
        )];
    }

    // Grouped positions: [Company, (type · total)?, T1, D1, (L1)?, T2, D2, …]
    let (company, group_type) = (lines[0].1.to_string(), None::<String>);
    let mut out = Vec::new();
    for (n, &k) in date_idx.iter().enumerate() {
        if k == 0 {
            continue;
        }
        let title = lines[k - 1].1;
        let next_title = date_idx.get(n + 1).map_or(lines.len(), |&nk| nk - 1);
        let loc = (k + 1 < next_title).then(|| lines[k + 1].1);
        let start = lines[k].0 + 1;
        let end = if next_title < lines.len() {
            lines[next_title].0
        } else {
            tokens.len()
        };
        out.push(mk(
            title,
            &company,
            group_type.clone(),
            lines[k].1,
            loc,
            desc_after(start, end),
        ));
    }
    out
}

/// Turns one entry into an education: `[school, degree?, dates?]`.
pub(crate) fn parse_education_entry(entry: &RawEntry) -> Option<Education> {
    let tokens = lines_and_descs(&entry.tokens);
    let lines: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Line(l) => Some(l.as_str()),
            _ => None,
        })
        .collect();
    let school = *lines.first()?;
    let date_pos = lines.iter().position(|l| is_date_line(l));
    let degree_end = date_pos.unwrap_or(lines.len().min(2));
    let degree = lines
        .get(1..degree_end)
        .map(|d| d.join(", "))
        .filter(|d| !d.trim().is_empty());
    let (from_date, to_date) = date_pos.map_or((None, None), |i| parse_education_times(lines[i]));
    let desc: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Desc(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    Some(Education {
        institution_name: Some(school.to_string()),
        degree: degree.map(|d| d.trim().to_string()),
        linkedin_url: entry.url.clone(),
        from_date,
        to_date,
        description: (!desc.is_empty()).then(|| clean_desc(&desc.join("\n\n"))),
    })
}

// --------------------------------------------------------------------------
// Accomplishments, interests, contacts
// --------------------------------------------------------------------------

const ISSUED_BY: [&str; 13] = [
    "issued by",
    "délivré par",
    "délivrée par",
    "émis par",
    "émise par",
    "décerné par",
    "décernée par",
    "attribué par",
    "otorgado por",
    "expedido por",
    "emitido por",
    "ausgestellt von",
    "rilasciato da",
];
const ISSUED: [&str; 18] = [
    "issued",
    "délivré le",
    "délivrée le",
    "délivré en",
    "délivrée en",
    "délivrée",
    "délivré",
    "émis le",
    "émise le",
    "émis en",
    "émise en",
    "obtenu en",
    "obtenue en",
    "fecha de expedición",
    "expedición",
    "emitido",
    "ausgestellt",
    "rilasciato",
];
const CREDENTIAL_ID: [&str; 9] = [
    "credential id",
    "id de la référence",
    "id de référence",
    "id de la certification",
    "id de la qualification",
    "id de credencial",
    "id. de la credencial",
    "nachweis-id",
    "id credenziale",
];

/// `line` without the first matching (case-insensitive) prefix, if any.
fn strip_prefix_ci(line: &str, prefixes: &[&str]) -> Option<String> {
    let lower = line.to_lowercase();
    prefixes.iter().find(|p| lower.starts_with(*p)).map(|p| {
        line.chars()
            .skip(p.chars().count())
            .collect::<String>()
            .trim_start_matches([':', ' ', '\u{a0}'])
            .trim()
            .to_string()
    })
}

/// `"janv. 2023 · Expire en janv. 2026"` → `"janv. 2023"`.
fn date_part(s: &str) -> String {
    let s = strip_prefix_ci(s.trim(), &ISSUED).unwrap_or_else(|| s.trim().to_string());
    s.split('·').next().unwrap_or("").trim().to_string()
}

fn entry_lines(e: &RawEntry) -> (Vec<String>, Option<String>) {
    let mut lines = Vec::new();
    let mut descs = Vec::new();
    for t in &e.tokens {
        match t {
            Token::Line(l) if !is_badge(l) => lines.push(l.clone()),
            Token::Desc(d) => descs.push(d.as_str()),
            _ => {}
        }
    }
    let desc = (!descs.is_empty()).then(|| clean_desc(&descs.join("\n\n")));
    (lines, desc)
}

/// One entry of `details/<certifications|honors|languages|projects|…>`.
pub(crate) fn parse_accomplishment_entry(e: &RawEntry, category: &str) -> Option<Accomplishment> {
    let (lines, desc) = entry_lines(e);
    let title = lines.first()?.trim().to_string();
    if title.is_empty() || title.chars().count() > 200 {
        return None;
    }
    let mut acc = Accomplishment {
        category: category.to_string(),
        title,
        ..Accomplishment::default()
    };
    if category == "language" {
        // [language, proficiency]
        acc.description = lines.get(1).cloned();
        return Some(acc);
    }
    for (i, line) in lines.iter().enumerate().skip(1) {
        if let Some(id) = strip_prefix_ci(line, &CREDENTIAL_ID) {
            acc.credential_id = Some(id).filter(|s| !s.is_empty());
        } else if let Some(rest) = strip_prefix_ci(line, &ISSUED_BY) {
            let mut parts = rest.splitn(2, '·');
            acc.issuer = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(d) = parts.next().filter(|d| has_year(d)) {
                acc.issued_date.get_or_insert(date_part(d));
            }
        } else if has_year(line) && acc.issued_date.is_none() {
            // "Issued Jan 2023", "Publisher · Mar 2020", "janv. 2020 - mars 2020"
            match line.split_once('·') {
                Some((before, after)) if !has_year(before) && has_year(after) => {
                    if i == 1 && acc.issuer.is_none() {
                        acc.issuer = Some(before.trim().to_string());
                    }
                    acc.issued_date = Some(date_part(after));
                }
                _ => acc.issued_date = Some(date_part(line)),
            }
        } else if i == 1 && acc.issuer.is_none() {
            acc.issuer = Some(line.split('·').next().unwrap_or(line).trim().to_string());
        }
    }
    acc.credential_url = e
        .links
        .iter()
        .find(|l| l.contains("credential") || l.contains("verify"))
        .or_else(|| e.links.iter().find(|l| !l.contains("linkedin.com")))
        .cloned();
    acc.description = desc;
    Some(acc)
}

/// `"Liftia France, Entreprise Liftia France"` → `"Liftia France"`: the link
/// text includes a screen-reader label that repeats the name.
fn dedupe_a11y_name(raw: &str) -> String {
    let raw = raw.trim();
    if let Some((first, rest)) = raw.split_once(", ")
        && rest.trim_end().ends_with(first.trim())
    {
        return first.trim().to_string();
    }
    raw.to_string()
}

/// One entry of a `details/interests/` tab.
pub(crate) fn parse_interest_entry(e: &RawEntry, category: &str) -> Option<Interest> {
    let (lines, _) = entry_lines(e);
    let name = dedupe_a11y_name(lines.first()?);
    if name.is_empty() {
        return None;
    }
    Some(Interest {
        name,
        category: category.to_string(),
        linkedin_url: e.links.first().cloned().or_else(|| e.url.clone()),
    })
}

/// `"06 12 34 56 78 (Mobile)"` → `("06 12 34 56 78", Some("Mobile"))`.
fn split_trailing_label(text: &str) -> (String, Option<String>) {
    let t = text.trim();
    if let Some(stripped) = t.strip_suffix(')')
        && let Some(open) = stripped.rfind('(')
    {
        let label = stripped[open + 1..].trim();
        let value = stripped[..open].trim();
        if !value.is_empty() && !label.is_empty() {
            return (value.to_string(), Some(label.to_string()));
        }
    }
    (t.to_string(), None)
}

/// One row of the contact-info dialog → zero or more contacts.
pub(crate) fn parse_contact_row(row: &RawContact) -> Vec<Contact> {
    let heading_type = map_contact_heading_to_type(&row.heading);
    let mut out = Vec::new();
    for l in &row.links {
        let href = l.href.trim();
        let (ty, value) = if let Some(m) = href.strip_prefix("mailto:") {
            ("email", m.to_string())
        } else if let Some(t) = href.strip_prefix("tel:") {
            ("phone", t.to_string())
        } else if href.contains("linkedin.com/in/") {
            ("linkedin", href.to_string())
        } else {
            let ty = match heading_type {
                Some(t @ ("twitter" | "website")) => t,
                _ if href.contains("twitter.com") || href.contains("x.com/") => "twitter",
                _ => "website",
            };
            let v = if l.text.is_empty() {
                href.to_string()
            } else {
                l.text.clone()
            };
            (ty, v)
        };
        if !value.trim().is_empty() {
            out.push(Contact {
                contact_type: ty.to_string(),
                value: value.trim().to_string(),
                label: l.label.clone(),
            });
        }
    }
    if row.links.is_empty()
        && let Some(ty) = heading_type
        && !row.text.trim().is_empty()
    {
        let (value, label) = split_trailing_label(&row.text);
        out.push(Contact {
            contact_type: ty.to_string(),
            value,
            label,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accomplishments() {
        let a = parse_accomplishment_entry(
            &RawEntry {
                url: None,
                links: vec![
                    "https://www.linkedin.com/company/1/".into(),
                    "https://www.credly.com/badges/abc".into(),
                ],
                tokens: vec![
                    line("AWS Solutions Architect"),
                    line("Amazon Web Services (AWS)"),
                    line("Délivrée en janv. 2023 · Expire en janv. 2026"),
                    line("ID de la référence ABC-123"),
                ],
            },
            "certification",
        )
        .unwrap();
        assert_eq!(a.title, "AWS Solutions Architect");
        assert_eq!(a.issuer, s("Amazon Web Services (AWS)"));
        assert_eq!(a.issued_date, s("janv. 2023"));
        assert_eq!(a.credential_id, s("ABC-123"));
        assert_eq!(a.credential_url, s("https://www.credly.com/badges/abc"));

        let h = parse_accomplishment_entry(
            &entry(vec![
                line("Best paper"),
                line("Issued by ACM · Mar 2020"),
                desc("For X"),
            ]),
            "honor",
        )
        .unwrap();
        assert_eq!(h.issuer, s("ACM"));
        assert_eq!(h.issued_date, s("Mar 2020"));
        assert_eq!(h.description, s("For X"));

        let p = parse_accomplishment_entry(
            &entry(vec![line("Rust book"), line("O'Reilly · 12 mars 2021")]),
            "publication",
        )
        .unwrap();
        assert_eq!(p.issuer, s("O'Reilly"));
        assert_eq!(p.issued_date, s("12 mars 2021"));

        let l = parse_accomplishment_entry(
            &entry(vec![
                line("Anglais"),
                line("Capacité professionnelle complète"),
            ]),
            "language",
        )
        .unwrap();
        assert_eq!(l.title, "Anglais");
        assert_eq!(l.description, s("Capacité professionnelle complète"));
        assert_eq!(l.issuer, None);
    }

    #[test]
    fn interests() {
        let i = parse_interest_entry(
            &RawEntry {
                url: None,
                links: vec!["https://www.linkedin.com/in/christinelagarde/".into()],
                tokens: vec![
                    line("Christine Lagarde"),
                    line("· 3e et +"),
                    line("President of the ECB"),
                ],
            },
            "influencer",
        )
        .unwrap();
        assert_eq!(i.name, "Christine Lagarde");
        assert_eq!(
            dedupe_a11y_name("Liftia France, Entreprise Liftia France"),
            "Liftia France"
        );
        assert_eq!(
            dedupe_a11y_name("Lycée Hoche, École Lycée Hoche"),
            "Lycée Hoche"
        );
        assert_eq!(dedupe_a11y_name("Smith, Jones & Co"), "Smith, Jones & Co");
        assert_eq!(
            i.linkedin_url,
            s("https://www.linkedin.com/in/christinelagarde/")
        );
    }

    #[test]
    fn contacts() {
        let link = |href: &str, text: &str, label: Option<&str>| RawLink {
            href: href.into(),
            text: text.into(),
            label: label.map(Into::into),
        };
        let rows = [
            RawContact {
                heading: "Profil de Caroline".into(),
                links: vec![link(
                    "https://www.linkedin.com/in/caroline-witwer/",
                    "linkedin.com/in/caroline-witwer",
                    None,
                )],
                text: String::new(),
            },
            RawContact {
                heading: "Site web".into(),
                links: vec![link("https://acme.com", "acme.com", Some("Entreprise"))],
                text: String::new(),
            },
            RawContact {
                heading: "Adresse e-mail".into(),
                links: vec![link("mailto:a@b.fr", "a@b.fr", None)],
                text: String::new(),
            },
            RawContact {
                heading: "Téléphone".into(),
                links: vec![],
                text: "06 12 34 56 78 (Mobile)".into(),
            },
            RawContact {
                heading: "Anniversaire".into(),
                links: vec![],
                text: "14 juillet".into(),
            },
        ];
        let c: Vec<Contact> = rows.iter().flat_map(parse_contact_row).collect();
        let pairs: Vec<(&str, &str, Option<&str>)> = c
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
            pairs,
            vec![
                (
                    "linkedin",
                    "https://www.linkedin.com/in/caroline-witwer/",
                    None
                ),
                ("website", "acme.com", Some("Entreprise")),
                ("email", "a@b.fr", None),
                ("phone", "06 12 34 56 78", Some("Mobile")),
                ("birthday", "14 juillet", None),
            ]
        );
    }

    fn line(s: &str) -> Token {
        Token::Line(s.into())
    }
    fn desc(s: &str) -> Token {
        Token::Desc(s.into())
    }
    fn entry(tokens: Vec<Token>) -> RawEntry {
        RawEntry {
            url: Some("https://www.linkedin.com/company/1/".into()),
            links: vec![],
            tokens,
        }
    }
    fn s(v: &str) -> Option<String> {
        Some(v.into())
    }

    #[test]
    fn date_lines() {
        assert!(is_date_line("janv. 2024 - aujourd’hui · 2 ans 11 mois"));
        assert!(is_date_line("août 2022 · 1 mois"));
        assert!(is_date_line("2020 – 2024"));
        assert!(is_date_line("2015"));
        assert!(!is_date_line("Wavestone · CDI"));
        assert!(!is_date_line(
            "Marseille, Provence-Alpes-Côte d’Azur, France"
        ));
        assert!(!is_date_line("Promo 120345 · Paris"));
    }

    #[test]
    fn single_experience_fr() {
        let e = entry(vec![
            line("Consultante"),
            line("Wavestone · CDI"),
            line("janv. 2024 - aujourd’hui · 2 ans 11 mois"),
            line("Aujourd’hui"),
            line("Marseille, Provence-Alpes-Côte d’Azur, France"),
            desc("- Bilans carbone"),
        ]);
        let v = parse_experience_entry(&e);
        assert_eq!(v.len(), 1);
        let x = &v[0];
        assert_eq!(x.position_title, s("Consultante"));
        assert_eq!(x.institution_name, s("Wavestone"));
        assert_eq!(x.employment_type, s("CDI"));
        assert_eq!(x.from_date, s("janv. 2024"));
        assert_eq!(x.to_date, s("aujourd’hui"));
        assert_eq!(x.duration, s("2 ans 11 mois"));
        assert_eq!(
            x.location,
            s("Marseille, Provence-Alpes-Côte d’Azur, France")
        );
        assert_eq!(x.description, s("- Bilans carbone"));
    }

    #[test]
    fn single_experience_without_location() {
        let v = parse_experience_entry(&entry(vec![
            line("Auxiliaire d'été"),
            line("CMCIC FACTOR · CDD"),
            line("août 2022 · 1 mois"),
        ]));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].from_date, s("août 2022"));
        assert_eq!(v[0].location, None);
        assert_eq!(v[0].to_date, None);
    }

    #[test]
    fn desc_cleanup() {
        assert_eq!(clean_desc("-Capitaine\n… plus"), "-Capitaine");
        assert_eq!(clean_desc("Built things …see more"), "Built things");
    }

    #[test]
    fn grouped_experience() {
        let v = parse_experience_entry(&entry(vec![
            line("Acme"),
            line("CDI · 5 ans"),
            line("Manager"),
            line("2022 - aujourd’hui · 3 ans"),
            line("Paris"),
            desc("Lead team"),
            line("Engineer"),
            line("2020 - 2022 · 2 ans"),
        ]));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].position_title, s("Manager"));
        assert_eq!(v[0].institution_name, s("Acme"));
        assert_eq!(v[0].location, s("Paris"));
        assert_eq!(v[0].description, s("Lead team"));
        assert_eq!(v[1].position_title, s("Engineer"));
        assert_eq!(v[1].location, None);
        assert_eq!(v[1].description, None);
        assert_eq!(v[1].from_date, s("2020"));
    }

    #[test]
    fn no_dates_is_not_an_experience() {
        assert!(parse_experience_entry(&entry(vec![line("Alice"), line("· 3e et +")])).is_empty());
    }

    #[test]
    fn education_entries() {
        let e = parse_education_entry(&entry(vec![
            line("emlyon business school"),
            line("Programme grande école, Management"),
            line("2020 – 2024"),
            desc("Cheerleading"),
        ]))
        .unwrap();
        assert_eq!(e.institution_name, s("emlyon business school"));
        assert_eq!(e.degree, s("Programme grande école, Management"));
        assert_eq!(e.from_date, s("2020"));
        assert_eq!(e.to_date, s("2024"));
        assert_eq!(e.description, s("Cheerleading"));

        let e = parse_education_entry(&entry(vec![
            line("Aarhus BSS - Aarhus University"),
            line("sept. 2023 – déc. 2023"),
        ]))
        .unwrap();
        assert_eq!(e.degree, None);
        assert_eq!(e.from_date, s("sept. 2023"));
        assert_eq!(e.to_date, s("déc. 2023"));
    }
}
