//! `nabi audit`: the locked registry packages against their registry's
//! security advisories, OSV records from `GET /api/v1/advisories`.
//!
//! One request per index fetches the whole list, kept an hour in
//! `$NABI_HOME/cache/advisories/`; `--offline` reads only that. Exit 0:
//! none affected, 1: some are, 2: not checked.

#[allow(clippy::wildcard_imports)]
use crate::*;
use semver::Version;
use std::time::Duration;

/// How long a fetched advisory list stays fresh.
const FRESH: Duration = Duration::from_secs(3600);

/// Every advisory, newest first, withdrawn ones included.
const LIST: &str = "/api/v1/advisories?format=json";

/// One event of an OSV range.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Introduced(String),
    Fixed(String),
    LastAffected(String),
}

/// An OSV `affected` entry of the Nabi ecosystem.
struct Affected {
    name: String,
    /// The events of each SEMVER (or ECOSYSTEM) range.
    ranges: Vec<Vec<Event>>,
    versions: Vec<String>,
    severity: Option<String>,
}

struct Advisory {
    id: String,
    summary: String,
    aliases: Vec<String>,
    severity: Option<String>,
    withdrawn: bool,
    affected: Vec<Affected>,
}

/// What an advisory says of one locked version.
#[derive(Debug, PartialEq, Eq)]
struct Hit {
    /// The lowest version past every fix it needs.
    fixed: Option<String>,
    /// The last affected version, when a range names one and no fix.
    last: Option<String>,
    severity: Option<String>,
}

fn items(j: &Json) -> &[Json] {
    match j {
        Json::Arr(a) => a,
        _ => &[],
    }
}

fn strings(j: &Json) -> Vec<String> {
    items(j)
        .iter()
        .filter_map(Json::as_str)
        .map(str::to_owned)
        .collect()
}

/// `database_specific.severity` (or `ecosystem_specific`), else the
/// first `severity` score.
fn severity(j: &Json) -> Option<String> {
    ["database_specific", "ecosystem_specific"]
        .iter()
        .find_map(|k| j.get(k).get("severity").as_str())
        .or_else(|| items(j.get("severity")).first()?.get("score").as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn affected(a: &Json) -> Option<Affected> {
    let p = a.get("package");
    if p.get("ecosystem").as_str()? != "Nabi" {
        return None;
    }
    let ranges = items(a.get("ranges"))
        .iter()
        .filter(|r| matches!(r.get("type").as_str(), Some("SEMVER" | "ECOSYSTEM")))
        .map(|r| {
            items(r.get("events"))
                .iter()
                .filter_map(|e| {
                    let s = |k: &str| e.get(k).as_str().map(str::to_owned);
                    s("introduced")
                        .map(Event::Introduced)
                        .or_else(|| s("fixed").map(Event::Fixed))
                        .or_else(|| s("last_affected").map(Event::LastAffected))
                })
                .collect()
        })
        .collect();
    Some(Affected {
        name: p.get("name").as_str()?.to_owned(),
        ranges,
        versions: strings(a.get("versions")),
        severity: severity(a),
    })
}

fn advisory(j: &Json) -> Option<Advisory> {
    Some(Advisory {
        id: j.get("id").as_str()?.to_owned(),
        summary: j.get("summary").as_str().unwrap_or_default().to_owned(),
        aliases: strings(j.get("aliases")),
        severity: severity(j),
        withdrawn: !matches!(j.get("withdrawn"), Json::Null),
        affected: items(j.get("affected"))
            .iter()
            .filter_map(affected)
            .collect(),
    })
}

/// The records of `{"advisories": [..]}`; ones without an id are skipped.
fn parse(text: &str) -> Result<Vec<Advisory>, String> {
    let doc = Json::parse(text)?;
    match doc.get("advisories") {
        Json::Arr(list) => Ok(list.iter().filter_map(advisory).collect()),
        _ => Err("the reply has no `advisories` list".into()),
    }
}

/// `bound` <= `v`; `0` is below every version.
fn at_or_below(bound: &str, v: &Version) -> bool {
    bound == "0" || Version::parse(bound).is_ok_and(|b| b <= *v)
}

/// Whether range `events`, read in order, cover `v`.
fn covers(events: &[Event], v: &Version) -> bool {
    let mut on = false;
    for e in events {
        match e {
            Event::Introduced(b) if at_or_below(b, v) => on = true,
            Event::Fixed(b) if at_or_below(b, v) => on = false,
            Event::LastAffected(b) if Version::parse(b).is_ok_and(|b| b < *v) => on = false,
            _ => {}
        }
    }
    on
}

impl Advisory {
    /// Whether it names `name` at `version`, and the way out.
    fn hit(&self, name: &str, version: &str) -> Option<Hit> {
        if self.withdrawn {
            return None;
        }
        let v = Version::parse(version).ok();
        let (mut any, mut severity) = (false, None);
        // Per covering range its lowest fix above `v`; `None`: no fix.
        let mut fixes: Vec<Option<Version>> = Vec::new();
        let mut last = None;
        for a in self
            .affected
            .iter()
            .filter(|a| manifest::same(&a.name, name))
        {
            let listed = a
                .versions
                .iter()
                .any(|x| x == version || v.is_some() && Version::parse(x).ok() == v);
            let mut hit = listed;
            if let Some(v) = &v {
                for r in a.ranges.iter().filter(|r| covers(r, v)) {
                    hit = true;
                    let fix = r
                        .iter()
                        .filter_map(|e| match e {
                            Event::Fixed(b) => Version::parse(b).ok().filter(|b| b > v),
                            _ => None,
                        })
                        .min();
                    last = last.or_else(|| {
                        r.iter().find_map(|e| match e {
                            Event::LastAffected(b) => Some(b.clone()),
                            _ => None,
                        })
                    });
                    fixes.push(fix);
                }
            }
            if hit {
                any = true;
                severity = severity.or_else(|| a.severity.clone());
            }
        }
        if !any {
            return None;
        }
        let fixed = if fixes.is_empty() || fixes.iter().any(Option::is_none) {
            None
        } else {
            fixes.into_iter().flatten().max().map(|f| f.to_string())
        };
        Some(Hit {
            last: if fixed.is_none() { last } else { None },
            fixed,
            severity: severity.or_else(|| self.severity.clone()),
        })
    }

    /// `--ignore <id>` names it: its id or an alias, any case.
    fn ignored(&self, ignore: &[&str]) -> bool {
        ignore.iter().any(|i| {
            std::iter::once(&self.id)
                .chain(&self.aliases)
                .any(|a| a.eq_ignore_ascii_case(i))
        })
    }
}

/// Where the advisory list of index `url` is kept.
fn cache_file(home: &Path, url: &str) -> PathBuf {
    let key = nabic_core::hash::hex(&nabic_core::hash::sha256(url.as_bytes()));
    home.join("cache/advisories")
        .join(format!("{}.json", &key[..16]))
}

fn save(file: &Path, text: &str) -> Result<(), String> {
    let dir = file.parent().unwrap_or(file);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, file))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// The registry API of index `url`: from the cached index, else fetched.
fn api_of(home: &Path, url: &str, quiet: bool) -> Result<String, String> {
    let index = git::Cache::index(home, registry::sources(home)?);
    match registry::api_cached(&index, url) {
        Some(a) => Ok(a),
        None => registry::api(home, url, quiet),
    }
}

/// The advisories of index `url`: cached while fresh (`offline`: at any
/// age), else fetched; a failed fetch falls back to an old copy.
fn advisories(home: &Path, url: &str, offline: bool, quiet: bool) -> Result<Vec<Advisory>, String> {
    let file = cache_file(home, url);
    let have = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| Some((std::fs::read_to_string(&file).ok()?, t.elapsed().ok()?)));
    let bad = |e: String| {
        format!(
            "{}: {e}; delete it, then run `nabi audit` online",
            file.display()
        )
    };
    if let Some((text, age)) = &have {
        if offline || *age < FRESH {
            return parse(text).map_err(bad);
        }
    }
    if offline {
        return Err(format!(
            "no advisories of {url} are cached, and --offline is set: run `nabi audit` online once"
        ));
    }
    let fetched = api_of(home, url, quiet).and_then(|api| {
        if !quiet {
            term::status("Fetching", &format!("advisories from {api}"));
        }
        let text = registry::get(&api, LIST)?;
        let list = parse(&text).map_err(|e| format!("{api}{LIST}: {e}"))?;
        Ok((text, list))
    });
    match (fetched, have) {
        (Ok((text, list)), _) => {
            if let Err(e) = save(&file, &text) {
                warn_once(&e);
            }
            Ok(list)
        }
        (Err(e), Some((text, age))) => {
            warn_once(&format!(
                "{e}; using the advisories cached {} h ago",
                age.as_secs() / 3600
            ));
            parse(&text).map_err(bad)
        }
        (Err(e), None) => Err(format!(
            "{e}\n(check the connection and run `nabi audit` again)"
        )),
    }
}

/// A locked package an advisory names.
struct Finding<'a> {
    name: &'a str,
    version: &'a str,
    advisory: &'a Advisory,
    hit: Hit,
}

/// `n thing` / `n things`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `nabi audit [--json] [--ignore <id>].. [--offline]`.
pub(crate) fn cmd_audit(g: &Global, args: &[String]) -> Result<i32, String> {
    let (vals, sw, pos) = flags(args, &["--ignore", "--format"], &["--json"])?;
    if let Some(p) = pos.first() {
        return Err(format!("unexpected argument `{p}`"));
    }
    let json = sw.iter().any(|s| s == "--json") || json_format(args, "audit")?;
    let ignore: Vec<&str> = vals
        .iter()
        .filter(|(k, _)| k == "--ignore")
        .map(|(_, v)| v.as_str())
        .collect();
    Ok(audit(g, &ignore, json).unwrap_or_else(|e| {
        eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Error));
        2
    }))
}

fn audit(g: &Global, ignore: &[&str], json: bool) -> Result<i32, String> {
    if g.env.is_none() && package_dir(Path::new(".")).is_none() {
        return Err(
            "not inside a package (no nabi.nabion here or above): run `nabi audit` in one, or \
             `nabi audit --env <name>` for an environment's libraries"
                .into(),
        );
    }
    let home = home();
    let t = target_for(g, None)?;
    let path = t.ctx.lock_path.as_deref();
    let mut lock = path.map(lock::Lock::read).transpose()?.flatten();
    if let (None, Some(p)) = (&lock, path) {
        let mut o = options(g);
        o.dev = true;
        resolve::resolve(&home, &t.ctx, &o)
            .map_err(|e| format!("{e}\n(there is no nabi.lock yet; `nabi update` makes one)"))?;
        lock = lock::Lock::read(p)?;
    }
    let lock = lock.unwrap_or_default();

    // What no advisory can name: path and git packages, native ones.
    let mut skipped: Vec<(String, &str)> = Vec::new();
    let mut manifests = vec![t.ctx.manifest.clone()];
    let members = t.ctx.workspace.as_ref().map_or(&[][..], |w| &w.members[..]);
    for m in members {
        manifests.extend(manifest::load(&m.dir.join(MANIFEST)).ok());
    }
    for d in manifests
        .iter()
        .flat_map(|m| m.deps.iter().chain(&m.dev_deps))
    {
        let member = members.iter().any(|m| manifest::same(&m.name, &d.name));
        if matches!(d.source, Source::Path(_))
            && !member
            && !skipped.iter().any(|(n, _)| manifest::same(n, &d.name))
        {
            skipped.push((d.name.clone(), "path dependency"));
        }
    }
    let mut by_index: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for p in &lock.packages {
        let Some(v) = &p.version else {
            skipped.push((p.name.clone(), "git dependency"));
            continue;
        };
        match by_index.iter_mut().find(|(u, _)| *u == p.url) {
            Some((_, l)) => l.push((&p.name, v)),
            None => by_index.push((&p.url, vec![(&p.name, v)])),
        }
    }
    for n in &lock.natives {
        skipped.push((n.name.clone(), "native package"));
    }

    let mut lists = Vec::new();
    for (url, _) in &by_index {
        lists.push(advisories(&home, url, g.offline, g.quiet || json)?);
    }
    let (mut found, mut ignored, mut checked) = (Vec::new(), Vec::new(), 0);
    for ((_, pkgs), list) in by_index.iter().zip(&lists) {
        for &(name, version) in pkgs {
            checked += 1;
            for a in list {
                let Some(hit) = a.hit(name, version) else {
                    continue;
                };
                if a.ignored(ignore) {
                    ignored.push(a.id.as_str());
                } else {
                    found.push(Finding {
                        name,
                        version,
                        advisory: a,
                        hit,
                    });
                }
            }
        }
    }
    ignored.dedup();
    let total: usize = lists.iter().map(Vec::len).sum();
    if json {
        report_json(&found, &skipped, checked, total, ignored.len());
    } else {
        report(g, &found, &skipped, checked, &ignored);
    }
    Ok(i32::from(!found.is_empty()))
}

/// What to do about `f`.
fn advice(f: &Finding<'_>) -> String {
    let n = f.name;
    match (&f.hit.fixed, &f.hit.last) {
        (Some(fix), _) => format!(
            "upgrade to {fix} or later: `nabi update {n}` (if a requirement holds it back: `nabi \
             add {n}@^{fix}`)"
        ),
        (None, Some(last)) => format!(
            "versions after {last} are not affected: `nabi update {n}` (if a requirement holds it \
             back, raise it past {last})"
        ),
        (None, None) => format!(
            "no fixed version yet: drop it (`nabi remove {n}`) or, once you judge it safe here, \
             `nabi audit --ignore {}`",
            f.advisory.id
        ),
    }
}

fn report(
    g: &Global,
    found: &[Finding<'_>],
    skipped: &[(String, &str)],
    checked: usize,
    ignored: &[&str],
) {
    let help = term::label(Stream::Stdout, Role::Help);
    for f in found {
        let sev = f
            .hit
            .severity
            .as_deref()
            .map(|s| format!(" (severity {s})"))
            .unwrap_or_default();
        let summary = if f.advisory.summary.is_empty() {
            String::new()
        } else {
            format!(": {}", f.advisory.summary)
        };
        println!(
            "{} {} is affected by {}{sev}{summary}",
            f.name, f.version, f.advisory.id
        );
        println!("  {help} {}", advice(f));
    }
    if g.quiet {
        return;
    }
    for (name, why) in skipped {
        term::note(&format!(
            "{name} not checked: a {why}, and advisories name registry versions"
        ));
    }
    if !ignored.is_empty() {
        term::note(&format!("ignored {}", ignored.join(", ")));
    }
    let packages = count(checked, "locked package", "locked packages");
    if found.is_empty() {
        term::status("Audited", &format!("{packages}: no known advisories"));
    } else {
        eprintln!(
            "nabi: {} {} in {packages}",
            term::label(Stream::Stderr, Role::Error),
            count(found.len(), "advisory applies", "advisories apply")
        );
    }
}

/// One JSON object per line: an `advisory` record per finding, a
/// `skipped` one per package not checked, then a `summary`.
fn report_json(
    found: &[Finding<'_>],
    skipped: &[(String, &str)],
    checked: usize,
    advisories: usize,
    ignored: usize,
) {
    let record = |ty: &str, fields: Vec<(&str, Json)>| {
        let mut all = vec![
            (
                "schema".to_owned(),
                Json::num(nabic_diagnostic::RECORD_SCHEMA),
            ),
            ("type".to_owned(), Json::str(ty)),
        ];
        all.extend(fields.into_iter().map(|(k, v)| (k.to_owned(), v)));
        println!("{}", Json::Obj(all));
    };
    let opt = |s: &Option<String>| s.as_deref().map_or(Json::Null, Json::str);
    let num = |n: usize| Json::num(u32::try_from(n).unwrap_or(u32::MAX));
    for f in found {
        let a = f.advisory;
        record(
            "advisory",
            vec![
                ("name", Json::str(f.name)),
                ("version", Json::str(f.version)),
                ("id", Json::str(&a.id)),
                (
                    "aliases",
                    Json::Arr(a.aliases.iter().map(Json::str).collect()),
                ),
                ("summary", Json::str(&a.summary)),
                ("severity", opt(&f.hit.severity)),
                ("fixed", opt(&f.hit.fixed)),
                ("last_affected", opt(&f.hit.last)),
                ("help", Json::str(advice(f))),
            ],
        );
    }
    for (name, why) in skipped {
        record(
            "skipped",
            vec![("name", Json::str(name)), ("reason", Json::str(*why))],
        );
    }
    record(
        "summary",
        vec![
            ("checked", num(checked)),
            ("affected", num(found.len())),
            ("advisories", num(advisories)),
            ("ignored", num(ignored)),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(affected: &str) -> Advisory {
        let text = format!(
            r#"{{"advisories":[{{"id":"NABI-2026-0001","summary":"s","aliases":["CVE-1"],
            "affected":[{affected}]}}]}}"#
        );
        parse(&text).unwrap().remove(0)
    }

    fn range(events: &str) -> String {
        format!(
            r#"{{"package":{{"ecosystem":"Nabi","name":"Num"}},
            "ranges":[{{"type":"SEMVER","events":[{events}]}}]}}"#
        )
    }

    /// `Some("-")`: affected with no fix.
    fn fixed(a: &Advisory, v: &str) -> Option<String> {
        a.hit("num", v)
            .map(|h| h.fixed.unwrap_or_else(|| "-".into()))
    }

    #[test]
    fn introduced_and_fixed() {
        let a = one(&range(r#"{"introduced":"0"},{"fixed":"1.2.0"}"#));
        assert_eq!(fixed(&a, "0.1.0"), Some("1.2.0".into()));
        assert_eq!(fixed(&a, "1.1.9"), Some("1.2.0".into()));
        assert_eq!(fixed(&a, "1.2.0-rc.1"), Some("1.2.0".into()));
        assert_eq!(fixed(&a, "1.2.0"), None);
        assert_eq!(fixed(&a, "2.0.0"), None);
        // Names fold case and `-` / `_`.
        assert!(a.hit("NUM", "1.0.0").is_some());
        assert!(a.hit("other", "1.0.0").is_none());
    }

    #[test]
    fn reintroduced() {
        let a = one(&range(
            r#"{"introduced":"1.0.0"},{"fixed":"1.1.0"},{"introduced":"2.0.0"},{"fixed":"2.0.3"}"#,
        ));
        assert_eq!(fixed(&a, "0.9.0"), None);
        assert_eq!(fixed(&a, "1.0.5"), Some("1.1.0".into()));
        assert_eq!(fixed(&a, "1.5.0"), None);
        assert_eq!(fixed(&a, "2.0.1"), Some("2.0.3".into()));
        assert_eq!(fixed(&a, "2.0.3"), None);
    }

    #[test]
    fn last_affected() {
        let a = one(&range(
            r#"{"introduced":"1.0.0"},{"last_affected":"1.4.2"}"#,
        ));
        let h = a.hit("num", "1.4.2").unwrap();
        assert_eq!((h.fixed, h.last), (None, Some("1.4.2".into())));
        assert!(a.hit("num", "1.4.3").is_none());
        assert!(a.hit("num", "0.9.9").is_none());
    }

    #[test]
    fn versions_list() {
        let a = one(
            r#"{"package":{"ecosystem":"Nabi","name":"num"},"versions":["0.3.0","0.3.1"],
            "database_specific":{"severity":"HIGH"}}"#,
        );
        let h = a.hit("num", "0.3.1").unwrap();
        assert_eq!((h.fixed, h.severity.as_deref()), (None, Some("HIGH")));
        assert!(a.hit("num", "0.3.2").is_none());
    }

    #[test]
    fn withdrawn_and_other_ecosystems() {
        let text = format!(
            r#"{{"advisories":[{{"id":"A","withdrawn":"2026-11-01T00:00:00Z","affected":[{}]}},
            {{"id":"B","affected":[{{"package":{{"ecosystem":"crates.io","name":"num"}},
            "versions":["1.0.0"]}}]}}]}}"#,
            range(r#"{"introduced":"0"}"#)
        );
        let list = parse(&text).unwrap();
        assert!(list.iter().all(|a| a.hit("num", "1.0.0").is_none()));
    }

    #[test]
    fn every_range_must_be_fixed() {
        // Two covering ranges: the fix is past both; one without a fix
        // leaves none.
        let two = format!(
            "{},{}",
            range(r#"{"introduced":"0"},{"fixed":"1.3.0"}"#),
            range(r#"{"introduced":"1.0.0"},{"fixed":"1.5.0"}"#)
        );
        assert_eq!(fixed(&one(&two), "1.1.0"), Some("1.5.0".into()));
        let open = format!(
            "{},{}",
            range(r#"{"introduced":"0"},{"fixed":"1.3.0"}"#),
            range(r#"{"introduced":"1.0.0"}"#)
        );
        assert_eq!(fixed(&one(&open), "1.1.0"), Some("-".into()));
    }

    #[test]
    fn ignore_by_id_or_alias() {
        let a = one(&range(r#"{"introduced":"0"}"#));
        assert!(a.ignored(&["nabi-2026-0001"]));
        assert!(a.ignored(&["CVE-1"]));
        assert!(!a.ignored(&["NABI-2026-0002"]));
    }
}
