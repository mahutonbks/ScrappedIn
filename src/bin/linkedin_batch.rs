//! Batch scraper: reads a file of LinkedIn URLs, scrapes them one by one with
//! random pauses, waits automatically when LinkedIn blocks, writes JSONL and
//! resumes where it stopped.
//!
//! ```text
//! cargo run --release --bin linkedin_batch -- urls.txt -o results.jsonl
//! ```
//!
//! Run with `--help` for all options.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use linkedin_scraper::core::utils::{detect_rate_limit, jitter};
use linkedin_scraper::resolve::cache::query_key;
use linkedin_scraper::resolve::{Query, parse_query_line};
use linkedin_scraper::{
    BrowserManager, BrowserOptions, CompanyScraper, Error, PersonScraper, Resolution, ResolveCache,
    ResolveConfig, Resolver,
};
use serde_json::{Value, json};

const HELP: &str = "\
linkedin_batch — scrape a list of LinkedIn URLs into a JSONL file (resumable)

USAGE:
    linkedin_batch <URLS_FILE> [OPTIONS]

URLS_FILE: one URL per line (/in/… profiles or /company/… pages).
           Blank lines and lines starting with # are ignored.
           Name lines are resolved to URLs first (opt-in, see RESOLUTION):
             company: Les Alchimistes | domain=alchimistes.fr | location=Toulon
             person: Jeanne Dupont | company=Les Alchimistes | title=directrice
             person: Jean Martin | company=https://www.linkedin.com/company/acme/

OPTIONS:
    -o, --output <FILE>        JSONL output (default: results.jsonl)
    -s, --session <FILE>       Session file (default: linkedin_session.json)
        --min-delay <SECS>     Minimum pause between URLs (default: 30)
        --max-delay <SECS>     Maximum pause between URLs (default: 90)
        --long-pause-every <N> Take a long pause every N URLs (default: 20, 0 = never)
        --long-pause <SECS>    Length of the long pause (default: 600)
        --limit <N>            Process at most N URLs in this run
        --employees <N>        For companies, also collect up to N employees, sorted by
                               seniority (default: 0 = none)
        --max-attempts <N>     Give up on a URL after N failed attempts across runs (default: 3)
        --max-blocks <N>       Stop after N consecutive blocks by LinkedIn (default: 3)
        --max-wait <SECS>      Cap for a single block wait (default: 7200)
        --headful              Show the browser window; when LinkedIn shows a security
                               check, wait for you to solve it by hand before retrying
        --solve-timeout <SECS> With --headful: time allowed to solve a check (default: 180)
        --resolve-only         Name lines: resolve to URLs only, do not scrape
        --resolve-cache <FILE> Resolution cache (default: resolutions.json)
        --refresh              Search again for cached names (manual cache entries are kept)
        --accept-score <X>     Min score to accept a match (default: 0.85)
        --min-margin <X>       Min gap between best and second candidate (default: 0.15)
        --min-score <X>        Min score to list a candidate as ambiguous (default: 0.50)
    -h, --help                 Show this help

OUTPUT: one JSON object per line:
    {\"url\", \"kind\": \"person\"|\"company\", \"status\": \"ok\"|\"error\"|\"skipped\",
     \"scraped_at\", \"data\" | \"error\"}

RESUME: rerun the same command. URLs already \"ok\" or \"skipped\" in the output
are not scraped again; failed ones are retried up to --max-attempts.

RESOLUTION: a name line runs LinkedIn searches (they count as pages for the
pauses and block handling). \"resolved\" → the URL is scraped as if given
directly and the record gets a \"resolution\" field (score, reasons). Ambiguous
or unknown names get a \"ambiguous\" / \"not_found\" record with the candidates and
are retried only after their cache entry changes (e.g. you set \"manual_url\"
in the cache) or with --refresh.

EXIT CODES: 0 done, 1 usage/IO error, 2 stopped because LinkedIn keeps
blocking (rerun later to resume), 3 session expired (run create_session),
4 LinkedIn monthly search limit reached (name resolution stopped).
";

#[derive(Debug, Clone)]
struct Config {
    input: PathBuf,
    output: PathBuf,
    session: PathBuf,
    min_delay: f64,
    max_delay: f64,
    long_pause_every: usize,
    long_pause: f64,
    limit: Option<usize>,
    max_attempts: usize,
    max_blocks: u32,
    max_wait: u64,
    headful: bool,
    solve_timeout: u64,
    employees: usize,
    resolve_only: bool,
    resolve_cache: PathBuf,
    refresh: bool,
    resolve: ResolveConfig,
}

fn parse_args(args: &[String]) -> Result<Option<Config>, String> {
    let mut cfg = Config {
        input: PathBuf::new(),
        output: "results.jsonl".into(),
        session: "linkedin_session.json".into(),
        min_delay: 30.0,
        max_delay: 90.0,
        long_pause_every: 20,
        long_pause: 600.0,
        limit: None,
        max_attempts: 3,
        max_blocks: 3,
        max_wait: 7200,
        headful: false,
        solve_timeout: 180,
        employees: 0,
        resolve_only: false,
        resolve_cache: "resolutions.json".into(),
        refresh: false,
        resolve: ResolveConfig::default(),
    };
    let mut input = None;
    let mut it = args.iter();
    fn val<'a>(
        it: &mut impl Iterator<Item = &'a String>,
        flag: &str,
    ) -> Result<&'a String, String> {
        it.next().ok_or_else(|| format!("missing value for {flag}"))
    }
    fn num<T: std::str::FromStr>(s: &str, flag: &str) -> Result<T, String> {
        s.parse()
            .map_err(|_| format!("invalid number for {flag}: {s}"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "-o" | "--output" => cfg.output = val(&mut it, a)?.into(),
            "-s" | "--session" => cfg.session = val(&mut it, a)?.into(),
            "--min-delay" => cfg.min_delay = num(val(&mut it, a)?, a)?,
            "--max-delay" => cfg.max_delay = num(val(&mut it, a)?, a)?,
            "--long-pause-every" => cfg.long_pause_every = num(val(&mut it, a)?, a)?,
            "--long-pause" => cfg.long_pause = num(val(&mut it, a)?, a)?,
            "--limit" => cfg.limit = Some(num(val(&mut it, a)?, a)?),
            "--max-attempts" => cfg.max_attempts = num(val(&mut it, a)?, a)?,
            "--max-blocks" => cfg.max_blocks = num(val(&mut it, a)?, a)?,
            "--max-wait" => cfg.max_wait = num(val(&mut it, a)?, a)?,
            "--headful" => cfg.headful = true,
            "--employees" => cfg.employees = num(val(&mut it, a)?, a)?,
            "--solve-timeout" => cfg.solve_timeout = num(val(&mut it, a)?, a)?,
            "--resolve-only" => cfg.resolve_only = true,
            "--resolve-cache" => cfg.resolve_cache = val(&mut it, a)?.into(),
            "--refresh" => cfg.refresh = true,
            "--accept-score" => cfg.resolve.accept_score = num(val(&mut it, a)?, a)?,
            "--min-margin" => cfg.resolve.min_margin = num(val(&mut it, a)?, a)?,
            "--min-score" => cfg.resolve.min_score = num(val(&mut it, a)?, a)?,
            s if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            s if input.is_none() => input = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument: {s}")),
        }
    }
    cfg.input = input.ok_or("missing URLS_FILE")?;
    if cfg.min_delay < 0.0 || cfg.max_delay < cfg.min_delay {
        return Err("--max-delay must be >= --min-delay >= 0".into());
    }
    cfg.max_attempts = cfg.max_attempts.max(1);
    cfg.max_blocks = cfg.max_blocks.max(1);
    Ok(Some(cfg))
}

// --------------------------------------------------------------------------
// URLs
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Person,
    Company,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Company => "company",
        }
    }
}

/// Canonical form used as the resume key: `https://www.linkedin.com/in/<id>/`.
fn normalize_url(raw: &str) -> Option<(String, Kind)> {
    let raw = raw.trim();
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let u = url::Url::parse(&with_scheme).ok()?;
    let host = u.host_str()?.to_lowercase();
    if host != "linkedin.com" && !host.ends_with(".linkedin.com") {
        return None;
    }
    let segs: Vec<&str> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let kind = match segs.first().copied() {
        Some("in") => Kind::Person,
        Some("company") => Kind::Company,
        _ => return None,
    };
    let id = segs.get(1)?;
    Some((format!("https://www.linkedin.com/{}/{id}/", segs[0]), kind))
}

fn read_urls(path: &Path) -> std::io::Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = match parse_query_line(line) {
            Some(Ok(q)) => format!("query:{}", query_key(&q)),
            _ => normalize_url(line).map_or_else(|| line.to_string(), |(u, _)| u),
        };
        if seen.insert(key) {
            out.push(line.to_string());
        }
    }
    Ok(out)
}

// --------------------------------------------------------------------------
// Resume state
// --------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Progress {
    done: HashSet<String>,
    failures: HashMap<String, usize>,
    /// Name lines: latest record per input line.
    queries: HashMap<String, Value>,
    /// Name lines: failed attempts per input line.
    query_failures: HashMap<String, usize>,
}

/// Reads an existing JSONL output. Unparsable lines (e.g. a line cut by a
/// crash) are ignored.
fn load_progress(path: &Path) -> std::io::Result<Progress> {
    let mut p = Progress::default();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(p),
        Err(e) => return Err(e),
    };
    for line in BufReader::new(file).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(input) = v["input"].as_str() {
            if v["status"] == "error" {
                *p.query_failures.entry(input.to_string()).or_default() += 1;
            }
            p.queries.insert(input.to_string(), v.clone());
        }
        let (Some(url), Some(status)) = (v["url"].as_str(), v["status"].as_str()) else {
            continue;
        };
        match status {
            "ok" | "skipped" => {
                p.done.insert(url.to_string());
            }
            "error" => *p.failures.entry(url.to_string()).or_default() += 1,
            _ => {}
        }
    }
    Ok(p)
}

/// Opens the output for appending; adds a newline first if the last line was
/// cut (crash while writing).
fn open_output(path: &Path) -> std::io::Result<File> {
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let len = f.metadata()?.len();
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    Ok(f)
}

fn write_record(out: &mut File, record: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()?;
    out.sync_data()
}

// --------------------------------------------------------------------------
// Timing
// --------------------------------------------------------------------------

/// Wait after the `n`-th consecutive block: suggested × 2^(n-1), at least
/// 60 s, capped by `max_wait`, with ±10 % jitter.
fn block_wait(suggested: u64, n: u32, max_wait: u64) -> u64 {
    let base = suggested.max(60) as f64 * 2f64.powi(n.saturating_sub(1) as i32);
    let jittered = base * jitter(0.9, 1.1);
    (jittered as u64).min(max_wait)
}

async fn sleep_verbose(secs: f64, why: &str) {
    if secs <= 0.0 {
        return;
    }
    let until = chrono::Local::now() + chrono::Duration::milliseconds((secs * 1000.0) as i64);
    eprintln!(
        "   ⏸  {why}: {:.0}s (until {})",
        secs,
        until.format("%H:%M:%S")
    );
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

// --------------------------------------------------------------------------
// Main loop
// --------------------------------------------------------------------------

async fn scrape_one(
    browser: &BrowserManager,
    url: &str,
    kind: Kind,
    employees: usize,
) -> linkedin_scraper::Result<Value> {
    let page = browser.page()?;
    Ok(match kind {
        Kind::Person => PersonScraper::new(page).scrape(url).await?.to_dict(),
        Kind::Company => {
            let c = CompanyScraper::new(page)
                .with_employees(employees)
                .scrape(url)
                .await?;
            if c.name.as_deref().is_none_or(|n| n == "Unknown Company") {
                return Err(Error::Scraping(
                    "company page did not load (no name found)".into(),
                ));
            }
            c.to_dict()
        }
    })
}

/// `--headful`: if a security check is on screen, lets the user solve it.
/// Returns `true` once it is gone, `false` if there was none or on timeout.
async fn wait_for_manual_solve(browser: &BrowserManager, timeout: u64) -> bool {
    let Ok(page) = browser.page() else {
        return false;
    };
    if detect_rate_limit(&page).await.is_ok() {
        return false; // nothing on screen to solve
    }
    let _ = page.bring_to_front().await;
    eprintln!("   🧩 Security check on screen: solve it in the browser window ({timeout}s max)...");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(timeout) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if detect_rate_limit(&page).await.is_ok() {
            return true;
        }
    }
    eprintln!("   ⌛ Not solved in time.");
    false
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One line of work.
#[derive(Debug, Clone)]
enum Work {
    /// A LinkedIn URL: scraped exactly as before name resolution existed.
    Url(String, Kind),
    /// A name line, resolved first.
    Query { line: String, query: Query },
}

/// What happened to a name line.
struct QueryOutcome {
    status: &'static str,
    url: Option<String>,
    kind: Kind,
    resolution: Value,
    data: Option<Value>,
    note: Option<String>,
}

/// Compact, readable resolution summary for the JSONL record.
fn resolution_summary<T: serde::Serialize>(
    query: &Query,
    r: &Resolution<T>,
    url_of: impl Fn(&T) -> &str,
) -> Value {
    let cands: Vec<Value> = r
        .candidates()
        .iter()
        .map(|c| {
            let mut item = serde_json::to_value(&c.item).unwrap_or(Value::Null);
            if let Value::Object(m) = &mut item {
                m.insert("score".into(), json!(c.score));
                m.insert("reasons".into(), json!(c.reasons));
            }
            item
        })
        .collect();
    let best = r.best();
    json!({
        "query": query,
        "status": r.status(),
        "url": best.map(|b| url_of(&b.item)),
        "score": best.map(|b| b.score),
        "reasons": best.map(|b| b.reasons.clone()),
        "candidates": cands,
    })
}

fn query_kind(q: &Query) -> Kind {
    match q {
        Query::Company(_) => Kind::Company,
        Query::Person(_) => Kind::Person,
    }
}

/// Resolves a name line and, when resolved, scrapes the URL found.
async fn process_query(
    browser: &BrowserManager,
    resolver: &mut Resolver,
    query: &Query,
    cfg: &Config,
    done_urls: &HashSet<String>,
) -> linkedin_scraper::Result<QueryOutcome> {
    let kind = query_kind(query);
    let (resolution, url) = match query {
        Query::Company(q) => {
            let r = resolver.resolve_company(q).await?;
            (
                resolution_summary(query, &r, |h| h.url.as_str()),
                r.best().map(|b| b.item.url.clone()),
            )
        }
        Query::Person(q) => {
            let r = resolver.resolve_person(q).await?;
            (
                resolution_summary(query, &r, |h| h.url.as_str()),
                r.best().map(|b| b.item.url.clone()),
            )
        }
    };
    let status = resolution["status"].as_str().unwrap_or("not_found");
    let mut out = QueryOutcome {
        status: if status == "ambiguous" {
            "ambiguous"
        } else {
            "not_found"
        },
        url: url.clone(),
        kind,
        resolution,
        data: None,
        note: None,
    };
    let Some(url) = url else {
        return Ok(out);
    };
    if cfg.resolve_only {
        out.status = "resolved";
        return Ok(out);
    }
    if done_urls.contains(&url) {
        out.status = "skipped";
        out.note = Some(format!("already scraped as {url}"));
        return Ok(out);
    }
    if resolver.pages_loaded() > 0 {
        // The searches were pages too: pause before the profile like between URLs.
        sleep_verbose(jitter(cfg.min_delay, cfg.max_delay), "pause").await;
    }
    out.data = Some(scrape_one(browser, &url, kind, cfg.employees).await?);
    out.status = "ok";
    Ok(out)
}

/// The to-do list and the counters printed before starting.
struct Plan {
    todo: Vec<Work>,
    already_done: usize,
    given_up: usize,
    waiting: usize,
    /// Records to append before starting (invalid lines, once).
    skipped: Vec<Value>,
}

/// Decides what to do with each line. URL lines follow exactly the
/// historical rules; name lines are retried only when needed.
fn plan(urls: &[String], progress: &Progress, cfg: &Config, cache: Option<&ResolveCache>) -> Plan {
    let mut p = Plan {
        todo: Vec::new(),
        already_done: 0,
        given_up: 0,
        waiting: 0,
        skipped: Vec::new(),
    };
    for raw in urls {
        if let Some(parsed) = parse_query_line(raw) {
            let query = match parsed {
                Ok(q) => q,
                Err(e) => {
                    if !progress.queries.contains_key(raw.as_str()) {
                        eprintln!("⚠  skipped (invalid name line: {e}): {raw}");
                        p.skipped.push(json!({"input": raw, "url": null, "kind": null, "status": "skipped",
                                              "scraped_at": now(), "error": format!("invalid name line: {e}")}));
                    }
                    continue;
                }
            };
            let prior = progress.queries.get(raw.as_str());
            let prior_status = prior.and_then(|v| v["status"].as_str()).unwrap_or("");
            let stamp_changed = || {
                let now = cache.and_then(|c| c.stamp(&query_key(&query)));
                prior
                    .and_then(|v| v["cache_stamp"].as_str())
                    .map(str::to_string)
                    != now
            };
            let failures = progress
                .query_failures
                .get(raw.as_str())
                .copied()
                .unwrap_or(0);
            match prior_status {
                "ok" | "skipped" => p.already_done += 1,
                "resolved" if cfg.resolve_only => p.already_done += 1,
                "ambiguous" | "not_found" if !cfg.refresh && !stamp_changed() => p.waiting += 1,
                _ if failures >= cfg.max_attempts => p.given_up += 1,
                _ => p.todo.push(Work::Query {
                    line: raw.clone(),
                    query,
                }),
            }
            continue;
        }
        match normalize_url(raw) {
            None => {
                if !progress.done.contains(raw.as_str()) {
                    eprintln!("⚠  skipped (not a /in/ or /company/ LinkedIn URL): {raw}");
                    p.skipped
                        .push(json!({"url": raw, "kind": null, "status": "skipped",
                                          "scraped_at": now(), "error": "unsupported URL"}));
                }
            }
            Some((url, _)) if progress.done.contains(&url) => p.already_done += 1,
            Some((url, _))
                if progress.failures.get(&url).copied().unwrap_or(0) >= cfg.max_attempts =>
            {
                p.given_up += 1;
            }
            Some((url, kind)) => p.todo.push(Work::Url(url, kind)),
        }
    }
    if let Some(limit) = cfg.limit {
        p.todo.truncate(limit);
    }
    p
}

async fn run(cfg: Config) -> Result<ExitCode, String> {
    let urls =
        read_urls(&cfg.input).map_err(|e| format!("cannot read {}: {e}", cfg.input.display()))?;
    let progress = load_progress(&cfg.output)
        .map_err(|e| format!("cannot read {}: {e}", cfg.output.display()))?;
    let mut out = open_output(&cfg.output)
        .map_err(|e| format!("cannot open {}: {e}", cfg.output.display()))?;

    // Name lines need the resolution cache to decide what to retry.
    let has_queries = urls.iter().any(|l| parse_query_line(l).is_some());
    let cache = if has_queries {
        Some(
            ResolveCache::load(&cfg.resolve_cache)
                .map_err(|e| format!("cannot read {}: {e}", cfg.resolve_cache.display()))?,
        )
    } else {
        None
    };

    let Plan {
        todo,
        already_done,
        given_up,
        waiting,
        skipped,
    } = plan(&urls, &progress, &cfg, cache.as_ref());
    for rec in &skipped {
        write_record(&mut out, rec).map_err(|e| e.to_string())?;
    }
    if has_queries {
        eprintln!(
            "📋 {} lines in file · {already_done} already done · {given_up} given up after {} attempts · {waiting} unresolved names waiting for a cache edit or --refresh · {} to process now",
            urls.len(),
            cfg.max_attempts,
            todo.len()
        );
    } else {
        eprintln!(
            "📋 {} URLs in file · {already_done} already done · {given_up} given up after {} attempts · {} to scrape now",
            urls.len(),
            cfg.max_attempts,
            todo.len()
        );
    }
    if todo.is_empty() {
        eprintln!("✅ Nothing to do.");
        return Ok(ExitCode::SUCCESS);
    }

    if !cfg.session.exists() {
        return Err(format!(
            "{} not found. Run: cargo run --example create_session",
            cfg.session.display()
        ));
    }
    let mut browser = BrowserManager::launch(BrowserOptions::default().headless(!cfg.headful))
        .await
        .map_err(|e| format!("cannot launch browser: {e}"))?;
    browser
        .load_session(&cfg.session)
        .await
        .map_err(|e| format!("cannot load session: {e}"))?;

    let mut resolver = match cache {
        Some(cache) => {
            let page = browser.page().map_err(|e| e.to_string())?;
            let rcfg = ResolveConfig {
                // Every search page is paced like a profile page.
                pause_secs: Some((cfg.min_delay, cfg.max_delay)),
                ..cfg.resolve.clone()
            };
            Some(
                Resolver::new(page, rcfg)
                    .with_cache(cache)
                    .with_refresh(cfg.refresh),
            )
        }
        None => None,
    };
    let mut done_urls = progress.done.clone();

    let (mut ok, mut failed, mut unresolved) = (0usize, 0usize, 0usize);
    let mut code = ExitCode::SUCCESS;
    let total = todo.len();
    // Pages opened so far (a URL = 1, a name = its searches + the profile)
    // and long pauses taken: searches count like profile pages. With URLs
    // only, `units == i` and this is the historical "every N URLs" rule.
    let (mut units, mut long_pauses) = (0usize, 0usize);

    'urls: for (i, work) in todo.iter().enumerate() {
        if i > 0 {
            if cfg.long_pause_every > 0 && units / cfg.long_pause_every > long_pauses {
                long_pauses = units / cfg.long_pause_every;
                sleep_verbose(cfg.long_pause * jitter(0.8, 1.2), "long pause").await;
            } else {
                sleep_verbose(jitter(cfg.min_delay, cfg.max_delay), "pause").await;
            }
        }
        let (url, kind) = match work {
            Work::Url(url, kind) => (url, *kind),
            Work::Query { line, query } => {
                eprintln!("[{}/{total}] {} name: {line}", i + 1, query.kind());
                let resolver = resolver
                    .as_mut()
                    .expect("resolver exists when names are present");
                let mut blocks = 0u32;
                loop {
                    let before = resolver.pages_loaded();
                    resolver.reset_pace();
                    let res = process_query(&browser, resolver, query, &cfg, &done_urls).await;
                    units += (resolver.pages_loaded() - before) as usize;
                    match res {
                        Ok(o) => {
                            let mut rec = json!({"input": line, "url": o.url, "kind": o.kind.as_str(),
                                                 "status": o.status, "scraped_at": now(),
                                                 "resolution": o.resolution});
                            if let Some(d) = o.data {
                                rec["data"] = d;
                                units += 1;
                            }
                            if let Some(n) = o.note {
                                rec["note"] = json!(n);
                            }
                            if matches!(o.status, "ambiguous" | "not_found") {
                                rec["cache_stamp"] =
                                    json!(resolver.cache().stamp(&query_key(query)));
                                unresolved += 1;
                                eprintln!(
                                    "   ? {}: {} candidate(s) — see the record, or set \"manual_url\" in {}",
                                    o.status,
                                    rec["resolution"]["candidates"]
                                        .as_array()
                                        .map_or(0, Vec::len),
                                    cfg.resolve_cache.display()
                                );
                            } else {
                                if let Some(u) = &o.url {
                                    done_urls.insert(u.clone());
                                }
                                ok += usize::from(o.status == "ok" || o.status == "resolved");
                                eprintln!(
                                    "   ✓ {} {} (score {})",
                                    o.status,
                                    o.url.as_deref().unwrap_or(""),
                                    rec["resolution"]["score"]
                                );
                            }
                            write_record(&mut out, &rec).map_err(|e| e.to_string())?;
                            break;
                        }
                        Err(Error::SearchLimitReached(msg)) => {
                            eprintln!("🛑 {msg}");
                            eprintln!(
                                "   URL lines and cached names still work; rerun after the reset to resume."
                            );
                            code = ExitCode::from(4);
                            break 'urls;
                        }
                        Err(Error::RateLimit {
                            message,
                            suggested_wait_time,
                        }) => {
                            if cfg.headful
                                && wait_for_manual_solve(&browser, cfg.solve_timeout).await
                            {
                                eprintln!("   ✓ Check passed, retrying now");
                                tokio::time::sleep(Duration::from_secs_f64(jitter(3.0, 8.0))).await;
                                continue;
                            }
                            blocks += 1;
                            eprintln!(
                                "   ⛔ blocked by LinkedIn ({blocks}/{}): {message}",
                                cfg.max_blocks
                            );
                            if blocks >= cfg.max_blocks {
                                eprintln!(
                                    "🛑 LinkedIn keeps blocking. Stopping here; rerun the same command later to resume."
                                );
                                code = ExitCode::from(2);
                                break 'urls;
                            }
                            let wait = block_wait(suggested_wait_time, blocks, cfg.max_wait);
                            sleep_verbose(wait as f64, "waiting before retrying the same name")
                                .await;
                        }
                        Err(Error::Authentication(msg)) => {
                            eprintln!("🛑 Session invalid or expired: {msg}");
                            eprintln!(
                                "   Run `cargo run --example create_session`, then rerun to resume."
                            );
                            code = ExitCode::from(3);
                            break 'urls;
                        }
                        Err(e) => {
                            write_record(
                                &mut out,
                                &json!({"input": line, "url": null, "kind": query.kind(), "status": "error",
                                        "scraped_at": now(), "error": e.to_string()}),
                            )
                            .map_err(|e| e.to_string())?;
                            failed += 1;
                            eprintln!("   ✗ error: {e}");
                            break;
                        }
                    }
                }
                continue;
            }
        };
        units += 1;
        eprintln!("[{}/{total}] {} {url}", i + 1, kind.as_str());

        let mut blocks = 0u32;
        loop {
            match scrape_one(&browser, url, kind, cfg.employees).await {
                Ok(data) => {
                    write_record(
                        &mut out,
                        &json!({"url": url, "kind": kind.as_str(), "status": "ok",
                                "scraped_at": now(), "data": data}),
                    )
                    .map_err(|e| e.to_string())?;
                    done_urls.insert(url.clone());
                    ok += 1;
                    eprintln!("   ✓ ok");
                    break;
                }
                Err(Error::RateLimit {
                    message,
                    suggested_wait_time,
                }) => {
                    if cfg.headful && wait_for_manual_solve(&browser, cfg.solve_timeout).await {
                        eprintln!("   ✓ Check passed, retrying now");
                        tokio::time::sleep(Duration::from_secs_f64(jitter(3.0, 8.0))).await;
                        continue;
                    }
                    blocks += 1;
                    eprintln!(
                        "   ⛔ blocked by LinkedIn ({blocks}/{}): {message}",
                        cfg.max_blocks
                    );
                    if blocks >= cfg.max_blocks {
                        eprintln!(
                            "🛑 LinkedIn keeps blocking. Stopping here; rerun the same command later to resume."
                        );
                        code = ExitCode::from(2);
                        break 'urls;
                    }
                    let wait = block_wait(suggested_wait_time, blocks, cfg.max_wait);
                    sleep_verbose(wait as f64, "waiting before retrying the same URL").await;
                }
                Err(Error::Authentication(msg)) => {
                    eprintln!("🛑 Session invalid or expired: {msg}");
                    eprintln!("   Run `cargo run --example create_session`, then rerun to resume.");
                    code = ExitCode::from(3);
                    break 'urls;
                }
                Err(e) => {
                    write_record(
                        &mut out,
                        &json!({"url": url, "kind": kind.as_str(), "status": "error",
                                "scraped_at": now(), "error": e.to_string()}),
                    )
                    .map_err(|e| e.to_string())?;
                    failed += 1;
                    eprintln!("   ✗ error: {e}");
                    break;
                }
            }
        }
    }

    browser.close().await;
    if has_queries {
        eprintln!(
            "\n📊 This run: {ok} ok, {failed} errors, {unresolved} unresolved names · output: {} · cache: {}",
            cfg.output.display(),
            cfg.resolve_cache.display()
        );
    } else {
        eprintln!(
            "\n📊 This run: {ok} ok, {failed} errors · output: {}",
            cfg.output.display()
        );
    }
    Ok(code)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match parse_args(&args) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            return ExitCode::from(1);
        }
    };
    match run(cfg).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        parse_args(&["in.txt".to_string()]).unwrap().unwrap()
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plan_urls_unchanged() {
        // Same rules as before name resolution: canonical URL, done/failed
        // tracking by URL, invalid lines recorded once as "skipped".
        let mut progress = Progress::default();
        progress
            .done
            .insert("https://www.linkedin.com/in/done/".into());
        progress
            .failures
            .insert("https://www.linkedin.com/in/failing/".into(), 3);
        let urls = lines(&[
            "https://fr.linkedin.com/in/new?trk=x",
            "https://www.linkedin.com/in/done/",
            "https://www.linkedin.com/in/failing/",
            "https://www.linkedin.com/company/acme/about/",
            "https://example.com/x",
        ]);
        let p = plan(&urls, &progress, &cfg(), None);
        let todo: Vec<String> = p
            .todo
            .iter()
            .map(|w| match w {
                Work::Url(u, k) => format!("{} {u}", k.as_str()),
                Work::Query { .. } => panic!("no name line here"),
            })
            .collect();
        assert_eq!(
            todo,
            vec![
                "person https://www.linkedin.com/in/new/",
                "company https://www.linkedin.com/company/acme/"
            ]
        );
        assert_eq!((p.already_done, p.given_up, p.waiting), (1, 1, 0));
        assert_eq!(p.skipped.len(), 1);
        let rec = &p.skipped[0];
        let mut keys: Vec<&str> = rec
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["error", "kind", "scraped_at", "status", "url"]);
        assert_eq!(rec["error"], "unsupported URL");

        // Already recorded as skipped: not written again.
        progress.done.insert("https://example.com/x".into());
        assert!(plan(&urls, &progress, &cfg(), None).skipped.is_empty());
    }

    #[test]
    fn plan_name_lines() {
        let line = "company: Acme Compost | location=Toulon".to_string();
        let urls = vec![line.clone(), "company: | bad".to_string()];
        let cache = ResolveCache::in_memory();
        let mut progress = Progress::default();

        // New name line → to do; invalid one → skipped record with "input".
        let p = plan(&urls, &progress, &cfg(), Some(&cache));
        assert!(matches!(&p.todo[..], [Work::Query { .. }]));
        assert_eq!(p.skipped[0]["input"], "company: | bad");

        // Ambiguous with an unchanged cache entry → waits.
        progress.queries.insert(
            line.clone(),
            json!({"input": line, "status": "ambiguous", "cache_stamp": null}),
        );
        let p = plan(&urls[..1], &progress, &cfg(), Some(&cache));
        assert_eq!((p.todo.len(), p.waiting), (0, 1));

        // --refresh retries it.
        let mut c = cfg();
        c.refresh = true;
        assert_eq!(plan(&urls[..1], &progress, &c, Some(&cache)).todo.len(), 1);

        // A cache edit (stamp differs) retries it too.
        progress.queries.insert(
            line.clone(),
            json!({"input": line, "status": "not_found", "cache_stamp": "0000"}),
        );
        assert_eq!(
            plan(&urls[..1], &progress, &cfg(), Some(&cache)).todo.len(),
            1
        );

        // "resolved" is done for --resolve-only runs, to scrape otherwise.
        progress
            .queries
            .insert(line.clone(), json!({"input": line, "status": "resolved"}));
        assert_eq!(
            plan(&urls[..1], &progress, &cfg(), Some(&cache)).todo.len(),
            1
        );
        let mut c = cfg();
        c.resolve_only = true;
        assert_eq!(
            plan(&urls[..1], &progress, &c, Some(&cache)).already_done,
            1
        );

        // Done once ok.
        progress
            .queries
            .insert(line.clone(), json!({"input": line, "status": "ok"}));
        assert_eq!(
            plan(&urls[..1], &progress, &cfg(), Some(&cache)).already_done,
            1
        );
    }

    #[test]
    fn name_lines_are_deduplicated_by_query() {
        let dir = std::env::temp_dir().join(format!("lkbatch-q-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("in.txt");
        std::fs::write(
            &path,
            "person: Jean Martin | company=Acme SA\nperson: MARTIN Jean | company=acme\nhttps://www.linkedin.com/in/x\n",
        )
        .unwrap();
        assert_eq!(read_urls(&path).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalizes_urls() {
        let n = |s| normalize_url(s).map(|(u, k)| (u, k.as_str()));
        assert_eq!(
            n("https://fr.linkedin.com/in/caroline-witwer?trk=x"),
            Some((
                "https://www.linkedin.com/in/caroline-witwer/".into(),
                "person"
            ))
        );
        assert_eq!(
            n("www.linkedin.com/company/les-alchimistes-var/about/"),
            Some((
                "https://www.linkedin.com/company/les-alchimistes-var/".into(),
                "company"
            ))
        );
        assert_eq!(n("https://example.com/in/x"), None);
        assert_eq!(n("https://www.linkedin.com/feed/"), None);
        assert_eq!(n("https://www.linkedin.com/in/"), None);
    }

    #[test]
    fn block_wait_grows_and_is_capped() {
        let a = block_wait(600, 1, 7200);
        let b = block_wait(600, 2, 7200);
        assert!((540..=660).contains(&a));
        assert!((1080..=1320).contains(&b));
        assert_eq!(block_wait(3600, 3, 7200), 7200);
        assert!(block_wait(0, 1, 7200) >= 54);
    }

    #[test]
    fn random_in_range() {
        for _ in 0..100 {
            let r = jitter(30.0, 90.0);
            assert!((30.0..=90.0).contains(&r));
        }
    }

    #[test]
    fn resume_state() {
        let dir = std::env::temp_dir().join(format!("lkbatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.jsonl");
        std::fs::write(
            &path,
            "{\"url\":\"a\",\"status\":\"ok\"}\n{\"url\":\"b\",\"status\":\"error\"}\n{\"url\":\"b\",\"status\":\"error\"}\n{\"url\":\"c\",\"sta",
        )
        .unwrap();
        let p = load_progress(&path).unwrap();
        assert!(p.done.contains("a"));
        assert_eq!(p.failures.get("b"), Some(&2));
        assert!(!p.done.contains("c"));

        let mut f = open_output(&path).unwrap();
        write_record(&mut f, &json!({"url": "d", "status": "ok"})).unwrap();
        drop(f);
        let p = load_progress(&path).unwrap();
        assert!(
            p.done.contains("d"),
            "record after a cut line must still parse"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
