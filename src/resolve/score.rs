//! Candidate scoring (pure functions).
//!
//! A score is the **weighted mean of the signals the query provides**, each in
//! `[0, 1]`. A signal the query asks for but the candidate cannot be checked
//! on (e.g. a website not verified yet) counts as neutral (0.5). Signals the
//! query does not provide are left out, so a name-only query can still reach
//! 1.0. Finally the score is capped by the name similarity when the latter is
//! low: a perfect company match never rescues a different name.
//!
//! | Company signal | weight | value |
//! |---|---|---|
//! | name | 1.0 | [`company_name_similarity`] |
//! | website domain | 2.0 | 1 if same registrable domain, 0 if different, 0.5 if unverified |
//! | location | 0.5 | 1 if a significant word matches, else 0 |
//! | industry | 0.4 | share of the query's words found |
//!
//! | Person signal | weight | value |
//! |---|---|---|
//! | name | 3.0 | [`person_name_similarity`] |
//! | current company | 2.0 | 1 if found through a trusted `currentCompany` filter or named in the headline, else 0.2 |
//! | title | 1.0 | 1 if a title word is in the headline; else 0.7 / 0.4 / 0 by seniority distance |
//! | location | 0.6 | as for companies |

use strsim::jaro_winkler;

use super::normalize::{PersonName, domain_of, normalize_company, normalize_text, tokens};
use super::{CompanyQuery, PersonQuery};
use crate::scrapers::{CompanyHit, PersonHit};
use crate::seniority::{self, Seniority};

/// Weighted mean of `(weight, value)`; `None` values are neutral (0.5).
fn weighted(signals: &[(f64, Option<f64>)]) -> f64 {
    let total: f64 = signals.iter().map(|(w, _)| w).sum();
    if total == 0.0 {
        return 0.0;
    }
    signals
        .iter()
        .map(|(w, v)| w * v.unwrap_or(0.5))
        .sum::<f64>()
        / total
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Same first `n` characters (both words at least `n` long).
fn same_prefix(a: &str, b: &str, n: usize) -> bool {
    a.chars().count() >= n && b.chars().count() >= n && a.chars().take(n).eq(b.chars().take(n))
}

/// Fuzzy token equality: 1 if equal, Jaro-Winkler if ≥ 0.92 (typos), else 0.
fn token_match(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let jw = jaro_winkler(a, b);
    if jw >= 0.92 { jw * 0.9 } else { 0.0 }
}

/// Similarity of two company names after [`normalize_company`]:
/// 1.0 when equal (legal forms, accents, "Les"… ignored); otherwise 0.85 ×
/// a fuzzy Dice coefficient on words, so that "Acme Compost" vs "Acme
/// Compost Lyon" (0.68) stays well below an exact match.
pub fn company_name_similarity(a: &str, b: &str) -> f64 {
    let (na, nb) = (normalize_company(a), normalize_company(b));
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb || na.replace(' ', "") == nb.replace(' ', "") {
        return 1.0;
    }
    let (ta, tb): (Vec<&str>, Vec<&str>) = (na.split(' ').collect(), nb.split(' ').collect());
    let matched: f64 = ta
        .iter()
        .map(|x| tb.iter().map(|y| token_match(x, y)).fold(0.0, f64::max))
        .sum();
    0.85 * (2.0 * matched / (ta.len() + tb.len()) as f64).min(1.0)
}

/// Similarity of two person names, order-insensitive:
/// each query part is matched to its best counterpart (same word 1.0, typo
/// ≤ 0.9, initial vs word 0.9), and the sum is divided by the larger number
/// of parts, so missing or extra first names lower the score
/// ("Jean Martin" vs "Jean-Baptiste Martin" = 0.67).
pub fn person_name_similarity(a: &PersonName, b: &PersonName) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a.key() == b.key() {
        return 1.0;
    }
    let part = |t: &str, other: &PersonName| -> f64 {
        let words = other
            .tokens
            .iter()
            .map(|o| token_match(t, o))
            .fold(0.0, f64::max);
        let initial = t
            .chars()
            .next()
            .filter(|c| other.initials.contains(c))
            .map_or(0.0, |_| 0.9);
        words.max(initial)
    };
    let mut sum: f64 = a.tokens.iter().map(|t| part(t, b)).sum();
    for i in &a.initials {
        let s = if b.initials.contains(i) {
            1.0
        } else if b.tokens.iter().any(|t| t.starts_with(*i)) {
            0.9
        } else {
            0.0
        };
        sum += s;
    }
    let n = (a.tokens.len() + a.initials.len()).max(b.tokens.len() + b.initials.len());
    (sum / n as f64).min(1.0)
}

/// Generic location words that prove nothing.
const GENERIC_PLACES: &[&str] = &[
    "france",
    "region",
    "area",
    "greater",
    "metropolitan",
    "grand",
    "peripherie",
    "et",
    "and",
    "de",
    "du",
    "des",
    "la",
    "le",
    "les",
    "sur",
    "en",
    "province",
];

/// 1 if a significant word of `query` is in `candidate`, 0 otherwise;
/// `None` if the candidate location is unknown or the query has no
/// significant word.
pub fn location_match(query: &str, candidate: Option<&str>) -> Option<f64> {
    let q: Vec<String> = tokens(query)
        .into_iter()
        .filter(|t| t.len() >= 3 && !GENERIC_PLACES.contains(&t.as_str()))
        .collect();
    let c = tokens(candidate?);
    if q.is_empty() || c.is_empty() {
        return None;
    }
    Some(
        if q.iter()
            .any(|x| c.iter().any(|y| token_match(x, y) >= 0.85))
        {
            1.0
        } else {
            0.0
        },
    )
}

const INDUSTRY_FILLERS: &[&str] = &[
    "services", "service", "de", "des", "du", "en", "et", "and", "of", "the", "la", "le", "les",
];

/// Share of the query's significant industry words found in the candidate's.
pub fn industry_match(query: &str, candidate: Option<&str>) -> Option<f64> {
    let q: Vec<String> = tokens(query)
        .into_iter()
        .filter(|t| t.len() >= 3 && !INDUSTRY_FILLERS.contains(&t.as_str()))
        .collect();
    let c = tokens(candidate?);
    if q.is_empty() || c.is_empty() {
        return None;
    }
    let found = q
        .iter()
        .filter(|x| {
            c.iter()
                .any(|y| token_match(x, y) >= 0.85 || same_prefix(x, y, 5))
        })
        .count();
    Some(found as f64 / q.len() as f64)
}

/// Scores a company hit. `hit.website` / `industry` / `headquarters` are
/// used when the candidate was verified on its `/about/` page.
pub fn score_company(q: &CompanyQuery, hit: &CompanyHit) -> (f64, Vec<String>) {
    let mut reasons = Vec::new();
    let name = company_name_similarity(&q.name, &hit.name);
    reasons.push(format!(
        "nom {:.2} (« {} » / « {} »)",
        name, q.name, hit.name
    ));
    let mut signals = vec![(1.0, Some(name))];

    if let Some(want) = q.domain.as_deref().and_then(domain_of) {
        let v = match hit.website.as_deref().and_then(domain_of) {
            Some(got) if got == want => {
                reasons.push(format!("domaine {got} = {want}"));
                Some(1.0)
            }
            Some(got) => {
                reasons.push(format!("domaine {got} ≠ {want}"));
                Some(0.0)
            }
            None if hit.verified() => {
                reasons.push(format!(
                    "domaine: aucun site sur la page /about/ (attendu {want})"
                ));
                Some(0.25)
            }
            None => {
                reasons.push("domaine non vérifié (neutre)".into());
                None
            }
        };
        signals.push((2.0, v));
    }
    if let Some(loc) = q.location.as_deref() {
        let cand = hit.headquarters.as_deref().or(hit.location.as_deref());
        let v = location_match(loc, cand);
        reasons.push(match v {
            Some(x) if x > 0.0 => format!("lieu ✓ ({})", cand.unwrap_or("")),
            Some(_) => format!("lieu ✗ ({} ≠ {loc})", cand.unwrap_or("")),
            None => "lieu inconnu (neutre)".into(),
        });
        signals.push((0.5, v));
    }
    if let Some(ind) = q.industry.as_deref() {
        let cand = hit.industry.as_deref().or(hit.subtitle.as_deref());
        let v = industry_match(ind, cand);
        reasons.push(match v {
            Some(x) => format!("secteur {:.2} ({})", x, cand.unwrap_or("")),
            None => "secteur inconnu (neutre)".into(),
        });
        signals.push((0.4, v));
    }
    let mut score = weighted(&signals);
    if name < 0.5 && score > name {
        reasons.push(format!("plafonné par le nom ({name:.2})"));
        score = name;
    }
    (round2(score), reasons)
}

/// What is known about the current company when scoring people.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompanyEvidence {
    /// Company name, to look for in headlines.
    pub name: String,
    /// The hit comes from a `currentCompany` search whose company id is
    /// trusted (header link, or URN confirmed by comparison).
    pub filtered: bool,
}

/// `true` if the headline names the company ("… @Acme", "chez Acme Compost").
pub fn headline_mentions_company(headline: &str, company: &str) -> bool {
    let h = format!(" {} ", normalize_text(headline));
    let c = normalize_company(company);
    if c.is_empty() {
        return false;
    }
    if h.contains(&format!(" {c} ")) {
        return true;
    }
    // All significant words of the company name present.
    let words: Vec<&str> = c.split(' ').filter(|w| w.len() >= 3).collect();
    !words.is_empty() && words.iter().all(|w| h.contains(&format!(" {w} ")))
}

/// Title value: 1 if a significant word of the requested title is in the
/// headline (fuzzy, "directrice" ~ "directeur"), otherwise compares
/// seniority levels (same 0.7, one apart 0.4, else 0); `None` if the
/// headline gives no level.
pub fn title_match(title: &str, headline: &str) -> Option<f64> {
    let q: Vec<String> = tokens(title).into_iter().filter(|t| t.len() >= 3).collect();
    let h = tokens(headline);
    if q.iter().any(|x| {
        h.iter()
            .any(|y| token_match(x, y) >= 0.85 || same_prefix(x, y, 6))
    }) {
        return Some(1.0);
    }
    let (a, b) = (seniority::classify(title), seniority::classify(headline));
    if a == Seniority::Unknown || b == Seniority::Unknown {
        return None;
    }
    Some(match a.rank().abs_diff(b.rank()) {
        0 => 0.7,
        1 => 0.4,
        _ => 0.0,
    })
}

/// Scores a person hit.
pub fn score_person(
    q: &PersonQuery,
    hit: &PersonHit,
    company: Option<&CompanyEvidence>,
) -> (f64, Vec<String>) {
    let mut reasons = Vec::new();
    let name = person_name_similarity(&PersonName::parse(&q.name), &PersonName::parse(&hit.name));
    reasons.push(format!(
        "nom {:.2} (« {} » / « {} »)",
        name, q.name, hit.name
    ));
    let mut signals = vec![(3.0, Some(name))];
    let headline = hit.headline.as_deref().unwrap_or("");

    if let Some(c) = company {
        let v = if c.filtered {
            reasons.push(format!(
                "entreprise ✓ (filtre currentCompany « {} »)",
                c.name
            ));
            1.0
        } else if headline_mentions_company(headline, &c.name) {
            reasons.push(format!("entreprise ✓ (« {} » dans le titre)", c.name));
            1.0
        } else {
            reasons.push(format!(
                "entreprise non confirmée (« {} » absent du titre)",
                c.name
            ));
            0.2
        };
        signals.push((2.0, Some(v)));
    }
    if let Some(t) = q.title.as_deref() {
        let v = title_match(t, headline);
        reasons.push(match v {
            Some(1.0) => format!("poste ✓ (« {t} »)"),
            Some(x) => format!(
                "poste {x:.1} (niveau {} / {})",
                seniority::classify(t).as_str(),
                seniority::classify(headline).as_str()
            ),
            None => "poste inconnu (neutre)".into(),
        });
        signals.push((1.0, v));
    }
    if let Some(loc) = q.location.as_deref() {
        let v = location_match(loc, hit.location.as_deref());
        reasons.push(match v {
            Some(x) if x > 0.0 => format!("lieu ✓ ({})", hit.location.as_deref().unwrap_or("")),
            Some(_) => format!("lieu ✗ ({} ≠ {loc})", hit.location.as_deref().unwrap_or("")),
            None => "lieu inconnu (neutre)".into(),
        });
        signals.push((0.6, v));
    }
    let mut score = weighted(&signals);
    if name < 0.6 && score > name {
        reasons.push(format!("plafonné par le nom ({name:.2})"));
        score = name;
    }
    (round2(score), reasons)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, sub: &str, loc: &str) -> CompanyHit {
        CompanyHit {
            name: name.into(),
            url: format!(
                "https://www.linkedin.com/company/{}/",
                normalize_text(name).replace(' ', "-")
            ),
            subtitle: Some(sub.into()),
            location: Some(loc.into()),
            ..Default::default()
        }
    }

    fn person(name: &str, headline: &str, loc: &str) -> PersonHit {
        PersonHit {
            name: name.into(),
            headline: Some(headline.into()),
            location: Some(loc.into()),
            ..Default::default()
        }
    }

    #[test]
    fn company_names() {
        assert_eq!(
            company_name_similarity("Les Alchimistes SAS", "Les Alchimistes"),
            1.0
        );
        assert_eq!(company_name_similarity("ACME inc.", "Acme"), 1.0);
        let lyon = company_name_similarity("Acme Compost", "Acme Compost Lyon");
        assert!((0.6..0.75).contains(&lyon), "{lyon}");
        assert!(company_name_similarity("Acme Compost", "Acme Films") < 0.5);
        assert!(company_name_similarity("Acme Compost", "Zeta Corp") < 0.1);
        assert!(
            company_name_similarity("Wavestone", "Wavestonne") > 0.7,
            "typo"
        );
    }

    #[test]
    fn person_names() {
        let s =
            |a: &str, b: &str| person_name_similarity(&PersonName::parse(a), &PersonName::parse(b));
        assert_eq!(s("Jean Martin", "MARTIN Jean"), 1.0);
        assert_eq!(s("Cedric Davoine", "Cédric Davoine"), 1.0);
        assert_eq!(s("Dr Jean Martin", "Jean Martin, PhD"), 1.0);
        assert!((s("J. Martin", "Jean Martin") - 0.95).abs() < 1e-9);
        assert!((s("Jean Martin", "Jean-Baptiste Martin") - 2.0 / 3.0).abs() < 1e-9);
        assert!(s("Jean Martin", "Jeanne Martin") < 0.95);
        assert!(s("Jean Martin", "Paul Durand") < 0.1);
    }

    #[test]
    fn locations_and_industry() {
        assert_eq!(
            location_match("Toulon", Some("Toulon, Provence-Alpes-Côte d’Azur")),
            Some(1.0)
        );
        assert_eq!(
            location_match("Toulon", Some("Paris, Île-de-France")),
            Some(0.0)
        );
        assert_eq!(
            location_match("France", Some("Paris")),
            None,
            "generic word only"
        );
        assert_eq!(location_match("Toulon", None), None);
        assert_eq!(
            industry_match(
                "environnement",
                Some("Services de conseil en environnement")
            ),
            Some(1.0)
        );
        assert_eq!(
            industry_match("déchets", Some("Collecte des déchets")),
            Some(1.0)
        );
        assert_eq!(
            industry_match("logiciel", Some("Collecte des déchets")),
            Some(0.0)
        );
    }

    #[test]
    fn company_scores() {
        let q = CompanyQuery {
            name: "Acme Compost".into(),
            location: Some("Toulon".into()),
            ..Default::default()
        };
        let (a, ra) = score_company(
            &q,
            &hit("Acme Compost", "Collecte des déchets", "Toulon, Var"),
        );
        let (b, _) = score_company(&q, &hit("Acme Compost", "Conseil", "Paris, Île-de-France"));
        assert_eq!(a, 1.0, "{ra:?}");
        assert!((b - 0.67).abs() < 0.01, "{b}");

        // Domain: unverified is neutral, a match decides, a mismatch sinks.
        let q = CompanyQuery {
            name: "Acme Compost".into(),
            domain: Some("https://www.acme-compost.fr".into()),
            ..Default::default()
        };
        let mut h = hit("Acme Compost", "x", "y");
        let (unverified, r) = score_company(&q, &h);
        assert!((unverified - 0.67).abs() < 0.01, "{unverified} {r:?}");
        h.website = Some("http://acme-compost.fr/contact".into());
        assert_eq!(score_company(&q, &h).0, 1.0);
        h.website = Some("https://other.com".into());
        assert!((score_company(&q, &h).0 - 0.33).abs() < 0.01);

        // A different name is never rescued by other signals.
        h.name = "Zeta".into();
        h.website = Some("https://acme-compost.fr".into());
        assert!(score_company(&q, &h).0 < 0.1);
    }

    #[test]
    fn person_scores() {
        let q = PersonQuery {
            name: "Jeanne Dupont".into(),
            title: Some("directrice".into()),
            location: Some("Toulon".into()),
            ..Default::default()
        };
        let ev = CompanyEvidence {
            name: "Acme Compost".into(),
            filtered: false,
        };
        let good = person(
            "Jeanne Dupont",
            "Directrice générale chez Acme Compost",
            "Toulon, Var, France",
        );
        let homonym = person(
            "Jeanne Dupont",
            "Comptable chez Autre Société",
            "Lyon et périphérie",
        );
        let (g, rg) = score_person(&q, &good, Some(&ev));
        let (h, _) = score_person(&q, &homonym, Some(&ev));
        assert_eq!(g, 1.0, "{rg:?}");
        assert!(g - h >= 0.15, "{g} vs {h}");

        // Without any hint, two homonyms tie.
        let bare = PersonQuery::new("Jeanne Dupont");
        assert_eq!(
            score_person(&bare, &good, None).0,
            score_person(&bare, &homonym, None).0
        );

        // Trusted filter counts as company match even without mention.
        let filtered = CompanyEvidence {
            filtered: true,
            ..ev.clone()
        };
        let quiet = person("Jeanne Dupont", "Directrice générale", "Toulon");
        assert_eq!(score_person(&q, &quiet, Some(&filtered)).0, 1.0);
        // Different person capped by the name.
        assert!(
            score_person(
                &q,
                &person("Paul Durand", "Directeur chez Acme Compost", "Toulon"),
                Some(&filtered)
            )
            .0 < 0.2
        );
    }

    #[test]
    fn titles() {
        assert_eq!(
            title_match("directrice", "Directeur commercial"),
            Some(1.0),
            "same stem"
        );
        assert_eq!(
            title_match("CEO", "Fondatrice et présidente"),
            Some(0.7),
            "same level"
        );
        assert_eq!(title_match("directeur", "Head of Sales"), Some(0.4));
        assert_eq!(title_match("CEO", "Stagiaire RH"), Some(0.0));
        assert_eq!(title_match("CEO", "Passionnée de vélo"), None);
        assert!(headline_mentions_company(
            "Responsable d'exploitation @Acme Compost",
            "Acme Compost SAS"
        ));
        assert!(!headline_mentions_company(
            "Comptable chez Autre",
            "Acme Compost"
        ));
    }
}
