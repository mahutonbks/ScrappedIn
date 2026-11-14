# linkedin-scraper-rs

Portage en Rust de [`joeyism/linkedin_scraper`](https://github.com/joeyism/linkedin_scraper) (v3.1.2), une bibliothèque async de scraping LinkedIn.
Playwright est remplacé par [`chromiumoxide`](https://crates.io/crates/chromiumoxide), qui pilote Chrome directement via le protocole DevTools (CDP).

> ⚠️ Les conditions d'utilisation de LinkedIn interdisent le scraping automatisé : le compte utilisé peut être restreint ou banni. Utilise un compte de test, une cadence raisonnable, et respecte la vie privée des personnes dont tu récupères les données (RGPD).

## Fonctionnalités

Elles reprennent celles de la version Python :

| Python | Rust |
|---|---|
| `BrowserManager` (context manager async) | `BrowserManager::launch(opts)` + `close().await` |
| `PersonScraper`, `CompanyScraper`, `JobScraper`, `JobSearchScraper`, `CompanyPostsScraper` | mêmes noms, mêmes paramètres, même logique d'extraction |
| `login_with_credentials`, `login_with_cookie`, `wait_for_manual_login`, `is_logged_in`, `load_credentials_from_env` | idem (`.env` lu via `dotenvy`) |
| Modèles Pydantic (`Person`, `Experience`, `Company`, `Job`, `Post`, …) | structs `serde` + `validate()`, `to_json()`, `to_dict()`, `Display` (≈ `__repr__`) |
| Exceptions (`AuthenticationError`, `RateLimitError`, …) | `enum Error` (`thiserror`) |
| Callbacks (`Console`, `Silent`, `JSONLog`, `Multi`) | trait `ProgressCallback` + mêmes implémentations |
| `session.json` (format `storage_state` de Playwright) | **même format** : une session créée avec la lib Python se charge telle quelle, et inversement |

## Installation

Il faut Chrome ou Chromium installé (détection automatique, ou `BrowserOptions::executable(...)`).

```toml
[dependencies]
linkedin-scraper-rs = { path = "../linkedin-scraper-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Utilisation

```bash
# 1. Créer une session (connexion manuelle, 2FA/CAPTCHA compris)
cargo run --example create_session

# 2. Scraper
cargo run --example scrape_person -- https://www.linkedin.com/in/williamhgates/
cargo run --example scrape_company -- https://www.linkedin.com/company/microsoft/
cargo run --example scrape_company_posts -- https://www.linkedin.com/company/microsoft/ 5
cargo run --example scrape_jobs -- "rust developer" "Marseille" 3
cargo run --example scrape_login          # vérifie is_logged_in()
cargo run --example login_credentials     # connexion via LINKEDIN_EMAIL / LINKEDIN_PASSWORD
```

```rust
use linkedin_scraper::{BrowserManager, BrowserOptions, ConsoleCallback, PersonScraper};

#[tokio::main]
async fn main() -> linkedin_scraper::Result<()> {
    let mut browser = BrowserManager::launch(BrowserOptions::default()).await?;
    browser.load_session("linkedin_session.json").await?;

    let person = PersonScraper::new(browser.page()?)
        .with_callback(ConsoleCallback::default())
        .scrape("https://www.linkedin.com/in/williamhgates/")
        .await?;

    println!("{person}");
    println!("{}", person.to_json_pretty()?);
    browser.close().await;
    Ok(())
}
```

Les logs passent par `tracing` : active-les avec `tracing-subscriber` et `RUST_LOG=linkedin_scraper=debug`.

### Scraping en lot : `linkedin_batch`

```bash
# urls.txt : une URL /in/… ou /company/… par ligne (# = commentaire)
cargo run --release --bin linkedin_batch -- urls.txt -o results.jsonl
cargo run --release --bin linkedin_batch -- --help     # toutes les options
```

- **Pauses aléatoires** entre chaque URL (`--min-delay 30 --max-delay 90` par défaut) et une
  pause longue toutes les 20 URLs (`--long-pause-every`, `--long-pause`).
- **Blocages respectés** : sur un checkpoint, un CAPTCHA, un message de rate limit ou la page
  Cloudflare « Just a moment... », l'outil attend le délai suggéré (doublé à chaque blocage
  consécutif, plafonné par `--max-wait`) puis réessaie la même URL. Après `--max-blocks`
  blocages consécutifs, il s'arrête proprement (code 2). Si la session a expiré, il s'arrête
  avec le code 3.
- **JSONL** : une ligne par URL `{url, kind, status: ok|error|skipped, scraped_at, data|error}`,
  écrite et synchronisée sur le disque immédiatement.
- **Reprise** : relance la même commande. Les URLs déjà `ok` ou `skipped` sont sautées, les
  erreurs sont retentées jusqu'à `--max-attempts` (3). Les URLs sont normalisées
  (`fr.linkedin.com/in/x?trk=…` = `www.linkedin.com/in/x/`), donc pas de doublons.
- `--limit N` limite le nombre d'URLs traitées par exécution ; `--headful` affiche le navigateur.
- `--employees N` récupère aussi jusqu'à N employés pour chaque entreprise.

### Employés d'une entreprise et graphe de connaissance

```bash
# Employés (onglet « Personnes »), triés par niveau hiérarchique, + graphe Neo4j
cargo run --example scrape_company -- https://www.linkedin.com/company/acme/ --headful --employees 50 --graph acme.cypher

# Graphe fusionné de tous les résultats d'un lot (profils + entreprises)
cargo run --release --bin linkedin_graph -- results.jsonl -o graph.cypher   # ou .json / .jsonld
cypher-shell -u neo4j -p <mot de passe> -f graph.cypher
```

```rust
let company = CompanyScraper::new(page).with_employees(50).scrape(url).await?;
for e in &company.employees {           // déjà triés, du plus haut niveau au plus bas
    println!("{} {:?} {}", e.seniority_rank, e.seniority, e.name);
}
let mut g = KnowledgeGraph::new();
g.add_company(&company);                // + g.add_person(&person) pour fusionner des profils
std::fs::write("graph.cypher", g.to_cypher())?;
```

- **Niveaux** (`seniority`, rang 0–9) : `executive` (CEO, fondateur, PDG, gérant, associé…),
  `vice_president`, `director`, `head`, `manager` (responsable, chef de projet…), `senior`
  (lead, principal…), `individual`, `entry` (junior, doctorant…), `intern` (stagiaire,
  alternant…), `unknown`. Classement par mots-clés FR/EN/ES/DE sur l'intitulé ; les rôles
  passés (« ex-CEO », « ancien directeur ») et les assistants (« Assistante de direction »)
  ne comptent pas.
- **Graphe** : nœuds `Person`, `Organization`, `School`, `Industry`, `Place`, `Specialty`,
  `Language`, `Accomplishment`, `Group`, `Newsletter` ; relations `WORKS_AT` / `WORKED_AT`
  (titre, dates, niveau), `STUDIED_AT`, `IN_INDUSTRY`, `HEADQUARTERED_IN`, `LOCATED_IN`,
  `HAS_SPECIALTY`, `SPEAKS`, `HAS_ACCOMPLISHMENT`, `FOLLOWS`. Identifiants stables
  (`person:<slug>`, `company:<slug>`…) : une même personne vue sur la page « Personnes » et
  sur son profil n'est qu'un nœud, et son poste qu'une relation.
- **Formats** : `.json` (`{nodes, edges}` pour NetworkX, Gephi, Cytoscape…), `.jsonld`
  (schema.org, rôles via `EmployeeRole`), `.cypher` (contraintes d'unicité + `MERGE`,
  réimportable sans doublons).

### Résolution par nom

Au lieu d'une URL, une ligne du fichier peut donner **un nom** et des indices. L'outil cherche
alors le profil avec les pages de recherche LinkedIn, dans le navigateur déjà connecté (pas
d'API, pas d'appel HTTP hors navigateur). Les lignes d'URL se comportent exactement comme avant.

```text
company: Les Alchimistes | domain=alchimistes.fr | location=Toulon | industry=déchets
person: Jeanne Dupont | company=Les Alchimistes | title=directrice | location=Var
person: Jean Martin | company=https://www.linkedin.com/company/acme/
person: Jean Martin | company=Acme | company_domain=acme.fr
```

- Préfixes `company:`/`entreprise:` et `person:`/`personne:`. Clés : `domain`/`domaine`/`site`,
  `location`/`lieu`/`ville`, `industry`/`secteur`, `company`/`entreprise`, `title`/`poste`/`titre`.
- **Entreprise** : recherche (`/search/results/companies/`), puis score de chaque résultat. Avec
  `domain`, la page `/about/` des 3 meilleurs candidats au plus est ouverte pour comparer le site
  web, le signal le plus fort. Elle donne aussi l'ID numérique de l'entreprise.
- **Personne** : si l'entreprise est connue, son ID sert de filtre `currentCompany`. Quand l'ID
  vient du lien « N employés » de la page entreprise, il est sûr et une seule recherche suffit.
  Quand il ne vient que des données de la page (URN le plus fréquent), il est incertain :
  l'outil lance la recherche filtrée **et** la recherche `"<nom> <entreprise>"` sans filtre, puis
  compare les deux. L'ID est confirmé ou écarté, et la comparaison est notée dans `reasons`.
  LinkedIn ignore le filtre de poste (`titleFreeText`). Le poste sert donc au score : mots du
  titre, puis niveau hiérarchique. Si le résultat reste ambigu avec une page pleine, une
  recherche `"<nom> <poste>"` est faite en plus.
- **Score** dans [0, 1] : moyenne pondérée des indices fournis. Entreprise : nom 1, domaine 2,
  lieu 0,5, secteur 0,4. Personne : nom 3, entreprise 2, poste 1, lieu 0,6. Un indice non
  vérifiable compte 0,5, et un nom trop différent plafonne le score. Le détail est dans
  `src/resolve/score.rs`.
- **Décision** : `resolved` si score ≥ `--accept-score` (0,85) **et** écart avec le deuxième
  ≥ `--min-margin` (0,15) ; `ambiguous` si au moins un candidat ≥ `--min-score` (0,50) ;
  sinon `not_found`. Le premier résultat n'est jamais choisi par défaut.
- **Sortie** : `resolved` → l'URL est scrapée comme une URL donnée directement, et le record
  reçoit `input` et `resolution` (score, raisons, candidats). `ambiguous` / `not_found` → record
  avec les candidats, sans scraping. `--resolve-only` ne fait que la résolution.
- **Cache** `resolutions.json` (`--resolve-cache`) : une requête déjà résolue n'est jamais
  recherchée à nouveau, sauf avec `--refresh`. Pour trancher à la main, ajoute
  `"manual_url": "https://www.linkedin.com/in/…/"` à l'entrée, ou corrige `resolution` et mets
  `"manual": true`. Ces entrées sont respectées même avec `--refresh`. Une ligne
  `ambiguous` / `not_found` n'est retentée que si son entrée de cache a changé, ou avec `--refresh`.
- **Rythme** : chaque page de recherche ou `/about/` compte comme une page de profil (pauses
  aléatoires, pauses longues, gestion des blocages). Une recherche lit au plus 1 page de
  10 résultats, 2 si nécessaire.
- **Limite des comptes gratuits** : LinkedIn plafonne chaque mois les recherches de profils
  (« commercial use limit »). Quand elle est atteinte, le batch s'arrête proprement (code de
  sortie 4) avec `Error::SearchLimitReached`. Les lignes d'URL et les noms déjà en cache
  fonctionnent toujours. Cette page n'a pas encore été observée en vrai : sa détection repose
  sur le texte, et seulement quand la page n'a aucun résultat.

Dans le code : `Resolver::new(page, ResolveConfig::default()).with_cache(…)`, puis
`resolve_company(&CompanyQuery)` / `resolve_person(&PersonQuery)`. Les scrapers de recherche
`CompanySearch` et `PeopleSearch` s'utilisent aussi seuls. En ligne de commande :
`cargo run --example resolve_name -- company "Acme" --domain acme.fr`, et `compare-ids <URLs…>`
pour comparer les deux méthodes d'obtention de l'ID d'une entreprise.

## Architecture

```
src/
├── core/
│   ├── browser.rs   BrowserManager, sessions (format Playwright)
│   ├── page.rs      Page + Locator : API « à la Playwright » sur chromiumoxide
│   ├── locator.js   moteur de sélecteurs exécuté dans la page
│   ├── auth.rs      login, cookie, détection de connexion
│   └── utils.rs     rate-limit, scroll, retry, modales…
├── scrapers/        person, company, job, job_search, company_posts,
│                    company_search, people_search (résolution par nom)
├── resolve/         normalisation, score, décision, cache, Resolver
├── models/          Person, Company, Job, Post…
├── callbacks.rs
└── error.rs
```

Il n'existe pas de binding Playwright maintenu en Rust. Or le code Python repose beaucoup sur les *locators* Playwright (`:has-text()`, `xpath=ancestor::…`, `> *`, `text="…"`, `.first`, `.all()`, attente automatique). `core/page.rs` réimplémente ce sous-ensemble : un `Locator` est une chaîne de sélecteurs paresseuse, résolue dans la page par `locator.js` à chaque action. Comme avec Playwright, un locator ne devient donc jamais « périmé ».

## Différences avec la version Python

- **Pas de context manager async** : appelle `browser.close().await` à la fin (le processus Chrome est quand même tué au `Drop`).
- **`load_session`** vide puis recharge les cookies du navigateur au lieu de recréer un contexte. Les `Page` déjà obtenues restent valides.
- **Pas de *strict mode*** : une action sur un locator qui correspond à plusieurs éléments utilise le premier.
- **`wait_until`** : `goto` attend l'événement `load`. `NetworkIdle` est approximé par `load` suivi de 500 ms de calme.
- **Bug corrigé** : en Python, la détection de CAPTCHA lève `RateLimitError` à l'intérieur d'un `try/except Exception: pass`, donc l'erreur n'est jamais remontée. Ici, elle l'est.
- **Optimisation** : les boucles « un appel `inner_text()` par élément » (des centaines d'allers-retours CDP) sont faites en un seul appel (`all_inner_texts`, `evaluate_all`). Le résultat est identique.
- Les méthodes privées jamais appelées de `CompanyPostsScraper` (`_parse_post_element`, etc.) n'ont pas été portées, car tout passe par l'extraction JS.
- **Comme en Python**, `urljoin` retire le dernier segment d'une URL de profil sans `/` final : passe toujours `…/in/<id>/`.

## Tests

```bash
cargo test
```

- **Tests unitaires** : parsing des dates, accomplissements, compteurs, URLs, format de session, callbacks, retry.
- **Tests de bout en bout** (`tests/e2e.rs`) : un petit serveur HTTP local sert de fausses pages LinkedIn, et les vrais scrapers tournent dessus dans un Chrome headless. Ça couvre le profil complet (expériences, formations, intérêts avec clic sur les onglets, certifications, contacts), l'entreprise, l'offre d'emploi, les posts et l'aller-retour de session. Ces tests sont ignorés si aucun Chrome n'est installé.

Ces tests ne remplacent pas un essai sur le vrai LinkedIn, dont le DOM change souvent. Les sélecteurs sont repris tels quels de la version Python.

## Licence

GPL-3.0 : ce portage est une œuvre dérivée de `linkedin_scraper`, qui est sous GPL-3.0 (le fichier `LICENSE` du dépôt d'origine fait foi, même si son README mentionne Apache 2.0).
