//! Visit counts for the operator of a hosted server.
//!
//! Every hour the server reads the web server's access logs (nginx's
//! "combined" format) and saves a count for each day, so the counts stay after
//! the logs rotate. It saves counts only: never an address, a browser, or a
//! path from the app, where a path can hold a token. GitHub gives the download
//! counts of the release files, and the desktop app reports each time it opens
//! (`record_open`).
//!
//! A visitor is one address with one browser, and a visit is one visitor on
//! one day. These visitors are left out:
//! - bots: a browser name that is not a person's browser, a request that only
//!   attacks send, or three or more missing pages;
//! - the operator: any visitor that opened /operator on the app.
//!
//! The first day in a log can be there only in part (the log rotated during
//! that day), so its counts never make a saved count smaller. Every later day
//! is there in full, and its counts replace the saved ones.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{Days, NaiveDate, Utc};
use sqlx::SqlitePool;

use super::Hosted;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Log {
    /// The website, such as packeeper.ca.
    Site,
    Demo,
    App,
}

impl Log {
    pub const ALL: [Log; 3] = [Log::Site, Log::Demo, Log::App];

    pub fn key(self) -> &'static str {
        match self {
            Log::Site => "site",
            Log::Demo => "demo",
            Log::App => "app",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Log::Site => "Website",
            Log::Demo => "Demo",
            Log::App => "App",
        }
    }

    fn setting(self) -> &'static str {
        match self {
            Log::Site => "PAC_STATS_SITE_LOG",
            Log::Demo => "PAC_STATS_DEMO_LOG",
            Log::App => "PAC_STATS_APP_LOG",
        }
    }
}

/// Where the counts come from. Empty unless the operator sets them.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub logs: Vec<(Log, PathBuf)>,
    /// A GitHub repository, such as sjhewitt/packeeper, for download counts.
    pub github: Option<String>,
}

impl Sources {
    /// PAC_STATS_SITE_LOG, PAC_STATS_DEMO_LOG, PAC_STATS_APP_LOG, and
    /// PAC_STATS_GITHUB.
    pub fn from_env() -> anyhow::Result<Sources> {
        let set = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let logs = Log::ALL.into_iter().filter_map(|log| set(log.setting()).map(|p| (log, PathBuf::from(p)))).collect();
        let github = set("PAC_STATS_GITHUB");
        if let Some(repo) = &github {
            anyhow::ensure!(valid_repo(repo), "PAC_STATS_GITHUB must look like owner/repository");
        }
        Ok(Sources { logs, github })
    }

    pub fn is_empty(&self) -> bool {
        self.logs.is_empty() && self.github.is_none()
    }

    /// The sources in words, for the start-up message.
    pub fn describe(&self) -> String {
        let mut parts: Vec<&str> = self.logs.iter().map(|(log, _)| log.title()).collect();
        if self.github.is_some() {
            parts.push("GitHub downloads");
        }
        parts.join(", ")
    }
}

fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    matches!(repo.split_once('/'), Some((owner, name)) if ok(owner) && ok(name))
}

/// Reads the sources now, then every hour.
pub fn spawn_hourly(hosted: Arc<Hosted>) {
    if hosted.stats.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            if let Err(e) = collect(&hosted.control, &hosted.stats).await {
                tracing::warn!("could not save the visit counts: {e:#}");
            }
        }
    });
}

/// Reads every source and saves the counts. When a source fails, stat_checks
/// records the error, and the other sources still count.
pub async fn collect(control: &SqlitePool, sources: &Sources) -> anyhow::Result<()> {
    let logs = sources.logs.clone();
    let tallies = tokio::task::spawn_blocking(move || tally_logs(&logs)).await?;
    for (log, result) in tallies {
        match result {
            Ok(tally) => {
                save(control, log, &tally).await?;
                checked(control, log.key(), "").await?;
            }
            Err(e) => {
                tracing::warn!("could not read the {} log: {e:#}", log.key());
                checked(control, log.key(), &format!("{e:#}")).await?;
            }
        }
    }
    if let Some(repo) = sources.github.clone() {
        match tokio::task::spawn_blocking(move || fetch_downloads(&repo)).await? {
            Ok(totals) => {
                save_downloads(control, &totals).await?;
                checked(control, "downloads", "").await?;
            }
            Err(e) => {
                tracing::warn!("could not get the download counts: {e:#}");
                checked(control, "downloads", &format!("{e:#}")).await?;
            }
        }
    }
    Ok(())
}

/// One request in the log.
struct Request<'a> {
    addr: &'a str,
    day: NaiveDate,
    method: &'a str,
    target: &'a str,
    status: u16,
    referrer: &'a str,
    agent: &'a str,
}

impl Request<'_> {
    fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or_default()
    }

    fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, q)| q)
    }

    /// The visitor, as a number that lives only while the log is read.
    fn visitor(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.addr.hash(&mut h);
        self.agent.hash(&mut h);
        h.finish()
    }
}

/// One line of nginx's "combined" format:
/// `ADDR - USER [13/Oct/2026:02:48:57 +0000] "GET / HTTP/2.0" 200 146 "REFERRER" "BROWSER"`.
/// nginx writes a quote inside a field as \x22, so quotes split the fields.
fn parse(line: &str) -> Option<Request<'_>> {
    let mut parts = line.split('"');
    let head = parts.next()?;
    let request = parts.next()?;
    let middle = parts.next()?;
    let referrer = parts.next()?;
    parts.next()?;
    let agent = parts.next()?;
    let addr = head.split(' ').next()?;
    let time = head.split_once('[')?.1.split_once(']')?.0;
    let day = chrono::DateTime::parse_from_str(time, "%d/%b/%Y:%H:%M:%S %z").ok()?.with_timezone(&Utc).date_naive();
    let mut request = request.split(' ');
    let method = request.next().unwrap_or_default();
    let target = request.next().unwrap_or_default();
    let status = middle.split_whitespace().next()?.parse().ok()?;
    Some(Request { addr, day, method, target, status, referrer, agent })
}

/// What one log holds, by day.
#[derive(Default)]
struct Tally {
    /// The first day in the log, which it may hold only in part.
    first: Option<NaiveDate>,
    days: BTreeMap<NaiveDate, Day>,
}

#[derive(Default)]
struct Day {
    visitors: HashMap<u64, Visitor>,
    views: i64,
    signins: i64,
    signup_views: i64,
    pages: HashMap<String, i64>,
}

struct Visitor {
    phone: bool,
    /// Opened a page past the sign-in page (the demo).
    inside: bool,
    source: String,
}

impl Day {
    /// Each count as (measure, name, n), without zeros.
    fn counts(&self, log: Log) -> Vec<(&'static str, String, i64)> {
        let visitors = |f: fn(&Visitor) -> bool| self.visitors.values().filter(|v| f(v)).count() as i64;
        let mut rows = vec![
            ("visits", String::new(), self.visitors.len() as i64),
            ("views", String::new(), self.views),
            ("phones", String::new(), visitors(|v| v.phone)),
        ];
        match log {
            Log::Site => {}
            Log::Demo => {
                rows.push(("signins", String::new(), self.signins));
                rows.push(("inside", String::new(), visitors(|v| v.inside)));
            }
            Log::App => {
                rows.push(("signins", String::new(), self.signins));
                rows.push(("signup_views", String::new(), self.signup_views));
            }
        }
        let mut sources: HashMap<&str, i64> = HashMap::new();
        for v in self.visitors.values() {
            *sources.entry(&v.source).or_default() += 1;
        }
        rows.extend(sources.into_iter().map(|(s, n)| ("source", s.to_string(), n)));
        rows.extend(self.pages.iter().map(|(p, n)| ("page", p.clone(), *n)));
        rows.retain(|r| r.2 > 0);
        rows
    }
}

fn tally_logs(logs: &[(Log, PathBuf)]) -> Vec<(Log, anyhow::Result<Tally>)> {
    // First find the visitors to leave out, in every log: the operator opens
    // /operator on the app, and visits the website and the demo too.
    let mut skip = HashSet::new();
    let screened: Vec<_> = logs
        .iter()
        .map(|(log, path)| {
            let read = log_files(path).and_then(|files| screen(*log, &files, &mut skip).map(|own| (files, own)));
            (*log, read)
        })
        .collect();
    screened
        .into_iter()
        .map(|(log, read)| (log, read.and_then(|(files, own)| count(log, &files, &skip, &own))))
        .collect()
}

/// Adds the bots and the operator in this log to `skip`. Returns the site's
/// own host: the referrer of most of its pictures, style sheets, and scripts.
fn screen(log: Log, files: &[PathBuf], skip: &mut HashSet<u64>) -> anyhow::Result<String> {
    let mut missing: HashMap<u64, HashSet<String>> = HashMap::new();
    let mut hosts: HashMap<String, usize> = HashMap::new();
    each_line(files, |line| {
        let Some(r) = parse(line) else { return };
        let visitor = r.visitor();
        let operator = log == Log::App && r.status == 200 && r.path().starts_with("/operator");
        if operator || !is_browser(r.agent) || is_probe(&r) {
            skip.insert(visitor);
            return;
        }
        if r.status == 404 && !browser_asks_itself(r.path()) {
            let paths = missing.entry(visitor).or_default();
            paths.insert(r.path().to_string());
            if paths.len() >= 3 {
                skip.insert(visitor);
            }
        }
        if !is_page(r.path()) {
            *hosts.entry(referrer_host(r.referrer)).or_default() += 1;
        }
    })?;
    hosts.remove("");
    Ok(hosts.into_iter().max_by_key(|(_, n)| *n).map(|(host, _)| host).unwrap_or_default())
}

fn count(log: Log, files: &[PathBuf], skip: &HashSet<u64>, own: &str) -> anyhow::Result<Tally> {
    let mut tally = Tally::default();
    each_line(files, |line| {
        let Some(r) = parse(line) else { return };
        tally.first = Some(tally.first.map_or(r.day, |first| first.min(r.day)));
        // Every day in the log gets an entry, so a day of bots only saves zeros.
        let day = tally.days.entry(r.day).or_default();
        let visitor = r.visitor();
        if skip.contains(&visitor) {
            return;
        }
        let path = r.path();
        if log != Log::Site && r.method == "POST" && path == "/login" && r.status == 303 {
            day.signins += 1;
        }
        if r.method != "GET" || !matches!(r.status, 200 | 304) || !is_page(path) {
            return;
        }
        day.views += 1;
        let entry = day.visitors.entry(visitor).or_insert_with(|| Visitor {
            phone: ["Mobile", "Android", "iPhone"].iter().any(|w| r.agent.contains(w)),
            inside: false,
            source: source(&r, own),
        });
        match log {
            Log::Demo => {
                entry.inside |= path != "/login";
                *day.pages.entry(page_name(path)).or_default() += 1;
            }
            Log::App if path == "/signup" => day.signup_views += 1,
            _ => {}
        }
    })?;
    Ok(tally)
}

/// The log, and its older copies beside it, such as access.log.1. Compressed
/// copies are left out: by the time a log is compressed, its counts are saved.
fn log_files(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    anyhow::ensure!(path.is_file(), "cannot find {}", path.display());
    let mut files = vec![path.to_path_buf()];
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?.flatten() {
        let other = entry.file_name().to_string_lossy().into_owned();
        let compressed = [".gz", ".bz2", ".xz", ".zst"].iter().any(|e| other.ends_with(e));
        if other.len() > name.len() && other.starts_with(name) && !compressed && entry.path().is_file() {
            files.push(entry.path());
        }
    }
    Ok(files)
}

fn each_line(files: &[PathBuf], mut f: impl FnMut(&str)) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    for file in files {
        let mut reader = BufReader::new(std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?);
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf).with_context(|| format!("read {}", file.display()))? == 0 {
                break;
            }
            f(String::from_utf8_lossy(&buf).trim_end());
        }
    }
    Ok(())
}

/// People's browsers all start with Mozilla/. Bots that copy it mostly name
/// themselves or give a web address.
fn is_browser(agent: &str) -> bool {
    const BOT_WORDS: [&str; 10] = [
        "bot",
        "crawl",
        "spider",
        "scan",
        "headless",
        "preview",
        "http",
        "facebookexternalhit",
        "lighthouse",
        "python",
    ];
    let lower = agent.to_ascii_lowercase();
    agent.starts_with("Mozilla/") && !BOT_WORDS.iter().any(|w| lower.contains(w))
}

/// A request that only attacks and scanners send.
fn is_probe(r: &Request) -> bool {
    const PARTS: [&str; 20] = [
        ".php",
        "wp-",
        "/.env",
        "/.git",
        "cgi-bin",
        "xmlrpc",
        ".asp",
        "/.aws",
        "/.ds_store",
        "graphql",
        "actuator",
        "boaform",
        "hnap1",
        ".sql",
        ".bak",
        "_ignition",
        "telescope",
        "server-status",
        "autodiscover",
        "phpmyadmin",
    ];
    const STARTS: [&str; 12] = [
        "/admin", "/api", "/vendor", "/owa", "/ecp", "/console", "/solr", "/manager", "/remote", "/sdk", "/config",
        "/druid",
    ];
    if !matches!(r.method, "GET" | "HEAD" | "POST") || !r.target.starts_with('/') {
        return true;
    }
    let path = r.path().to_ascii_lowercase();
    PARTS.iter().any(|p| path.contains(p)) || STARTS.iter().any(|s| path.starts_with(s))
}

/// Files that browsers ask for on their own, so a person can miss them.
fn browser_asks_itself(path: &str) -> bool {
    matches!(path, "/favicon.ico" | "/robots.txt" | "/sitemap.xml")
        || path.starts_with("/apple-touch-icon")
        || path.starts_with("/.well-known/")
}

/// A page, not a picture, style sheet, or script.
fn is_page(path: &str) -> bool {
    const FILES: [&str; 15] = [
        ".css",
        ".js",
        ".png",
        ".jpg",
        ".jpeg",
        ".gif",
        ".svg",
        ".ico",
        ".webp",
        ".woff",
        ".woff2",
        ".txt",
        ".xml",
        ".json",
        ".webmanifest",
    ];
    !path.starts_with("/static/")
        && !path.starts_with("/img/")
        && !path.starts_with("/.well-known/")
        && !FILES.iter().any(|e| path.ends_with(e))
}

/// Where a visit came from: the referring site, or Facebook and Instagram when
/// their apps send no referrer. Empty when unknown. A visit "from" the site's
/// own host is unknown too: the visitor's address changed during the visit.
fn source(r: &Request, own: &str) -> String {
    let host = referrer_host(r.referrer);
    if !host.is_empty() {
        return if host == own { String::new() } else { host };
    }
    let facebook = r.query().split('&').any(|p| p.starts_with("fbclid="))
        || ["FBAN", "FBAV", "FB_IAB"].iter().any(|w| r.agent.contains(w));
    if facebook {
        return "facebook.com".into();
    }
    if r.agent.contains("Instagram") {
        return "instagram.com".into();
    }
    String::new()
}

/// The host of a referrer without www. and the mobile prefixes, so that
/// m.facebook.com and l.facebook.com count as facebook.com.
fn referrer_host(referrer: &str) -> String {
    let Some((_, rest)) = referrer.split_once("://") else { return String::new() };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default().split(':').next().unwrap_or_default();
    let host = host.to_ascii_lowercase();
    let host =
        ["www.", "m.", "l.", "lm.", "mobile."].iter().find_map(|p| host.strip_prefix(p)).unwrap_or(host.as_str());
    let plain = host.len() <= 100 && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if plain { host.to_string() } else { String::new() }
}

/// A demo page with its numbers left out, such as /transactions/{id}.
fn page_name(path: &str) -> String {
    let parts: Vec<&str> = path
        .split('/')
        .map(|s| if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) { "{id}" } else { s })
        .collect();
    parts.join("/").chars().take(80).collect()
}

async fn save(control: &SqlitePool, log: Log, tally: &Tally) -> anyhow::Result<()> {
    let mut tx = control.begin().await?;
    for (day, counts) in &tally.days {
        let day_text = day.to_string();
        if Some(*day) != tally.first {
            sqlx::query("DELETE FROM stat_counts WHERE day = ? AND log = ?")
                .bind(&day_text)
                .bind(log.key())
                .execute(&mut *tx)
                .await?;
        }
        for (measure, name, n) in counts.counts(log) {
            sqlx::query(
                "INSERT INTO stat_counts (day, log, measure, name, n) VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (day, log, measure, name) DO UPDATE SET n = MAX(n, excluded.n)",
            )
            .bind(&day_text)
            .bind(log.key())
            .bind(measure)
            .bind(&name)
            .bind(n)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn checked(control: &SqlitePool, source: &str, error: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO stat_checks (source, checked_at, error) VALUES (?, ?, ?)
         ON CONFLICT (source) DO UPDATE SET checked_at = excluded.checked_at, error = excluded.error",
    )
    .bind(source)
    .bind(Utc::now().format("%Y-%m-%d %H:%M").to_string())
    .bind(error)
    .execute(control)
    .await?;
    Ok(())
}

const PLATFORMS: [(&str, &str); 3] = [("mac", "Mac"), ("windows", "Windows"), ("linux", "Linux server")];

fn fetch_downloads(repo: &str) -> anyhow::Result<Vec<(&'static str, i64)>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut res = agent
        .get(&format!("https://api.github.com/repos/{repo}/releases?per_page=100"))
        .header("User-Agent", "pac-keeper")
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("ask GitHub for the releases")?;
    let body = res.body_mut().read_to_string()?;
    download_totals(&serde_json::from_str(&body)?)
}

/// The downloads of every release, by platform.
fn download_totals(releases: &serde_json::Value) -> anyhow::Result<Vec<(&'static str, i64)>> {
    let releases = releases.as_array().context("GitHub did not send a list of releases")?;
    let mut totals: Vec<(&'static str, i64)> = PLATFORMS.iter().map(|(key, _)| (*key, 0)).collect();
    for asset in releases.iter().filter_map(|r| r["assets"].as_array()).flatten() {
        let name = asset["name"].as_str().unwrap_or_default().to_ascii_lowercase();
        let platform = if name.ends_with(".dmg") {
            0
        } else if name.ends_with(".exe") || name.ends_with(".msi") {
            1
        } else if name.contains("linux") {
            2
        } else {
            continue;
        };
        totals[platform].1 += asset["download_count"].as_i64().unwrap_or(0);
    }
    Ok(totals)
}

/// GitHub's totals only grow, so each day keeps the last total of that day.
async fn save_downloads(control: &SqlitePool, totals: &[(&str, i64)]) -> anyhow::Result<()> {
    let today = Utc::now().date_naive().to_string();
    for (platform, n) in totals {
        sqlx::query(
            "INSERT INTO stat_counts (day, log, measure, name, n) VALUES (?, 'downloads', ?, '', ?)
             ON CONFLICT (day, log, measure, name) DO UPDATE SET n = excluded.n",
        )
        .bind(&today)
        .bind(platform)
        .bind(n)
        .execute(control)
        .await?;
    }
    Ok(())
}

/// The `log` of the desktop app's opens in stat_counts.
pub const DESKTOP: &str = "desktop";

/// The kinds of computer that the desktop app runs on.
const KINDS: [(&str, &str); 2] = [("mac", "Mac"), ("windows", "Windows")];

/// One open of the desktop app, as the app reports it. The app sends no name
/// or number for the computer. It says itself if this is its first open of
/// the day, so each computer counts once a day.
pub struct Open<'a> {
    pub version: &'a str,
    /// The kind of computer: mac or windows.
    pub kind: &'a str,
    /// The first open on this computer this day (UTC).
    pub first_today: bool,
    /// The first open that this computer reports.
    pub new: bool,
}

/// Counts one open of the desktop app. Anyone can send one, so the version
/// and the kind of computer must be known forms, or they count as "other".
pub async fn record_open(control: &SqlitePool, open: &Open<'_>) -> anyhow::Result<()> {
    let version = if valid_version(open.version) { open.version } else { "other" };
    let kind = KINDS.iter().find(|(key, _)| *key == open.kind).map_or("other", |(key, _)| key);
    let mut counts = vec![("opens", "")];
    if open.first_today {
        counts.extend([("computers", ""), ("kind", kind), ("version", version)]);
    }
    if open.new {
        counts.push(("new", ""));
    }
    let day = Utc::now().date_naive().to_string();
    let mut tx = control.begin().await?;
    for (measure, name) in counts {
        sqlx::query(
            "INSERT INTO stat_counts (day, log, measure, name, n) VALUES (?, ?, ?, ?, 1)
             ON CONFLICT (day, log, measure, name) DO UPDATE SET n = n + 1",
        )
        .bind(&day)
        .bind(DESKTOP)
        .bind(measure)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A release number, such as 0.5.2.
fn valid_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts.iter().all(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The name of a log's key, or of DESKTOP, in the summary.
fn title(key: &str) -> &'static str {
    Log::ALL.into_iter().find(|log| log.key() == key).map_or("Desktop app", Log::title)
}

/// The counts in the summary: the log, the measure, and its label.
const SUMMARY: [(&str, &str, &str); 12] = [
    ("site", "visits", "Visits"),
    ("site", "views", "Page views"),
    ("site", "phones", "Visits on phones and tablets"),
    ("demo", "visits", "Visits"),
    ("demo", "signins", "Sign-ins"),
    ("demo", "inside", "Visits that looked inside"),
    ("app", "visits", "Visits"),
    ("app", "signup_views", "Sign-up page views"),
    ("app", "signins", "Sign-ins"),
    (DESKTOP, "opens", "Opens"),
    (DESKTOP, "computers", "Computers that opened it"),
    (DESKTOP, "new", "New computers"),
];

/// The columns of the table by day: the log, the measure, and its heading.
pub const DAY_COLUMNS: [(&str, &str, &str); 7] = [
    ("site", "visits", "Website visits"),
    ("site", "views", "Website page views"),
    ("demo", "visits", "Demo visits"),
    ("demo", "signins", "Demo sign-ins"),
    ("app", "visits", "App visits"),
    ("app", "signup_views", "Sign-up page views"),
    (DESKTOP, "opens", "Desktop app opens"),
];

/// Everything the operator sees.
pub struct Report {
    pub rows: Vec<SummaryRow>,
    pub days: Vec<DayRow>,
    pub sources: Vec<SourceList>,
    pub pages: Vec<(String, i64)>,
    pub downloads: Vec<Download>,
    /// The desktop app in the last 30 days: computers by kind and by version.
    pub kinds: Vec<(String, i64)>,
    pub versions: Vec<(String, i64)>,
    pub accounts: Accounts,
    pub checks: Vec<Check>,
}

pub struct SummaryRow {
    pub log: &'static str,
    /// The first row of its log, which shows the log's name.
    pub first: bool,
    pub label: &'static str,
    pub today: i64,
    pub week: i64,
    pub month: i64,
    pub all: i64,
}

pub struct DayRow {
    pub day: String,
    pub counts: Vec<i64>,
}

pub struct SourceList {
    pub log: &'static str,
    pub sources: Vec<(String, i64)>,
}

pub struct Download {
    pub platform: &'static str,
    pub total: i64,
    /// Downloads in the last 7 days, once the counts are a week old.
    pub week: Option<i64>,
}

/// Accounts on the hosted server, without operators.
pub struct Accounts {
    pub people: i64,
    pub verified: i64,
    pub new_week: i64,
    pub pacs_active: i64,
    pub pacs_waiting: i64,
    pub parents: i64,
}

pub struct Check {
    pub source: &'static str,
    pub checked_at: String,
    pub error: String,
}

pub async fn report(control: &SqlitePool) -> anyhow::Result<Report> {
    let today = Utc::now().date_naive();
    let since = |days: u64| (today - Days::new(days)).to_string();
    let (week, month) = (since(6), since(29));

    let totals: Vec<(String, String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT log, measure,
                SUM(CASE WHEN day >= ? THEN n ELSE 0 END), SUM(CASE WHEN day >= ? THEN n ELSE 0 END),
                SUM(CASE WHEN day >= ? THEN n ELSE 0 END), SUM(n)
         FROM stat_counts WHERE name = '' GROUP BY log, measure",
    )
    .bind(today.to_string())
    .bind(&week)
    .bind(&month)
    .fetch_all(control)
    .await?;
    let rows = SUMMARY
        .iter()
        .enumerate()
        .map(|(i, (log, measure, label))| {
            let t = totals.iter().find(|t| t.0 == *log && t.1 == *measure);
            SummaryRow {
                log: title(log),
                first: i == 0 || SUMMARY[i - 1].0 != *log,
                label,
                today: t.map_or(0, |t| t.2),
                week: t.map_or(0, |t| t.3),
                month: t.map_or(0, |t| t.4),
                all: t.map_or(0, |t| t.5),
            }
        })
        .collect();

    let daily: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT day, log, measure, n FROM stat_counts WHERE name = '' AND log != 'downloads' AND day >= ?",
    )
    .bind(&month)
    .fetch_all(control)
    .await?;
    let first: Option<String> =
        sqlx::query_scalar("SELECT MIN(day) FROM stat_counts WHERE log != 'downloads'").fetch_one(control).await?;
    let mut days = Vec::new();
    if let Some(first) = first {
        for back in 0..30 {
            let day = since(back);
            if day < first {
                break;
            }
            let counts = DAY_COLUMNS
                .iter()
                .map(|(log, measure, _)| {
                    daily.iter().find(|d| d.0 == day && d.1 == *log && d.2 == *measure).map_or(0, |d| d.3)
                })
                .collect();
            days.push(DayRow { day, counts });
        }
    }

    let top = |log: &'static str, measure: &'static str| {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT name, SUM(n) AS total FROM stat_counts WHERE log = ? AND measure = ? AND day >= ?
             GROUP BY name ORDER BY total DESC, name LIMIT 10",
        )
        .bind(log)
        .bind(measure)
        .bind(month.clone())
        .fetch_all(control)
    };
    let mut sources = Vec::new();
    for log in Log::ALL {
        let list: Vec<(String, i64)> = top(log.key(), "source")
            .await?
            .into_iter()
            .map(|(name, n)| (if name.is_empty() { "Direct or unknown".to_string() } else { name }, n))
            .collect();
        if !list.is_empty() {
            sources.push(SourceList { log: log.title(), sources: list });
        }
    }
    let pages = top("demo", "page").await?;
    let kinds = top(DESKTOP, "kind")
        .await?
        .into_iter()
        .map(|(key, n)| (KINDS.iter().find(|k| k.0 == key).map_or("Other", |k| k.1).to_string(), n))
        .collect();
    let versions = top(DESKTOP, "version").await?;

    let mut downloads = Vec::new();
    for (key, platform) in PLATFORMS {
        let latest: Option<i64> = sqlx::query_scalar(
            "SELECT n FROM stat_counts WHERE log = 'downloads' AND measure = ? ORDER BY day DESC LIMIT 1",
        )
        .bind(key)
        .fetch_optional(control)
        .await?;
        let Some(total) = latest else { continue };
        let before: Option<i64> = sqlx::query_scalar(
            "SELECT n FROM stat_counts WHERE log = 'downloads' AND measure = ? AND day <= ? ORDER BY day DESC LIMIT 1",
        )
        .bind(key)
        .bind(since(7))
        .fetch_optional(control)
        .await?;
        downloads.push(Download { platform, total, week: before.map(|b| total - b) });
    }

    let (people, verified, new_week): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(email_verified_at IS NOT NULL), 0), COALESCE(SUM(created_at >= ?), 0)
         FROM people WHERE is_operator = 0",
    )
    .bind(&week)
    .fetch_one(control)
    .await?;
    let pacs: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM pacs GROUP BY status").fetch_all(control).await?;
    let pac_count = |status: &str| pacs.iter().find(|p| p.0 == status).map_or(0, |p| p.1);
    let parents: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT person_id) FROM families").fetch_one(control).await?;
    let accounts = Accounts {
        people,
        verified,
        new_week,
        pacs_active: pac_count("active"),
        pacs_waiting: pac_count("requested"),
        parents,
    };

    let checks: Vec<(String, String, String)> =
        sqlx::query_as("SELECT source, checked_at, error FROM stat_checks").fetch_all(control).await?;
    let mut checks: Vec<Check> = checks
        .into_iter()
        .map(|(source, checked_at, error)| Check {
            source: match source.as_str() {
                "site" => "Website log",
                "demo" => "Demo log",
                "app" => "App log",
                _ => "GitHub downloads",
            },
            checked_at,
            error,
        })
        .collect();
    checks.sort_by_key(|c| c.source);

    Ok(Report { rows, days, sources, pages, downloads, kinds, versions, accounts, checks })
}

impl Report {
    /// The report as plain text, for the command line.
    pub fn text(&self) -> anyhow::Result<String> {
        let mut out = String::new();
        writeln!(out, "Visits. A visit is one visitor (an address and a browser) on one day, in UTC.")?;
        writeln!(
            out,
            "Bots and the operator are left out. A computer that opens the desktop app counts once a day.\n"
        )?;
        writeln!(out, "{:<13}{:<30}{:>8}{:>8}{:>8}{:>8}", "", "", "Today", "7 days", "30 days", "All")?;
        for r in &self.rows {
            let log = if r.first { r.log } else { "" };
            writeln!(out, "{log:<13}{:<30}{:>8}{:>8}{:>8}{:>8}", r.label, r.today, r.week, r.month, r.all)?;
        }
        let list = |items: &[(String, i64)]| {
            items.iter().map(|(name, n)| format!("{name} {n}")).collect::<Vec<_>>().join(", ")
        };
        if !self.sources.is_empty() {
            writeln!(out, "\nWhere visits came from, last 30 days:")?;
            for s in &self.sources {
                writeln!(out, "  {}: {}", s.log, list(&s.sources))?;
            }
        }
        if !self.pages.is_empty() {
            writeln!(out, "\nDemo pages, last 30 days: {}", list(&self.pages))?;
        }
        if !self.downloads.is_empty() {
            let downloads: Vec<String> = self
                .downloads
                .iter()
                .map(|d| match d.week {
                    Some(week) => format!("{} {} ({week} in 7 days)", d.platform, d.total),
                    None => format!("{} {}", d.platform, d.total),
                })
                .collect();
            writeln!(out, "\nDownloads: {}", downloads.join(", "))?;
        }
        if !self.kinds.is_empty() {
            writeln!(
                out,
                "\nDesktop app computers, last 30 days: {}. Versions: {}",
                list(&self.kinds),
                list(&self.versions)
            )?;
        }
        let a = &self.accounts;
        writeln!(
            out,
            "\nAccounts: {} people ({} with a confirmed email, {} new in 7 days). PACs: {} active, {} waiting. Parents: {}.",
            a.people, a.verified, a.new_week, a.pacs_active, a.pacs_waiting, a.parents
        )?;
        for c in &self.checks {
            match c.error.as_str() {
                "" => writeln!(out, "{} read at {} UTC.", c.source, c.checked_at)?,
                error => writeln!(out, "{} failed at {} UTC: {error}", c.source, c.checked_at)?,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";
    const PHONE: &str =
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148";

    fn line(addr: &str, time: &str, request: &str, status: u16, referrer: &str, agent: &str) -> String {
        format!("{addr} - - [{time} +0000] \"{request} HTTP/2.0\" {status} 100 \"{referrer}\" \"{agent}\"\n")
    }

    async fn control() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations_control").run(&pool).await.unwrap();
        pool
    }

    async fn counts(pool: &SqlitePool, day: &str, log: &str) -> HashMap<String, i64> {
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT measure, name, n FROM stat_counts WHERE day = ? AND log = ?")
                .bind(day)
                .bind(log)
                .fetch_all(pool)
                .await
                .unwrap();
        rows.into_iter().map(|(m, name, n)| (if name.is_empty() { m } else { format!("{m}:{name}") }, n)).collect()
    }

    #[test]
    fn reads_a_combined_log_line() {
        let text =
            line("198.51.100.1", "13/Oct/2026:23:30:00", "GET /?fbclid=x", 200, "https://www.google.com/", CHROME);
        let r = parse(text.trim_end()).unwrap();
        assert_eq!((r.addr, r.method, r.path(), r.query(), r.status), ("198.51.100.1", "GET", "/", "fbclid=x", 200));
        assert_eq!(r.day, NaiveDate::from_ymd_opt(2026, 10, 13).unwrap());
        assert_eq!(r.agent, CHROME);
        // The day is in UTC.
        let late = "a - - [13/Oct/2026:23:30:00 -0800] \"GET / HTTP/1.1\" 200 1 \"-\" \"x\"";
        assert_eq!(parse(late).unwrap().day, NaiveDate::from_ymd_opt(2026, 10, 14).unwrap());
        assert!(parse("not a log line").is_none());
    }

    #[test]
    fn sources_and_pages() {
        assert_eq!(referrer_host("https://m.facebook.com/story"), "facebook.com");
        assert_eq!(referrer_host("https://www.google.com/search?q=pac"), "google.com");
        assert_eq!(referrer_host("android-app://com.google.android.gm/"), "com.google.android.gm");
        assert_eq!(referrer_host("-"), "");
        assert_eq!(referrer_host("https://a<b>.example/"), "");
        assert_eq!(page_name("/transactions/65/edit"), "/transactions/{id}/edit");
        assert!(is_browser(CHROME) && is_browser(PHONE));
        assert!(
            !is_browser("curl/8.7.1")
                && !is_browser("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)")
        );
        assert!(
            valid_repo("sjhewitt/packeeper") && !valid_repo("sjhewitt") && !valid_repo("a/b/c") && !valid_repo("a/b?x")
        );
    }

    #[tokio::test]
    async fn counts_people_and_leaves_out_bots_and_the_operator() {
        let dir = std::env::temp_dir().join(format!("pac-stats-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for log in ["site", "demo", "app"] {
            std::fs::create_dir_all(dir.join(log)).unwrap();
        }
        let day1 = "12/Oct/2026:10:00:00";
        let site = [
            line("198.51.100.1", day1, "GET /", 200, "https://www.google.com/", CHROME),
            line("198.51.100.1", day1, "GET /img/hero.png", 200, "https://packeeper.ca/", CHROME),
            line("198.51.100.1", "12/Oct/2026:11:00:00", "GET /", 304, "-", CHROME),
            line("198.51.100.2", day1, "GET /?fbclid=abc", 200, "-", PHONE),
            line("203.0.113.5", day1, "GET /", 200, "-", "curl/8.7.1"),
            line("203.0.113.6", day1, "GET /wp-login.php", 404, "-", CHROME),
            line("203.0.113.6", day1, "GET /", 200, "-", CHROME),
            line("203.0.113.7", day1, "GET /a", 404, "-", CHROME),
            line("203.0.113.7", day1, "GET /b", 404, "-", CHROME),
            line("203.0.113.7", day1, "GET /c", 404, "-", CHROME),
            line("203.0.113.7", day1, "GET /", 200, "-", CHROME),
            line("192.0.2.10", day1, "GET /", 200, "-", CHROME),
            // A phone that changed its address during the visit.
            line("198.51.100.8", day1, "GET /", 200, "https://packeeper.ca/", PHONE),
            line("198.51.100.1", "13/Oct/2026:09:00:00", "GET /", 200, "-", CHROME),
        ];
        let demo = [
            line("198.51.100.1", day1, "GET /", 303, "https://packeeper.ca/", CHROME),
            line("198.51.100.1", day1, "GET /login", 200, "https://packeeper.ca/", CHROME),
            line("198.51.100.1", day1, "POST /login", 303, "https://demo.packeeper.ca/login", CHROME),
            line("198.51.100.1", day1, "GET /", 200, "https://demo.packeeper.ca/login", CHROME),
            line("198.51.100.1", day1, "GET /transactions/65", 200, "https://demo.packeeper.ca/", CHROME),
            line("198.51.100.3", day1, "GET /login", 200, "-", CHROME),
            line("198.51.100.3", day1, "POST /login", 200, "-", CHROME),
        ];
        let app = [
            line("192.0.2.10", day1, "GET /operator", 200, "-", CHROME),
            line("198.51.100.4", day1, "GET /signup", 200, "https://packeeper.ca/", CHROME),
            line("198.51.100.4", day1, "GET /verify/SECRETTOKEN", 200, "-", CHROME),
        ];
        std::fs::write(dir.join("site/access.log"), site.concat()).unwrap();
        std::fs::write(dir.join("demo/access.log"), demo.concat()).unwrap();
        std::fs::write(dir.join("app/access.log"), app.concat()).unwrap();
        let mut sources = Sources {
            logs: vec![
                (Log::Site, dir.join("site/access.log")),
                (Log::Demo, dir.join("demo/access.log")),
                (Log::App, dir.join("app/access.log")),
            ],
            github: None,
        };
        let pool = control().await;
        collect(&pool, &sources).await.unwrap();

        let expect = |pairs: &[(&str, i64)]| pairs.iter().map(|(k, n)| (k.to_string(), *n)).collect::<HashMap<_, _>>();
        assert_eq!(
            counts(&pool, "2026-10-12", "site").await,
            expect(&[
                ("visits", 3),
                ("views", 4),
                ("phones", 2),
                ("source:google.com", 1),
                ("source:facebook.com", 1),
                ("source", 1),
            ])
        );
        assert_eq!(counts(&pool, "2026-10-13", "site").await, expect(&[("visits", 1), ("views", 1), ("source", 1)]));
        assert_eq!(
            counts(&pool, "2026-10-12", "demo").await,
            expect(&[
                ("visits", 2),
                ("views", 4),
                ("signins", 1),
                ("inside", 1),
                ("source:packeeper.ca", 1),
                ("source", 1),
                ("page:/login", 2),
                ("page:/", 1),
                ("page:/transactions/{id}", 1),
            ])
        );
        assert_eq!(
            counts(&pool, "2026-10-12", "app").await,
            expect(&[("visits", 1), ("views", 2), ("signup_views", 1), ("source:packeeper.ca", 1)])
        );
        // Nothing that could name a visitor is saved.
        let saved: Vec<String> =
            sqlx::query_scalar("SELECT day || log || measure || name FROM stat_counts").fetch_all(&pool).await.unwrap();
        assert!(saved.iter().all(|s| !s.contains("198.51") && !s.contains("SECRET") && !s.contains("Mozilla")));

        // The log rotates late on the 12th. The 12th is there in part, so its
        // counts stay; the 13th is there in full, so its counts are replaced.
        std::fs::write(
            dir.join("site/access.log"),
            [
                line("198.51.100.9", "12/Oct/2026:23:50:00", "GET /", 200, "-", CHROME),
                line("203.0.113.5", "13/Oct/2026:09:00:00", "GET /", 200, "-", "curl/8.7.1"),
            ]
            .concat(),
        )
        .unwrap();
        collect(&pool, &sources).await.unwrap();
        assert_eq!(counts(&pool, "2026-10-12", "site").await["visits"], 3);
        assert!(counts(&pool, "2026-10-13", "site").await.is_empty());

        // A missing log is noted, and the other logs still count.
        sources.logs.push((Log::Site, dir.join("nowhere/access.log")));
        collect(&pool, &sources).await.unwrap();
        let report = report(&pool).await.unwrap();
        assert!(report.checks.iter().any(|c| c.error.contains("cannot find")));
        assert_eq!(report.rows[0].all, 3, "website visits, all days");
        assert!(report.text().unwrap().contains("Website"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn counts_desktop_opens() {
        let pool = control().await;
        let open = |version, kind, first_today, new| Open { version, kind, first_today, new };
        for o in [
            open("0.5.2", "mac", true, true),
            open("0.5.2", "mac", false, false),
            open("0.5.1", "windows", true, false),
            // Anyone can send an open: free text does not get saved.
            open("<script>", "linux; DROP", true, false),
        ] {
            record_open(&pool, &o).await.unwrap();
        }
        let today = Utc::now().date_naive().to_string();
        let expect = [
            ("opens", 4),
            ("computers", 3),
            ("new", 1),
            ("kind:mac", 1),
            ("kind:windows", 1),
            ("kind:other", 1),
            ("version:0.5.2", 1),
            ("version:0.5.1", 1),
            ("version:other", 1),
        ];
        assert_eq!(counts(&pool, &today, DESKTOP).await, expect.iter().map(|(k, n)| (k.to_string(), *n)).collect());

        let report = report(&pool).await.unwrap();
        let desktop: Vec<_> =
            report.rows.iter().filter(|r| r.log == "Desktop app").map(|r| (r.label, r.today, r.all)).collect();
        assert_eq!(desktop, [("Opens", 4, 4), ("Computers that opened it", 3, 3), ("New computers", 1, 1)]);
        assert_eq!(report.days[0].counts.last(), Some(&4), "opens today in the table by day");
        assert!(report.kinds.contains(&("Mac".to_string(), 1)) && report.kinds.contains(&("Other".to_string(), 1)));
        assert!(report.versions.contains(&("0.5.2".to_string(), 1)));
        assert!(report.text().unwrap().contains("Desktop app"));
        assert!(valid_version("0.5.2") && valid_version("1.0") && !valid_version("1") && !valid_version("1.x.0"));
    }

    #[test]
    fn download_totals_by_platform() {
        let releases = serde_json::json!([
            {"assets": [
                {"name": "PAC.Keeper_0.1.1_universal.dmg", "download_count": 4},
                {"name": "PAC.Keeper_0.1.1_x64-setup.exe", "download_count": 3},
                {"name": "pac-keeper-0.1.1-x86_64-linux.tar.gz", "download_count": 2},
                {"name": "notes.txt", "download_count": 9}
            ]},
            {"assets": [{"name": "PAC.Keeper_0.1.0_universal.dmg", "download_count": 1}]}
        ]);
        assert_eq!(download_totals(&releases).unwrap(), vec![("mac", 5), ("windows", 3), ("linux", 2)]);
        assert!(download_totals(&serde_json::json!({"message": "Not Found"})).is_err());
    }
}
