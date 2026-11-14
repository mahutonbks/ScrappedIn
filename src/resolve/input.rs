//! Parsing of name-based input lines (batch files).
//!
//! ```text
//! company: Les Alchimistes | domain=alchimistes.fr | location=Toulon | industry=déchets
//! person: Caroline Witwer | company=Les Alchimistes | title=directrice | location=Var
//! person: Jean Martin | company=https://www.linkedin.com/company/acme/
//! ```
//!
//! Prefixes: `company:` / `entreprise:` and `person:` / `personne:`. Keys
//! accept English and French names (`domain`/`domaine`/`site`,
//! `location`/`lieu`/`ville`, `industry`/`secteur`, `company`/`entreprise`,
//! `title`/`poste`/`titre`). For people, `company_domain`,
//! `company_location` and `company_industry` refine a company given by name.

use serde::{Deserialize, Serialize};

use super::{CompanyQuery, CompanyRef, PersonQuery};

/// A parsed input line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    Company(CompanyQuery),
    Person(PersonQuery),
}

impl Query {
    pub fn name(&self) -> &str {
        match self {
            Query::Company(q) => &q.name,
            Query::Person(q) => &q.name,
        }
    }

    /// `"company"` or `"person"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Query::Company(_) => "company",
            Query::Person(_) => "person",
        }
    }
}

fn key_of(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .replace(['é', 'è'], "e")
        .replace(['-', ' '], "_")
}

/// `None` if the line is not a name query (e.g. a URL, a comment);
/// `Some(Err)` if it is one but malformed.
pub fn parse_query_line(line: &str) -> Option<Result<Query, String>> {
    let line = line.trim().trim_start_matches('\u{feff}');
    let (head, rest) = line.split_once(':')?;
    let kind = match key_of(head).as_str() {
        "company" | "entreprise" | "societe" | "organisation" | "organization" => "company",
        "person" | "personne" | "people" => "person",
        _ => return None,
    };
    Some(parse_fields(kind, rest))
}

fn parse_fields(kind: &str, rest: &str) -> Result<Query, String> {
    let mut parts = rest.split('|').map(str::trim);
    let name = parts.next().unwrap_or("").to_string();
    if name.is_empty() || name.contains('=') {
        return Err(format!("missing {kind} name before the first '|'"));
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for p in parts.filter(|p| !p.is_empty()) {
        let (k, v) = p
            .split_once('=')
            .ok_or_else(|| format!("expected key=value, got \"{p}\""))?;
        let v = v.trim();
        if !v.is_empty() {
            fields.push((key_of(k), v.to_string()));
        }
    }

    if kind == "company" {
        let mut q = CompanyQuery::new(name);
        for (k, v) in fields {
            match k.as_str() {
                "domain" | "domaine" | "site" | "website" | "web" => q.domain = Some(v),
                "location" | "lieu" | "loc" | "ville" | "city" => q.location = Some(v),
                "industry" | "secteur" | "sector" => q.industry = Some(v),
                other => return Err(format!("unknown company field \"{other}\"")),
            }
        }
        return Ok(Query::Company(q));
    }

    let mut q = PersonQuery::new(name);
    let mut company: Option<String> = None;
    let mut sub = CompanyQuery::default();
    for (k, v) in fields {
        match k.as_str() {
            "company" | "entreprise" | "societe" | "employer" | "employeur" => company = Some(v),
            "title" | "poste" | "titre" | "role" | "fonction" | "job" => q.title = Some(v),
            "location" | "lieu" | "loc" | "ville" | "city" => q.location = Some(v),
            "company_domain" | "domaine_entreprise" | "domain" | "domaine" => sub.domain = Some(v),
            "company_location" | "lieu_entreprise" => sub.location = Some(v),
            "company_industry" | "secteur_entreprise" | "secteur" | "industry" => {
                sub.industry = Some(v);
            }
            other => return Err(format!("unknown person field \"{other}\"")),
        }
    }
    q.company = match company {
        Some(c) if c.contains("linkedin.com/company/") => Some(CompanyRef::Url(c)),
        Some(c) => {
            sub.name = c;
            Some(CompanyRef::Name(sub))
        }
        None if sub != CompanyQuery::default() => {
            return Err("company_* fields need company=<name>".into());
        }
        None => None,
    };
    Ok(Query::Person(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_queries() {
        assert!(parse_query_line("https://www.linkedin.com/in/x/").is_none());
        assert!(parse_query_line("www.linkedin.com/company/x").is_none());
        assert!(parse_query_line("# comment: x").is_none());
        assert!(parse_query_line("").is_none());
    }

    #[test]
    fn companies() {
        let q =
            parse_query_line("company: Les Alchimistes | domain=alchimistes.fr | location=Toulon")
                .unwrap()
                .unwrap();
        assert_eq!(
            q,
            Query::Company(CompanyQuery {
                name: "Les Alchimistes".into(),
                domain: Some("alchimistes.fr".into()),
                location: Some("Toulon".into()),
                industry: None,
            })
        );
        let q = parse_query_line("Entreprise : Acme | Secteur = déchets | lieu=")
            .unwrap()
            .unwrap();
        let Query::Company(q) = q else { panic!() };
        assert_eq!(q.industry.as_deref(), Some("déchets"));
        assert_eq!(q.location, None, "empty value ignored");
    }

    #[test]
    fn people() {
        let q = parse_query_line(
            "person: Caroline Witwer | company=Les Alchimistes | title=directrice | location=Var",
        )
        .unwrap()
        .unwrap();
        let Query::Person(p) = q else { panic!() };
        assert_eq!(p.title.as_deref(), Some("directrice"));
        assert_eq!(p.location.as_deref(), Some("Var"));
        assert_eq!(
            p.company,
            Some(CompanyRef::Name(CompanyQuery::new("Les Alchimistes")))
        );

        let Query::Person(p) = parse_query_line(
            "personne: Jean Martin | company=https://www.linkedin.com/company/acme/",
        )
        .unwrap()
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            p.company,
            Some(CompanyRef::Url(
                "https://www.linkedin.com/company/acme/".into()
            ))
        );

        let Query::Person(p) =
            parse_query_line("person: A B | company=Acme | company_domain=acme.fr")
                .unwrap()
                .unwrap()
        else {
            panic!()
        };
        let Some(CompanyRef::Name(c)) = p.company else {
            panic!()
        };
        assert_eq!(c.domain.as_deref(), Some("acme.fr"));
    }

    #[test]
    fn errors() {
        assert!(parse_query_line("company: | domain=x").unwrap().is_err());
        assert!(parse_query_line("company: Acme | domain").unwrap().is_err());
        assert!(
            parse_query_line("company: Acme | colour=red")
                .unwrap()
                .is_err()
        );
        assert!(
            parse_query_line("person: A B | company_domain=acme.fr")
                .unwrap()
                .is_err()
        );
    }
}
