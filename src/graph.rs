//! Knowledge-graph export of scraped companies and profiles.
//!
//! [`KnowledgeGraph`] merges any number of [`Company`] and [`Person`] results
//! into typed nodes and relations, then exports them as:
//!
//! * [`KnowledgeGraph::to_json`] — generic `{nodes, edges}` (NetworkX,
//!   Gephi, Cytoscape, D3…);
//! * [`KnowledgeGraph::to_json_ld`] — schema.org JSON-LD (`@graph`);
//! * [`KnowledgeGraph::to_cypher`] — idempotent `MERGE` statements for Neo4j.
//!
//! Node ids are stable: LinkedIn slugs when a URL is known
//! (`person:jane-doe`, `company:acme`, `school:hec-paris`), otherwise a
//! normalized name (`company~acme-corp`, `place:paris-france`). Organizations
//! seen only by name are merged with an existing one of the same name.
//!
//! ```no_run
//! # fn demo(company: linkedin_scraper::Company, person: linkedin_scraper::Person) {
//! use linkedin_scraper::KnowledgeGraph;
//! let mut g = KnowledgeGraph::new();
//! g.add_company(&company);
//! g.add_person(&person);
//! std::fs::write("graph.cypher", g.to_cypher()).unwrap();
//! # }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::models::{Company, Person};
use crate::seniority::{self, Seniority};

/// Node types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NodeKind {
    Person,
    Organization,
    School,
    Industry,
    Place,
    Specialty,
    Language,
    Accomplishment,
    Group,
    Newsletter,
}

impl NodeKind {
    /// Neo4j label / JSON `kind`.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Person => "Person",
            NodeKind::Organization => "Organization",
            NodeKind::School => "School",
            NodeKind::Industry => "Industry",
            NodeKind::Place => "Place",
            NodeKind::Specialty => "Specialty",
            NodeKind::Language => "Language",
            NodeKind::Accomplishment => "Accomplishment",
            NodeKind::Group => "Group",
            NodeKind::Newsletter => "Newsletter",
        }
    }

    fn schema_type(self) -> &'static str {
        match self {
            NodeKind::Person => "Person",
            NodeKind::Organization | NodeKind::Group => "Organization",
            NodeKind::School => "EducationalOrganization",
            NodeKind::Place => "Place",
            NodeKind::Language => "Language",
            NodeKind::Newsletter => "Periodical",
            NodeKind::Accomplishment => "CreativeWork",
            NodeKind::Industry | NodeKind::Specialty => "DefinedTerm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Value>,
}

/// Relation types (Neo4j relationship types).
pub mod rel {
    pub const WORKS_AT: &str = "WORKS_AT";
    pub const WORKED_AT: &str = "WORKED_AT";
    pub const STUDIED_AT: &str = "STUDIED_AT";
    pub const IN_INDUSTRY: &str = "IN_INDUSTRY";
    pub const HEADQUARTERED_IN: &str = "HEADQUARTERED_IN";
    pub const LOCATED_IN: &str = "LOCATED_IN";
    pub const HAS_SPECIALTY: &str = "HAS_SPECIALTY";
    pub const SPEAKS: &str = "SPEAKS";
    pub const HAS_ACCOMPLISHMENT: &str = "HAS_ACCOMPLISHMENT";
    pub const FOLLOWS: &str = "FOLLOWS";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Value>,
}

/// Mergeable graph of companies, people and what links them.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeGraph {
    nodes: BTreeMap<String, Node>,
    edges: Vec<Edge>,
    edge_index: HashMap<(String, String, String), Vec<usize>>,
    /// normalized name → id, for organizations / schools known by name only.
    org_names: HashMap<(NodeKind, String), String>,
}

/// `"Côte d’Azur, France"` → `"côte-d-azur-france"`.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `(kind, slug)` from a LinkedIn URL: `/in/x` → person, `/company/x`,
/// `/school/x`, `/groups/x`, `/newsletters/x`.
fn linkedin_id(url: &str) -> Option<(NodeKind, String)> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?;
    if !host.ends_with("linkedin.com") {
        return None;
    }
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let kind = match *segs.first()? {
        "in" => NodeKind::Person,
        "company" => NodeKind::Organization,
        "school" => NodeKind::School,
        "groups" => NodeKind::Group,
        "newsletters" => NodeKind::Newsletter,
        _ => return None,
    };
    let slug = segs.get(1)?;
    let slug = crate::scrapers::percent_decode(slug);
    Some((kind, slug.to_lowercase()))
}

fn prefix(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Person => "person",
        NodeKind::Organization => "company",
        NodeKind::School => "school",
        NodeKind::Industry => "industry",
        NodeKind::Place => "place",
        NodeKind::Specialty => "specialty",
        NodeKind::Language => "language",
        NodeKind::Accomplishment => "accomplishment",
        NodeKind::Group => "group",
        NodeKind::Newsletter => "newsletter",
    }
}

fn props(pairs: &[(&str, Option<Value>)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .filter_map(|(k, v)| match v {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(v) => Some((k.to_string(), v.clone())),
        })
        .collect()
}

fn s(v: &Option<String>) -> Option<Value> {
    v.as_ref().map(|x| Value::String(x.trim().to_string()))
}

/// "aujourd’hui", "Present", "heute"… LinkedIn always shows one for current
/// roles; no end date means a single-month role ("août 2022 · 1 mois").
fn is_current(to_date: Option<&str>) -> bool {
    match to_date.map(|t| t.trim().to_lowercase().replace('’', "'")) {
        None => false,
        Some(t) if t.is_empty() => false,
        Some(t) => [
            "present",
            "aujourd'hui",
            "today",
            "actuel",
            "heute",
            "hoy",
            "presente",
            "oggi",
        ]
        .iter()
        .any(|w| t.contains(w)),
    }
}

impl KnowledgeGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Adds (or merges into) a node and returns its id. Known properties are
    /// kept; missing ones are filled in.
    fn upsert(
        &mut self,
        id: String,
        kind: NodeKind,
        name: &str,
        properties: BTreeMap<String, Value>,
    ) -> String {
        let name = name.trim();
        match self.nodes.get_mut(&id) {
            Some(n) => {
                if n.name.is_empty() {
                    n.name = name.to_string();
                }
                for (k, v) in properties {
                    n.properties.entry(k).or_insert(v);
                }
            }
            None => {
                self.nodes.insert(
                    id.clone(),
                    Node {
                        id: id.clone(),
                        kind,
                        name: name.to_string(),
                        properties,
                    },
                );
            }
        }
        if matches!(kind, NodeKind::Organization | NodeKind::School) && !name.is_empty() {
            self.org_names
                .entry((kind, slugify(name)))
                .or_insert_with(|| id.clone());
        }
        id
    }

    /// Node for an entity known by URL and/or name.
    fn entity(
        &mut self,
        kind: NodeKind,
        url: Option<&str>,
        name: &str,
        properties: BTreeMap<String, Value>,
    ) -> Option<String> {
        let by_url = url.and_then(linkedin_id);
        let (kind, id) = match by_url {
            // Trust the URL type (an interest under "Companies" may be a /school/).
            Some((k, slug)) => (k, format!("{}:{slug}", prefix(k))),
            None => {
                let slug = slugify(name);
                if slug.is_empty() {
                    return None;
                }
                let id = self
                    .org_names
                    .get(&(kind, slug.clone()))
                    .cloned()
                    .unwrap_or_else(|| format!("{}~{slug}", prefix(kind)));
                (kind, id)
            }
        };
        let mut properties = properties;
        if let Some(u) = url.filter(|u| u.starts_with("http")) {
            properties
                .entry("url".into())
                .or_insert_with(|| Value::String(u.to_string()));
        }
        Some(self.upsert(id, kind, name, properties))
    }

    /// Simple named node (industry, place, specialty, language).
    fn term(&mut self, kind: NodeKind, name: &str) -> Option<String> {
        let slug = slugify(name);
        (!slug.is_empty()).then(|| {
            self.upsert(
                format!("{}:{slug}", prefix(kind)),
                kind,
                name,
                BTreeMap::new(),
            )
        })
    }

    /// Adds an edge, or merges it into an existing one with the same ends,
    /// relation and title whose start date matches (or is unknown on one side):
    /// a role seen on the "People" tab and on the profile is one edge.
    fn link(
        &mut self,
        source: &str,
        target: &str,
        relation: &str,
        properties: BTreeMap<String, Value>,
    ) {
        let key = (source.to_string(), target.to_string(), relation.to_string());
        let same = |a: Option<&Value>, b: Option<&Value>| a.is_none() || b.is_none() || a == b;
        let existing = self.edge_index.get(&key).and_then(|idxs| {
            idxs.iter().copied().find(|&i| {
                let e = &self.edges[i].properties;
                same(e.get("title"), properties.get("title"))
                    && same(e.get("from"), properties.get("from"))
            })
        });
        match existing {
            Some(i) => {
                let e = &mut self.edges[i].properties;
                for (k, v) in properties {
                    // Profile data (dates, exact title) beats the people-page headline.
                    if k == "source" {
                        if e.get("source") != Some(&v) {
                            e.insert("source".into(), Value::from("profile+company_people_page"));
                        }
                    } else {
                        e.entry(k).or_insert(v);
                    }
                }
            }
            None => {
                self.edge_index
                    .entry(key)
                    .or_default()
                    .push(self.edges.len());
                self.edges.push(Edge {
                    source: source.to_string(),
                    target: target.to_string(),
                    relation: relation.to_string(),
                    properties,
                });
            }
        }
    }

    /// Adds a company, its industry, headquarters, specialties and employees.
    pub fn add_company(&mut self, c: &Company) {
        let name = c.name.clone().unwrap_or_default();
        let Some(org) = self.entity(
            NodeKind::Organization,
            Some(&c.linkedin_url),
            &name,
            props(&[
                ("website", s(&c.website)),
                ("phone", s(&c.phone)),
                ("founded", s(&c.founded)),
                ("company_type", s(&c.company_type)),
                ("company_size", s(&c.company_size)),
                ("headcount", c.headcount.map(Value::from)),
                ("about", s(&c.about_us)),
            ]),
        ) else {
            return;
        };
        if let Some(i) = c
            .industry
            .as_deref()
            .and_then(|i| self.term(NodeKind::Industry, i))
        {
            self.link(&org, &i, rel::IN_INDUSTRY, BTreeMap::new());
        }
        if let Some(p) = c
            .headquarters
            .as_deref()
            .and_then(|p| self.term(NodeKind::Place, p))
        {
            self.link(&org, &p, rel::HEADQUARTERED_IN, BTreeMap::new());
        }
        for spec in c.specialties.iter().flat_map(|s| s.split([',', ';'])) {
            if let Some(sp) = self.term(NodeKind::Specialty, spec) {
                self.link(&org, &sp, rel::HAS_SPECIALTY, BTreeMap::new());
            }
        }
        for e in &c.employees {
            let Some(p) = self.entity(
                NodeKind::Person,
                e.linkedin_url.as_deref(),
                &e.name,
                props(&[("headline", s(&e.designation))]),
            ) else {
                continue;
            };
            let level = e.seniority.unwrap_or_else(|| {
                e.designation
                    .as_deref()
                    .map_or(Seniority::Unknown, seniority::classify)
            });
            self.link(
                &p,
                &org,
                rel::WORKS_AT,
                props(&[
                    ("title", s(&e.designation)),
                    ("seniority", Some(Value::from(level.as_str()))),
                    ("seniority_rank", Some(Value::from(level.rank()))),
                    ("source", Some(Value::from("company_people_page"))),
                ]),
            );
        }
    }

    /// Adds a profile with its positions, schools, languages, accomplishments
    /// and followed entities.
    pub fn add_person(&mut self, p: &Person) {
        let name = p.name.clone().unwrap_or_default();
        let contacts: Vec<Value> = p
            .contacts
            .iter()
            .filter(|c| c.contact_type != "linkedin")
            .map(|c| json!({"type": c.contact_type, "value": c.value, "label": c.label}))
            .collect();
        let headline_level = p.headline.as_deref().map(seniority::classify);
        let Some(me) = self.entity(
            NodeKind::Person,
            Some(&p.linkedin_url),
            &name,
            props(&[
                ("headline", s(&p.headline)),
                ("location", s(&p.location)),
                ("about", s(&p.about)),
                ("open_to_work", p.open_to_work.then_some(Value::Bool(true))),
                ("seniority", headline_level.map(|l| Value::from(l.as_str()))),
                (
                    "contacts",
                    (!contacts.is_empty()).then_some(Value::Array(contacts)),
                ),
            ]),
        ) else {
            return;
        };
        if let Some(pl) = p
            .location
            .as_deref()
            .and_then(|l| self.term(NodeKind::Place, l))
        {
            self.link(&me, &pl, rel::LOCATED_IN, BTreeMap::new());
        }

        for x in &p.experiences {
            let Some(org_name) = x.institution_name.as_deref() else {
                continue;
            };
            let Some(org) = self.entity(
                NodeKind::Organization,
                x.linkedin_url.as_deref(),
                org_name,
                BTreeMap::new(),
            ) else {
                continue;
            };
            let level = x
                .position_title
                .as_deref()
                .map_or(Seniority::Unknown, seniority::classify);
            let current = is_current(x.to_date.as_deref());
            self.link(
                &me,
                &org,
                if current {
                    rel::WORKS_AT
                } else {
                    rel::WORKED_AT
                },
                props(&[
                    ("title", s(&x.position_title)),
                    ("employment_type", s(&x.employment_type)),
                    ("from", s(&x.from_date)),
                    ("to", s(&x.to_date)),
                    ("duration", s(&x.duration)),
                    ("location", s(&x.location)),
                    ("seniority", Some(Value::from(level.as_str()))),
                    ("seniority_rank", Some(Value::from(level.rank()))),
                    ("source", Some(Value::from("profile"))),
                ]),
            );
        }

        for e in &p.educations {
            let Some(school_name) = e.institution_name.as_deref() else {
                continue;
            };
            if let Some(sc) = self.entity(
                NodeKind::School,
                e.linkedin_url.as_deref(),
                school_name,
                BTreeMap::new(),
            ) {
                self.link(
                    &me,
                    &sc,
                    rel::STUDIED_AT,
                    props(&[
                        ("degree", s(&e.degree)),
                        ("from", s(&e.from_date)),
                        ("to", s(&e.to_date)),
                    ]),
                );
            }
        }

        for a in &p.accomplishments {
            if a.category == "language" {
                if let Some(l) = self.term(NodeKind::Language, &a.title) {
                    self.link(
                        &me,
                        &l,
                        rel::SPEAKS,
                        props(&[("proficiency", s(&a.description))]),
                    );
                }
                continue;
            }
            let id = format!("accomplishment:{}:{}", a.category, slugify(&a.title));
            let node = self.upsert(
                id,
                NodeKind::Accomplishment,
                &a.title,
                props(&[
                    ("category", Some(Value::from(a.category.as_str()))),
                    ("issuer", s(&a.issuer)),
                    ("credential_url", s(&a.credential_url)),
                ]),
            );
            self.link(
                &me,
                &node,
                rel::HAS_ACCOMPLISHMENT,
                props(&[
                    ("date", s(&a.issued_date)),
                    ("credential_id", s(&a.credential_id)),
                ]),
            );
        }

        for i in &p.interests {
            let kind = match i.category.as_str() {
                "company" => NodeKind::Organization,
                "school" => NodeKind::School,
                "influencer" => NodeKind::Person,
                "group" => NodeKind::Group,
                "newsletter" => NodeKind::Newsletter,
                _ => continue,
            };
            // A newsletter's only link is its publisher: key it by name.
            let url = (kind != NodeKind::Newsletter)
                .then_some(i.linkedin_url.as_deref())
                .flatten();
            if let Some(t) = self.entity(kind, url, &i.name, BTreeMap::new()) {
                self.link(
                    &me,
                    &t,
                    rel::FOLLOWS,
                    props(&[("category", Some(Value::from(i.category.as_str())))]),
                );
            }
        }
    }

    /// Adds one `linkedin_batch` JSONL record (`{"kind": "person"|"company", "data": …}`).
    /// Returns `false` if the record is not a successful scrape.
    pub fn add_record(&mut self, record: &Value) -> bool {
        if record["status"].as_str().is_some_and(|s| s != "ok") {
            return false;
        }
        let data = record.get("data").unwrap_or(record);
        let kind = record["kind"].as_str().unwrap_or_else(|| {
            if data.get("employees").is_some() || data.get("industry").is_some() {
                "company"
            } else {
                "person"
            }
        });
        let added = match kind {
            "company" => serde_json::from_value::<Company>(data.clone())
                .map(|c| self.add_company(&c))
                .is_ok(),
            "person" => serde_json::from_value::<Person>(data.clone())
                .map(|p| self.add_person(&p))
                .is_ok(),
            _ => false,
        };
        // Records of name lines resolved by `linkedin_batch` carry the match
        // score: keep it on the node, so weak matches can be reviewed.
        if added
            && let Some(score) = record["resolution"]["score"].as_f64()
            && let Some((k, slug)) = data["linkedin_url"].as_str().and_then(linkedin_id)
            && let Some(n) = self.nodes.get_mut(&format!("{}:{slug}", prefix(k)))
        {
            n.properties
                .insert("resolution_score".into(), Value::from(score));
            if let Some(q) = record["input"].as_str() {
                n.properties
                    .insert("resolution_input".into(), Value::from(q));
            }
        }
        added
    }

    // ----------------------------------------------------------------------
    // Exports
    // ----------------------------------------------------------------------

    /// `{"nodes": [...], "edges": [...]}`.
    pub fn to_json(&self) -> Value {
        json!({ "nodes": self.nodes.values().collect::<Vec<_>>(), "edges": self.edges })
    }

    /// schema.org JSON-LD. Positions use the schema.org Role pattern
    /// (`worksFor` → `EmployeeRole` → `worksFor`), carrying title, dates and
    /// seniority (`lkg:` terms).
    pub fn to_json_ld(&self) -> Value {
        let iri = |n: &Node| -> String {
            n.properties
                .get("url")
                .and_then(Value::as_str)
                .map_or_else(|| format!("urn:lkg:{}", n.id), str::to_string)
        };
        let ids: HashMap<&str, String> = self
            .nodes
            .values()
            .map(|n| (n.id.as_str(), iri(n)))
            .collect();
        let mut objs: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
        for n in self.nodes.values() {
            let mut o = Map::new();
            o.insert("@id".into(), Value::String(ids[n.id.as_str()].clone()));
            o.insert("@type".into(), Value::from(n.kind.schema_type()));
            o.insert("name".into(), Value::from(n.name.clone()));
            for (k, v) in &n.properties {
                let key = match (n.kind, k.as_str()) {
                    (_, "website") => "sameAs".to_string(),
                    (_, "phone") => "telephone".to_string(),
                    (_, "founded") => "foundingDate".to_string(),
                    (_, "about") => "description".to_string(),
                    (_, "headcount") => "numberOfEmployees".to_string(),
                    (_, "url") => "url".to_string(),
                    (NodeKind::Person, "headline") => "jobTitle".to_string(),
                    (_, other) => format!("lkg:{other}"),
                };
                o.insert(key, v.clone());
            }
            objs.insert(n.id.as_str(), o);
        }
        let push = |o: &mut Map<String, Value>, key: &str, v: Value| match o.get_mut(key) {
            Some(Value::Array(a)) => a.push(v),
            Some(_) | None => {
                o.insert(key.to_string(), Value::Array(vec![v]));
            }
        };
        for e in &self.edges {
            let target = json!({"@id": ids.get(e.target.as_str()).cloned().unwrap_or_default()});
            let role = |prop: &str, ty: &str| {
                let mut r = Map::new();
                r.insert("@type".into(), Value::from(ty));
                r.insert(prop.into(), target.clone());
                for (k, v) in &e.properties {
                    let key = match k.as_str() {
                        "title" | "degree" => "roleName",
                        "from" => "startDate",
                        "to" => "endDate",
                        other => {
                            r.insert(format!("lkg:{other}"), v.clone());
                            continue;
                        }
                    };
                    r.insert(key.into(), v.clone());
                }
                Value::Object(r)
            };
            let (prop, value) = match e.relation.as_str() {
                rel::WORKS_AT => ("worksFor", role("worksFor", "EmployeeRole")),
                rel::WORKED_AT => ("lkg:workedFor", role("lkg:workedFor", "EmployeeRole")),
                rel::STUDIED_AT => ("alumniOf", role("alumniOf", "OrganizationRole")),
                rel::LOCATED_IN => ("homeLocation", target),
                rel::HEADQUARTERED_IN => ("location", target),
                rel::SPEAKS => ("knowsLanguage", target),
                rel::IN_INDUSTRY => ("lkg:industry", target),
                rel::HAS_SPECIALTY => ("knowsAbout", target),
                rel::FOLLOWS => ("follows", target),
                rel::HAS_ACCOMPLISHMENT => {
                    ("lkg:accomplishment", role("lkg:accomplishment", "Role"))
                }
                other => {
                    let key = format!("lkg:{}", other.to_lowercase());
                    if let Some(o) = objs.get_mut(e.source.as_str()) {
                        push(o, &key, target);
                    }
                    continue;
                }
            };
            if let Some(o) = objs.get_mut(e.source.as_str()) {
                push(o, prop, value);
            }
        }
        json!({
            "@context": {"@vocab": "https://schema.org/", "lkg": "urn:lkg:vocab:"},
            "@graph": objs.into_values().map(Value::Object).collect::<Vec<_>>(),
        })
    }

    /// Neo4j Cypher: uniqueness constraints, then `MERGE` for every node and
    /// relation (safe to run several times).
    pub fn to_cypher(&self) -> String {
        let mut out = String::new();
        let labels: std::collections::BTreeSet<&str> =
            self.nodes.values().map(|n| n.kind.label()).collect();
        for l in &labels {
            let _ = writeln!(
                out,
                "CREATE CONSTRAINT {}_id IF NOT EXISTS FOR (n:{l}) REQUIRE n.id IS UNIQUE;",
                l.to_lowercase()
            );
        }
        out.push('\n');
        for n in self.nodes.values() {
            let mut p = n.properties.clone();
            p.insert("name".into(), Value::from(n.name.clone()));
            let _ = writeln!(
                out,
                "MERGE (n:{} {{id: {}}}) SET n += {};",
                n.kind.label(),
                cypher_value(&Value::from(n.id.clone())),
                cypher_map(&p)
            );
        }
        out.push('\n');
        for e in &self.edges {
            let (Some(a), Some(b)) = (self.nodes.get(&e.source), self.nodes.get(&e.target)) else {
                continue;
            };
            // Several roles at the same company are told apart by title only, so
            // that re-importing a richer version of the same role updates it.
            let key: BTreeMap<String, Value> = e
                .properties
                .iter()
                .filter(|(k, _)| k.as_str() == "title")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let _ = writeln!(
                out,
                "MATCH (a:{} {{id: {}}}), (b:{} {{id: {}}}) MERGE (a)-[r:{} {}]->(b) SET r += {};",
                a.kind.label(),
                cypher_value(&Value::from(a.id.clone())),
                b.kind.label(),
                cypher_value(&Value::from(b.id.clone())),
                e.relation,
                cypher_map(&key),
                cypher_map(&e.properties)
            );
        }
        out
    }
}

/// Cypher literal. Nested objects (contacts) are stored as JSON strings,
/// since Neo4j properties cannot be maps.
fn cypher_value(v: &Value) -> String {
    match v {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => v.to_string(),
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
            format!(
                "[{}]",
                a.iter().map(cypher_value).collect::<Vec<_>>().join(", ")
            )
        }
        other => Value::String(other.to_string()).to_string(),
    }
}

fn cypher_map(m: &BTreeMap<String, Value>) -> String {
    let inner: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("{}: {}", cypher_key(k), cypher_value(v)))
        .collect();
    format!("{{{}}}", inner.join(", "))
}

fn cypher_key(k: &str) -> String {
    if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !k.starts_with(|c: char| c.is_ascii_digit())
    {
        k.to_string()
    } else {
        format!("`{}`", k.replace('`', "``"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Accomplishment, Education, Employee, Experience, Interest};

    fn company() -> Company {
        Company {
            linkedin_url: "https://www.linkedin.com/company/les-alchimistes-var/".into(),
            name: Some("Les Alchimistes Côte d'Azur".into()),
            industry: Some("Services de conseil en environnement".into()),
            headquarters: Some("Toulon, Provence-Alpes-Côte d’Azur".into()),
            specialties: Some("Biodéchet, Compostage".into()),
            headcount: Some(19),
            employees: vec![
                Employee::new(
                    "Jane Doe",
                    Some("Directrice générale".into()),
                    Some("https://www.linkedin.com/in/jane-doe/".into()),
                ),
                Employee::new(
                    "Bob \"B\" Lee",
                    Some("Chargé de collecte".into()),
                    Some("https://www.linkedin.com/in/bob-lee/".into()),
                ),
            ],
            ..Company::default()
        }
    }

    fn person() -> Person {
        Person {
            linkedin_url: "https://www.linkedin.com/in/jane-doe/".into(),
            name: Some("Jane Doe".into()),
            headline: Some("Directrice générale".into()),
            location: Some("Toulon".into()),
            experiences: vec![
                Experience {
                    position_title: Some("Directrice générale".into()),
                    institution_name: Some("Les Alchimistes Côte d'Azur".into()),
                    from_date: Some("2021".into()),
                    to_date: Some("aujourd’hui".into()),
                    ..Experience::default()
                },
                Experience {
                    position_title: Some("Consultante".into()),
                    institution_name: Some("Wavestone".into()),
                    linkedin_url: Some("https://www.linkedin.com/company/10133/".into()),
                    from_date: Some("2018".into()),
                    to_date: Some("2021".into()),
                    ..Experience::default()
                },
            ],
            educations: vec![Education {
                institution_name: Some("Lycée Hoche".into()),
                linkedin_url: Some("https://www.linkedin.com/school/lyc%C3%A9e-hoche/".into()),
                ..Education::default()
            }],
            accomplishments: vec![Accomplishment {
                category: "language".into(),
                title: "Anglais".into(),
                description: Some("Courant".into()),
                ..Accomplishment::default()
            }],
            interests: vec![Interest {
                name: "Sainte-Croix".into(),
                category: "company".into(),
                linkedin_url: Some("https://www.linkedin.com/school/sainte-croix/".into()),
            }],
            ..Person::default()
        }
    }

    fn graph() -> KnowledgeGraph {
        let mut g = KnowledgeGraph::new();
        g.add_company(&company());
        g.add_person(&person());
        g
    }

    #[test]
    fn merges_people_and_organizations() {
        let g = graph();
        // Jane from the people page and from her profile is one node.
        let jane = g.node("person:jane-doe").unwrap();
        assert_eq!(jane.properties["location"], "Toulon");
        // Her current position (no URL) is merged with the scraped company by name.
        let works: Vec<_> = g
            .edges()
            .iter()
            .filter(|e| e.relation == rel::WORKS_AT)
            .collect();
        let jane_works: Vec<_> = works
            .iter()
            .filter(|e| e.source == "person:jane-doe" && e.target == "company:les-alchimistes-var")
            .collect();
        assert_eq!(
            jane_works.len(),
            1,
            "people-page role and profile role merged"
        );
        assert_eq!(jane_works[0].properties["from"], "2021");
        assert_eq!(
            jane_works[0].properties["source"],
            "profile+company_people_page"
        );
        assert!(
            g.edges()
                .iter()
                .any(|e| e.relation == rel::WORKED_AT && e.target == "company:10133")
        );
        assert!(
            g.node("school:lycée-hoche").is_some(),
            "percent-decoded slug"
        );
        // URL type wins over the interest tab.
        assert_eq!(
            g.node("school:sainte-croix").unwrap().kind,
            NodeKind::School
        );
        assert!(g.node("language:anglais").is_some());
        assert!(g.node("specialty:compostage").is_some());
        // Seniority on employee edges.
        let bob = g
            .edges()
            .iter()
            .find(|e| e.source == "person:bob-lee")
            .unwrap();
        assert_eq!(bob.properties["seniority"], "individual");
    }

    #[test]
    fn current_roles() {
        assert!(is_current(Some("aujourd’hui")));
        assert!(is_current(Some("Present")));
        assert!(!is_current(Some("déc. 2021")));
        assert!(!is_current(None));
    }

    #[test]
    fn same_role_twice_is_one_edge() {
        let mut g = graph();
        let before = g.edges().len();
        g.add_company(&company());
        g.add_person(&person());
        assert_eq!(g.edges().len(), before);
    }

    #[test]
    fn exports() {
        let g = graph();
        let j = g.to_json();
        assert!(j["nodes"].as_array().unwrap().len() >= 8);

        let ld = g.to_json_ld();
        let jane = ld["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["@id"] == "https://www.linkedin.com/in/jane-doe/")
            .unwrap();
        assert_eq!(jane["@type"], "Person");
        assert_eq!(jane["worksFor"][0]["@type"], "EmployeeRole");
        assert_eq!(
            jane["worksFor"][0]["worksFor"]["@id"],
            "https://www.linkedin.com/company/les-alchimistes-var/"
        );

        let cy = g.to_cypher();
        assert!(cy.contains(
            "CREATE CONSTRAINT person_id IF NOT EXISTS FOR (n:Person) REQUIRE n.id IS UNIQUE;"
        ));
        assert!(cy.contains(r#"MERGE (n:Person {id: "person:bob-lee"}) SET n += {headline: "Chargé de collecte", name: "Bob \"B\" Lee", url: "https://www.linkedin.com/in/bob-lee/"};"#));
        assert!(cy.contains("-[r:WORKS_AT {title: \"Directrice générale\"}]->"));
    }

    #[test]
    fn batch_records() {
        let mut g = KnowledgeGraph::new();
        assert!(g.add_record(&json!({"kind": "company", "status": "ok", "data": company()})));
        assert!(!g.add_record(&json!({"kind": "company", "status": "error", "error": "x"})));
        // Resolved name line: the score lands on the node.
        let mut rec = json!({"kind": "company", "status": "ok", "data": company(),
                             "input": "company: Les Alchimistes", "resolution": {"score": 0.93}});
        rec["data"]["linkedin_url"] =
            json!("https://www.linkedin.com/company/les-alchimistes-var/");
        assert!(g.add_record(&rec));
        let n = g.node("company:les-alchimistes-var").unwrap();
        assert_eq!(n.properties["resolution_score"], 0.93);
        assert_eq!(n.properties["resolution_input"], "company: Les Alchimistes");
        assert!(g.node("company:les-alchimistes-var").is_some());
    }
}
