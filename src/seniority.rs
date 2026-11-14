//! Job-title seniority classification (EN / FR / ES / DE).
//!
//! LinkedIn headlines are free text ("CEO @Acme", "Responsable marketing",
//! "Stagiaire RH | étudiante à l'EDHEC"…). [`classify`] maps one to a
//! [`Seniority`] level by keyword matching; the highest level found wins,
//! except for past roles ("ex-CEO", "ancien directeur") and assistants
//! ("Assistante de direction", "Assistant to the CEO"), which are ignored.

use serde::{Deserialize, Serialize};

/// Seniority levels, from most to least important.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seniority {
    Unknown,
    Intern,
    Entry,
    Individual,
    Senior,
    Manager,
    Head,
    Director,
    VicePresident,
    Executive,
}

impl Seniority {
    /// 0 (unknown) … 9 (executive). Higher = more important.
    pub fn rank(self) -> u8 {
        self as u8
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Seniority::Executive => "executive",
            Seniority::VicePresident => "vice_president",
            Seniority::Director => "director",
            Seniority::Head => "head",
            Seniority::Manager => "manager",
            Seniority::Senior => "senior",
            Seniority::Individual => "individual",
            Seniority::Entry => "entry",
            Seniority::Intern => "intern",
            Seniority::Unknown => "unknown",
        }
    }
}

/// `(keyword, level)`; keywords are matched on word boundaries in the
/// lower-cased title. Longer / more specific keywords come first.
const RULES: &[(&str, Seniority)] = &[
    // Executive
    ("chief executive", Seniority::Executive),
    ("chief", Seniority::Executive),
    ("ceo", Seniority::Executive),
    ("cto", Seniority::Executive),
    ("coo", Seniority::Executive),
    ("cfo", Seniority::Executive),
    ("cmo", Seniority::Executive),
    ("cio", Seniority::Executive),
    ("cso", Seniority::Executive),
    ("cpo", Seniority::Executive),
    ("pdg", Seniority::Executive),
    ("dg", Seniority::Executive),
    ("dga", Seniority::Executive),
    ("directeur général", Seniority::Executive),
    ("directrice générale", Seniority::Executive),
    ("directeur general", Seniority::Executive),
    ("directrice generale", Seniority::Executive),
    ("director general", Seniority::Executive),
    ("directora general", Seniority::Executive),
    ("geschäftsführer", Seniority::Executive),
    ("geschäftsführerin", Seniority::Executive),
    ("vorstand", Seniority::Executive),
    ("managing director", Seniority::Executive),
    ("general manager", Seniority::Executive),
    ("founder", Seniority::Executive),
    ("co-founder", Seniority::Executive),
    ("cofounder", Seniority::Executive),
    ("fondateur", Seniority::Executive),
    ("fondatrice", Seniority::Executive),
    ("cofondateur", Seniority::Executive),
    ("co-fondateur", Seniority::Executive),
    ("cofondatrice", Seniority::Executive),
    ("co-fondatrice", Seniority::Executive),
    ("fundador", Seniority::Executive),
    ("fundadora", Seniority::Executive),
    ("gründer", Seniority::Executive),
    ("president", Seniority::Executive),
    ("président", Seniority::Executive),
    ("présidente", Seniority::Executive),
    ("presidente", Seniority::Executive),
    ("owner", Seniority::Executive),
    ("gérant", Seniority::Executive),
    ("gérante", Seniority::Executive),
    ("dirigeant", Seniority::Executive),
    ("dirigeante", Seniority::Executive),
    ("managing partner", Seniority::Executive),
    ("associé", Seniority::Executive),
    ("associée", Seniority::Executive),
    ("partner", Seniority::Executive),
    // Vice president
    ("vice president", Seniority::VicePresident),
    ("vice-president", Seniority::VicePresident),
    ("vice-président", Seniority::VicePresident),
    ("vice-présidente", Seniority::VicePresident),
    ("vicepresidente", Seniority::VicePresident),
    ("evp", Seniority::VicePresident),
    ("svp", Seniority::VicePresident),
    ("vp", Seniority::VicePresident),
    // Director
    ("director", Seniority::Director),
    ("directeur", Seniority::Director),
    ("directrice", Seniority::Director),
    ("directora", Seniority::Director),
    ("direktor", Seniority::Director),
    ("leiter", Seniority::Director),
    ("leiterin", Seniority::Director),
    // Head
    ("head of", Seniority::Head),
    ("head", Seniority::Head),
    ("jefe", Seniority::Head),
    ("jefa", Seniority::Head),
    // Manager
    ("manager", Seniority::Manager),
    ("manageuse", Seniority::Manager),
    ("responsable", Seniority::Manager),
    ("chef de projet", Seniority::Manager),
    ("cheffe de projet", Seniority::Manager),
    ("chef d'équipe", Seniority::Manager),
    ("chef de service", Seniority::Manager),
    ("chef", Seniority::Manager),
    ("cheffe", Seniority::Manager),
    ("gerente", Seniority::Manager),
    ("supervisor", Seniority::Manager),
    ("superviseur", Seniority::Manager),
    ("coordinator", Seniority::Manager),
    ("coordinateur", Seniority::Manager),
    ("coordinatrice", Seniority::Manager),
    // Senior / lead
    ("senior", Seniority::Senior),
    ("sr", Seniority::Senior),
    ("principal", Seniority::Senior),
    ("staff", Seniority::Senior),
    ("lead", Seniority::Senior),
    ("expert", Seniority::Senior),
    ("experte", Seniority::Senior),
    ("confirmé", Seniority::Senior),
    ("confirmée", Seniority::Senior),
    // Entry
    ("junior", Seniority::Entry),
    ("jr", Seniority::Entry),
    ("graduate", Seniority::Entry),
    ("débutant", Seniority::Entry),
    ("débutante", Seniority::Entry),
    ("phd candidate", Seniority::Entry),
    ("phd student", Seniority::Entry),
    ("doctorant", Seniority::Entry),
    ("doctorante", Seniority::Entry),
    ("doktorand", Seniority::Entry),
    ("doktorandin", Seniority::Entry),
    ("doctorando", Seniority::Entry),
    // Intern / apprentice / student
    ("intern", Seniority::Intern),
    ("internship", Seniority::Intern),
    ("trainee", Seniority::Intern),
    ("apprentice", Seniority::Intern),
    ("stagiaire", Seniority::Intern),
    ("stage", Seniority::Intern),
    ("alternant", Seniority::Intern),
    ("alternante", Seniority::Intern),
    ("alternance", Seniority::Intern),
    ("apprenti", Seniority::Intern),
    ("apprentie", Seniority::Intern),
    ("becario", Seniority::Intern),
    ("becaria", Seniority::Intern),
    ("praktikant", Seniority::Intern),
    ("praktikantin", Seniority::Intern),
    ("werkstudent", Seniority::Intern),
    ("student", Seniority::Intern),
    ("étudiant", Seniority::Intern),
    ("étudiante", Seniority::Intern),
];

/// Generic job words: a title made only of these is an individual contributor.
const INDIVIDUAL: &[&str] = &[
    "engineer",
    "ingénieur",
    "ingénieure",
    "developer",
    "développeur",
    "développeuse",
    "consultant",
    "consultante",
    "analyst",
    "analyste",
    "designer",
    "chargé",
    "chargée",
    "specialist",
    "spécialiste",
    "technicien",
    "technicienne",
    "commercial",
    "commerciale",
    "sales",
    "assistant",
    "assistante",
    "accountant",
    "comptable",
    "officer",
    "agent",
    "operator",
    "opérateur",
    "opératrice",
    "associate",
    "advisor",
    "conseiller",
    "conseillère",
    "architect",
    "architecte",
    "researcher",
    "chercheur",
    "chercheuse",
    "scientist",
    "attaché",
    "attachée",
    "gestionnaire",
    "employé",
    "employée",
    "ouvrier",
    "ouvrière",
    "vendeur",
    "vendeuse",
    "business developer",
    "account executive",
    "chauffeur",
    "driver",
    "collecteur",
    "collectrice",
    "livreur",
    "livreuse",
    "préparateur",
    "préparatrice",
    "magasinier",
    "magasinière",
    "customer success",
    "support",
    "infirmier",
    "infirmière",
    "nurse",
    "teacher",
    "enseignant",
    "enseignante",
    "formateur",
    "formatrice",
    "juriste",
    "lawyer",
    "avocat",
    "avocate",
    "rédacteur",
    "rédactrice",
    "writer",
    "journaliste",
    "journalist",
    "recruteur",
    "recruteuse",
    "recruiter",
    "postdoc",
];

/// Misleading phrases rewritten before matching ("HR Business Partner" is not
/// a partner of the firm, a "Product Owner" does not own the company…).
const REWRITES: &[(&str, &str)] = &[
    ("business partner", "manager"),
    ("hr partner", "manager"),
    ("chief of staff", "director"),
    ("product owner", "manager"),
    ("process owner", "manager"),
    ("account owner", "manager"),
];

/// Words that turn what follows into a past role.
const PAST_MARKERS: &[&str] = &[
    "ex",
    "former",
    "ancien",
    "ancienne",
    "previously",
    "anciennement",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Positions (byte offsets) where `kw` occurs as whole words in `text`.
fn find_word(text: &str, kw: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find(kw) {
        let i = start + pos;
        let before_ok = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[i + kw.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            out.push(i);
        }
        start = i + kw.len().max(1);
    }
    out
}

/// `true` if the word just before `pos` is a past marker ("ex-CEO", "former VP").
fn is_past(text: &str, pos: usize) -> bool {
    let before = text[..pos].trim_end_matches(|c: char| c == '-' || c.is_whitespace());
    let last_word: String = before
        .chars()
        .rev()
        .take_while(|c| is_word_char(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    PAST_MARKERS.contains(&last_word.as_str())
}

/// Classifies a job title / LinkedIn headline.
pub fn classify(title: &str) -> Seniority {
    let mut t = title.to_lowercase().replace('’', "'");
    for (from, to) in REWRITES {
        t = t.replace(from, to);
    }
    if t.trim().is_empty() {
        return Seniority::Unknown;
    }
    // Only the current part of "Role @Company | other stuff" matters most,
    // but keep the whole text: many headlines are "Role1 & Role2".
    let assistant = ["assistant", "assistante", "asistente", "assistenz"]
        .iter()
        .any(|w| !find_word(&t, w).is_empty());

    let mut best = Seniority::Unknown;
    for (kw, level) in RULES {
        let hits = find_word(&t, kw);
        if hits.iter().any(|&p| !is_past(&t, p)) {
            let level = if assistant && *level >= Seniority::Manager {
                // "Assistante de direction", "Assistant to the CEO"
                Seniority::Individual
            } else {
                *level
            };
            if level > best {
                best = level;
            }
        }
    }
    if best == Seniority::Unknown && INDIVIDUAL.iter().any(|w| !find_word(&t, w).is_empty()) {
        best = Seniority::Individual;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use Seniority::*;

    #[test]
    fn levels() {
        let cases = [
            ("CEO @Acme", Executive),
            ("Co-fondatrice et Présidente", Executive),
            ("Directeur Général Adjoint", Executive),
            ("Fondateur | Les Alchimistes", Executive),
            ("VP Sales EMEA", VicePresident),
            ("Directrice commerciale", Director),
            ("Head of Data", Head),
            ("Responsable d'exploitation", Manager),
            ("Cheffe de projet compostage", Manager),
            ("Senior Software Engineer", Senior),
            ("Tech Lead Rust", Senior),
            (
                "Chargé de développement commercial @Les Alchimistes Côte d’Azur",
                Individual,
            ),
            ("Consultante", Individual),
            ("Junior Data Analyst", Entry),
            ("Stagiaire RH", Intern),
            // Generic job words only count when no level keyword is present.
            ("Alternante chargée de communication", Intern),
            ("", Unknown),
            ("Passionné de vélo", Unknown),
        ];
        for (title, expected) in cases {
            assert_eq!(classify(title), expected, "{title}");
        }
    }

    #[test]
    fn past_roles_and_assistants() {
        assert_eq!(classify("Consultant, ex-CEO de Foo"), Individual);
        assert_eq!(classify("Former VP Marketing, now advisor"), Individual);
        assert_eq!(
            classify("Ancien directeur, consultant indépendant"),
            Individual
        );
        assert_eq!(classify("Assistante de direction"), Individual);
        assert_eq!(classify("Executive Assistant to the CEO"), Individual);
        assert_eq!(classify("HR Business Partner"), Manager);
        assert_eq!(classify("Chief of Staff"), Director);
        assert_eq!(classify("Product Owner"), Manager);
        assert_eq!(classify("Animatrice socio-culturelle"), Unknown);
        assert_eq!(classify("chauffeur collecteur bio"), Individual);
        assert_eq!(classify("PhD candidate Soil biology"), Entry);
        assert_eq!(classify("Costumer Success chez Acme"), Unknown);
        assert_eq!(classify("Customer Success chez Acme"), Individual);
    }

    #[test]
    fn word_boundaries() {
        // "dg" must not match inside "edge", "cto" inside "director", "vp" inside "mvp".
        assert_eq!(classify("Edge computing engineer"), Individual);
        assert_eq!(classify("MVP builder"), Unknown);
        assert_eq!(classify("Stage manager au théâtre"), Manager);
    }

    #[test]
    fn ranks_are_ordered() {
        assert!(Executive.rank() > Director.rank());
        assert!(Director.rank() > Manager.rank());
        assert!(Manager.rank() > Individual.rank());
        assert!(Individual.rank() > Intern.rank());
        assert_eq!(Unknown.rank(), 0);
    }
}
