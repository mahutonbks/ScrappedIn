//! Text normalization for name matching (pure functions).
//!
//! * [`normalize_text`]: lower case, accents removed, punctuation and emojis
//!   turned into spaces, runs of spaces collapsed.
//! * [`normalize_company`]: same, plus dotted acronyms joined ("S.p.A." →
//!   "spa"), legal forms ("SAS", "GmbH", "Inc"…) and a trailing "Group" /
//!   "Groupe" removed, and filler words ("les", "the", "&"…) dropped.
//! * [`PersonName::parse`]: tokens of a person name, order-insensitive, with
//!   civil titles, degree suffixes ("PhD"), particles ("de", "van") and
//!   anything after a separator (", PhD", " | CEO", emojis) removed; single
//!   letters are kept apart as initials ("J. Martin").
//! * [`domain_of`]: registrable domain of a URL or bare host
//!   ("https://www.shop.acme.co.uk/x" → "acme.co.uk").

/// ASCII fold of common Latin letters with diacritics.
fn fold_char(c: char, out: &mut String) {
    let s = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' | 'ĝ' | 'ġ' | 'ģ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' | 'ĺ' | 'ļ' => "l",
        'ñ' | 'ń' | 'ň' | 'ņ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ŕ' | 'ř' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ť' | 'ţ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ž' | 'ż' => "z",
        'œ' => "oe",
        'æ' => "ae",
        'ß' => "ss",
        'þ' => "th",
        'ð' => "d",
        _ => {
            out.push(c);
            return;
        }
    };
    out.push_str(s);
}

/// Lower case, no accents, alphanumeric words separated by single spaces.
pub fn normalize_text(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        fold_char(c, &mut folded);
    }
    let mut out = String::with_capacity(folded.len());
    for c in folded.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// Normalized tokens.
pub fn tokens(s: &str) -> Vec<String> {
    normalize_text(s)
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Legal forms removed at the end of a company name (after dotted acronyms
/// were joined: "S.A.S." → "sas", "S.p.A." → "spa").
const LEGAL_FORMS: &[&str] = &[
    "sas",
    "sasu",
    "sarl",
    "sa",
    "eurl",
    "sci",
    "snc",
    "scop",
    "scic",
    "selarl",
    "selas",
    "gie",
    "inc",
    "incorporated",
    "corp",
    "corporation",
    "co",
    "company",
    "ltd",
    "limited",
    "llc",
    "llp",
    "lp",
    "plc",
    "pllc",
    "gmbh",
    "ag",
    "kg",
    "ug",
    "ohg",
    "ev",
    "sl",
    "slu",
    "sau",
    "spa",
    "srl",
    "srls",
    "bv",
    "nv",
    "oy",
    "ab",
    "as",
    "aps",
    "pty",
    "kk",
    "group",
    "groupe",
    "gruppe",
    "grupo",
    "gruppo",
    "holding",
];

/// Words that carry no identity in a company name.
const COMPANY_FILLERS: &[&str] = &[
    "les", "le", "la", "l", "the", "and", "et", "und", "y", "of", "de", "du", "des", "d",
];

/// Normalized company name without legal form and filler words.
/// Never returns an empty string for a non-empty input: if everything would
/// be removed ("Groupe SA"), the plain normalized text is kept.
pub fn normalize_company(name: &str) -> String {
    // Join dotted acronyms before punctuation becomes spaces.
    let mut joined = String::with_capacity(name.len());
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let dot_between_letters = c == '.'
            && i > 0
            && chars[i - 1].is_alphabetic()
            && chars
                .get(i + 1)
                .is_none_or(|n| n.is_alphabetic() || n.is_whitespace() || *n == ',' || *n == ')');
        if dot_between_letters && (i < 2 || !chars[i - 2].is_alphabetic()) {
            continue; // "S.p.A." / "S.A.S" style: drop the dot
        }
        joined.push(if c == '&' { ' ' } else { c });
    }
    let mut toks = tokens(&joined);
    let plain = toks.join(" ");
    while toks.len() > 1
        && toks
            .last()
            .is_some_and(|t| LEGAL_FORMS.contains(&t.as_str()))
    {
        toks.pop();
    }
    let kept: Vec<String> = toks
        .into_iter()
        .filter(|t| !COMPANY_FILLERS.contains(&t.as_str()))
        .collect();
    if kept.is_empty() {
        plain
    } else {
        kept.join(" ")
    }
}

const TITLES: &[&str] = &[
    "dr",
    "docteur",
    "doctor",
    "pr",
    "prof",
    "professeur",
    "professor",
    "m",
    "mr",
    "mrs",
    "ms",
    "mme",
    "mlle",
    "miss",
    "me",
    "maitre",
    "sir",
    "dame",
    "herr",
    "frau",
    "sr",
    "sra",
    "srta",
];
const SUFFIXES: &[&str] = &[
    "phd", "md", "mba", "msc", "bsc", "ing", "eng", "cpa", "cfa", "pmp", "jr", "junior", "ii",
    "iii", "iv", "esq", "dipl", "hdr",
];
const PARTICLES: &[&str] = &[
    "de", "du", "des", "d", "la", "le", "van", "von", "der", "den", "ter", "di", "da", "del",
    "della", "dos", "das", "do", "el", "al", "bin", "ben", "mac", "st",
];

/// A person name reduced to comparable parts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PersonName {
    /// Full tokens (≥ 2 letters), particles removed, sorted.
    pub tokens: Vec<String>,
    /// Single-letter tokens ("J. Martin" → `['j']`).
    pub initials: Vec<char>,
}

impl PersonName {
    pub fn parse(raw: &str) -> Self {
        // Everything after a separator is a title/credential, not the name:
        // "Jean Martin, PhD", "Jean Martin | CEO", "Jean Martin (il/lui)".
        let cut = raw
            .find([',', '|', '(', '•', '·', '/', '–', '—'])
            .map_or(raw, |i| &raw[..i]);
        let mut toks = tokens(cut);
        while toks.first().is_some_and(|t| TITLES.contains(&t.as_str())) && toks.len() > 1 {
            toks.remove(0);
        }
        while toks.last().is_some_and(|t| SUFFIXES.contains(&t.as_str())) && toks.len() > 1 {
            toks.pop();
        }
        let mut name = PersonName::default();
        let only_particles = toks
            .iter()
            .all(|t| PARTICLES.contains(&t.as_str()) || t.len() == 1);
        for t in toks {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    name.initials.push(c);
                }
            } else if only_particles || !PARTICLES.contains(&t.as_str()) {
                name.tokens.push(t);
            }
        }
        name.tokens.sort();
        name.tokens.dedup();
        name
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.initials.is_empty()
    }

    /// Canonical key (order-insensitive): `"dupont jeanne"`.
    pub fn key(&self) -> String {
        let mut parts = self.tokens.clone();
        parts.extend(self.initials.iter().map(std::string::ToString::to_string));
        parts.join(" ")
    }
}

/// Second-level labels that make a 3-label registrable domain ("co.uk").
const SECOND_LEVEL: &[&str] = &[
    "co", "com", "org", "net", "gov", "gouv", "ac", "edu", "asso", "nom",
];

/// Registrable domain of a URL or host: `"https://www.Acme.fr/contact"` →
/// `"acme.fr"`, `"shop.acme.co.uk"` → `"acme.co.uk"`. `None` if no host.
pub fn domain_of(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let with_scheme = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let host = url::Url::parse(&with_scheme)
        .ok()?
        .host_str()?
        .to_lowercase();
    let host = host.trim_end_matches('.');
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let n = labels.len();
    let take = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVEL.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    Some(labels[n.saturating_sub(take)..].join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text() {
        assert_eq!(
            normalize_text("  Côte d’Azur  —  ÉCOLE!! "),
            "cote d azur ecole"
        );
        assert_eq!(normalize_text("Œuvre & Straße"), "oeuvre strasse");
        assert_eq!(normalize_text("Jean 🚀 Martin"), "jean martin");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn companies() {
        assert_eq!(normalize_company("Les Alchimistes SAS"), "alchimistes");
        assert_eq!(normalize_company("ACME Inc."), "acme");
        assert_eq!(normalize_company("Acme, Ltd"), "acme");
        assert_eq!(normalize_company("Foo GmbH & Co. KG"), "foo");
        assert_eq!(normalize_company("Barilla S.p.A."), "barilla");
        assert_eq!(normalize_company("Telefónica S.L."), "telefonica");
        assert_eq!(normalize_company("Renault Group"), "renault");
        assert_eq!(normalize_company("Groupe SEB"), "groupe seb");
        assert_eq!(normalize_company("Wavestone SA"), "wavestone");
        assert_eq!(normalize_company("SARL"), "sarl", "never empty");
        assert_eq!(
            normalize_company("Les Alchimistes Côte d'Azur"),
            "alchimistes cote azur"
        );
    }

    #[test]
    fn people() {
        let p = PersonName::parse;
        assert_eq!(p("Jean Martin").key(), p("MARTIN Jean").key());
        assert_eq!(
            p("Jean-Pierre Martin").tokens,
            vec!["jean", "martin", "pierre"]
        );
        assert_eq!(p("J. Martin").initials, vec!['j']);
        assert_eq!(p("J. Martin").tokens, vec!["martin"]);
        assert_eq!(p("Dr Jean Martin").key(), "jean martin");
        assert_eq!(p("Mme Jeanne Dupont").key(), "dupont jeanne");
        assert_eq!(p("Jean Martin, PhD").key(), "jean martin");
        assert_eq!(p("Jean Martin MBA").key(), "jean martin");
        assert_eq!(p("Jean Martin 🚀 | CEO @Acme").key(), "jean martin");
        assert_eq!(p("Charles de Gaulle").key(), "charles gaulle");
        assert_eq!(p("Ludwig van Beethoven").key(), p("Beethoven Ludwig").key());
        assert_eq!(p("Cédric Davoine").key(), p("cedric davoine").key());
        assert_eq!(
            p("De La").key(),
            "de la",
            "particles kept when nothing else"
        );
    }

    #[test]
    fn domains() {
        assert_eq!(
            domain_of("https://www.Acme.fr/contact?x=1").as_deref(),
            Some("acme.fr")
        );
        assert_eq!(domain_of("acme.fr").as_deref(), Some("acme.fr"));
        assert_eq!(
            domain_of("http://shop.acme.co.uk").as_deref(),
            Some("acme.co.uk")
        );
        assert_eq!(
            domain_of("https://alchimistes.co/collecte").as_deref(),
            Some("alchimistes.co")
        );
        assert_eq!(domain_of("localhost"), None);
        assert_eq!(domain_of(""), None);
    }
}
