//! Picking one version per registry package so that every requirement
//! holds: a depth-first search over the packages in the order they are
//! first required, trying the versions a package offers in its order (a
//! locked one first, then newest first) and backing up when a choice
//! leaves some package with no version. A failure explains a conflict:
//! the requirements that cannot all hold, how their requirers came into
//! the build, and which requirement to change.

use crate::manifest;
use crate::semver::{Req, Version};
use nabic_core::HashMap;
use std::rc::Rc;

/// Choices tried before giving up: a graph this hard is better pinned.
const LIMIT: usize = 100_000;

/// A requirement on a package.
#[derive(Clone, Debug)]
pub struct Need {
    /// The package's name.
    pub name: String,
    /// The index it comes from.
    pub url: String,
    pub req: Req,
    /// The requirement as written.
    pub text: String,
}

/// A version of a package and what it requires.
#[derive(Clone, Debug)]
pub struct Cand {
    pub name: String,
    pub version: Version,
    pub needs: Vec<Need>,
}

/// Who requires something: a root (its own words, "the root package"), or
/// a version of a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum By {
    Root(String),
    Pkg(String, Version),
}

impl std::fmt::Display for By {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root(r) => f.write_str(r),
            Self::Pkg(n, v) => write!(f, "`{n}` {v}"),
        }
    }
}

/// A package picked: its name, index and version, and the requirements
/// on it.
pub struct Pick {
    pub name: String,
    pub url: String,
    pub version: Version,
    pub needs: Vec<(Need, By)>,
}

/// Why no set of versions was found.
pub enum Failure {
    /// The versions of a package cannot be had (an index error).
    Fatal(String),
    Conflict(Conflict),
}

/// Requirements on one package that no single version meets.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub name: String,
    pub needs: Vec<(Need, By)>,
    /// Each requirer's way into the build, root first.
    pub chains: Vec<String>,
    /// Requirers that had other versions to try.
    pub retried: Vec<String>,
    /// No version at all meets every one of `needs`.
    hard: bool,
}

/// The versions of package `name` in index `url`, in the order to try
/// them (empty: no such package).
pub type Fetch<'a> = dyn FnMut(&str, &str) -> Result<Vec<Cand>, String> + 'a;

pub struct Solver<'a> {
    fetch: &'a mut Fetch<'a>,
    cands: HashMap<(String, String), Rc<Vec<Cand>>>,
    /// Active requirements: (package key, requirement, requirer).
    needs: Vec<(String, Need, By)>,
    /// Package key -> (index, version).
    chosen: HashMap<String, (String, Version)>,
    picked: Vec<String>,
    first: Option<Conflict>,
    steps: usize,
}

impl<'a> Solver<'a> {
    pub fn new(fetch: &'a mut Fetch<'a>) -> Self {
        Self {
            fetch,
            cands: HashMap::default(),
            needs: Vec::new(),
            chosen: HashMap::default(),
            picked: Vec::new(),
            first: None,
            steps: 0,
        }
    }

    /// The versions of `name` in `url`, fetched once.
    pub fn candidates(&mut self, url: &str, name: &str) -> Result<Rc<Vec<Cand>>, String> {
        let k = (url.to_owned(), manifest::key(name));
        if let Some(c) = self.cands.get(&k) {
            return Ok(c.clone());
        }
        let c = Rc::new((self.fetch)(url, name)?);
        self.cands.insert(k, c.clone());
        Ok(c)
    }

    /// One version for every package `roots` reach, in the order picked.
    pub fn solve(&mut self, roots: Vec<(Need, By)>) -> Result<Vec<Pick>, Failure> {
        for (n, by) in roots {
            self.needs.push((manifest::key(&n.name), n, by));
        }
        match self.step() {
            Ok(true) => {}
            Ok(false) => {
                let c = self.first.take().ok_or_else(|| {
                    Failure::Fatal("no set of versions meets every requirement".to_owned())
                })?;
                return Err(Failure::Conflict(c));
            }
            Err(e) => return Err(Failure::Fatal(e)),
        }
        let picked = std::mem::take(&mut self.picked);
        Ok(picked
            .into_iter()
            .map(|k| {
                let (url, version) = self.chosen[&k].clone();
                let needs: Vec<(Need, By)> = self
                    .needs
                    .iter()
                    .filter(|(x, _, _)| *x == k)
                    .map(|(_, n, b)| (n.clone(), b.clone()))
                    .collect();
                let name = self
                    .cands
                    .get(&(url.clone(), k))
                    .and_then(|c| c.iter().find(|c| c.version == version))
                    .map_or_else(|| needs[0].0.name.clone(), |c| c.name.clone());
                Pick {
                    name,
                    url,
                    version,
                    needs,
                }
            })
            .collect())
    }

    fn on(&self, key: &str) -> Vec<(Need, By)> {
        self.needs
            .iter()
            .filter(|(k, _, _)| k == key)
            .map(|(_, n, b)| (n.clone(), b.clone()))
            .collect()
    }

    /// Pick the next package; whether the rest of the graph then fits.
    fn step(&mut self) -> Result<bool, String> {
        let Some((key, first)) = self
            .needs
            .iter()
            .find(|(k, _, _)| !self.chosen.contains_key(k))
            .map(|(k, n, _)| (k.clone(), n.clone()))
        else {
            return Ok(true);
        };
        let on = self.on(&key);
        if on.iter().any(|(n, _)| n.url != first.url) {
            self.conflict(&key)?;
            return Ok(false);
        }
        let all = self.candidates(&first.url, &first.name)?;
        let mut any = false;
        for c in all.iter() {
            if !on.iter().all(|(n, _)| n.req.matches(&c.version)) {
                continue;
            }
            any = true;
            self.steps += 1;
            if self.steps > LIMIT {
                return Err(format!(
                    "gave up after trying {LIMIT} combinations of versions; pin the versions of \
                     the main dependencies in nabi.nabion (e.g. `\"{}\": \"={}\"`) to narrow the \
                     search",
                    first.name, c.version
                ));
            }
            let mark = self.needs.len();
            self.chosen
                .insert(key.clone(), (first.url.clone(), c.version.clone()));
            self.picked.push(key.clone());
            let by = By::Pkg(c.name.clone(), c.version.clone());
            let mut ok = true;
            for n in &c.needs {
                let k = manifest::key(&n.name);
                self.needs.push((k.clone(), n.clone(), by.clone()));
                if let Some((u, v)) = self.chosen.get(&k).cloned() {
                    if u != n.url || !n.req.matches(&v) {
                        self.conflict(&k)?;
                        ok = false;
                        break;
                    }
                }
            }
            if ok && self.step()? {
                return Ok(true);
            }
            self.needs.truncate(mark);
            self.chosen.remove(&key);
            self.picked.pop();
        }
        if !any {
            self.conflict(&key)?;
        }
        Ok(false)
    }

    /// Keep the first conflict on `key`, and later the first that no
    /// version could solve.
    fn conflict(&mut self, key: &str) -> Result<(), String> {
        if self.first.as_ref().is_some_and(|c| c.hard) {
            return Ok(());
        }
        let needs = self.on(key);
        let urls_agree = needs.iter().all(|(n, _)| n.url == needs[0].0.url);
        let hard = !urls_agree || {
            let all = self.candidates(&needs[0].0.url, &needs[0].0.name)?;
            !all.iter()
                .any(|c| needs.iter().all(|(n, _)| n.req.matches(&c.version)))
        };
        if self.first.is_some() && !hard {
            return Ok(());
        }
        let mut chains = Vec::new();
        let mut retried = Vec::new();
        for (_, by) in &needs {
            if let By::Pkg(n, _) = by {
                self.chain(n, &mut chains, 0);
                let k = manifest::key(n);
                let on = self.on(&k);
                if let Some((u, _)) = self.chosen.get(&k).cloned() {
                    let all = self.candidates(&u, n)?;
                    let fits = all
                        .iter()
                        .filter(|c| on.iter().all(|(x, _)| x.req.matches(&c.version)))
                        .count();
                    if fits > 1 && !retried.contains(n) {
                        retried.push(n.clone());
                    }
                }
            }
        }
        self.first = Some(Conflict {
            name: needs[0].0.name.clone(),
            needs,
            chains,
            retried,
            hard,
        });
        Ok(())
    }

    /// How package `name` came in, root first, into `out` (once each).
    fn chain(&self, name: &str, out: &mut Vec<String>, depth: usize) {
        let k = manifest::key(name);
        let Some((_, v)) = self.chosen.get(&k) else {
            return;
        };
        let Some((_, n, by)) = self.needs.iter().find(|(x, _, _)| *x == k) else {
            return;
        };
        if let By::Pkg(p, _) = by {
            if depth < 32 {
                self.chain(p, out, depth + 1);
            }
        }
        let line = format!("`{name}` {v}: {by} requires `{}` `{}`", n.name, n.text);
        if !out.contains(&line) {
            out.push(line);
        }
    }

    /// The error for `c`: what conflicts, how it came in, what to change.
    pub fn explain(&mut self, c: &Conflict) -> String {
        let name = &c.name;
        let mut s = format!("no version of package `{name}` meets every requirement on it:");
        for (n, by) in &c.needs {
            s += &format!("\n  `{}` (required by {by}", n.text);
            if c.needs.iter().any(|(m, _)| m.url != n.url) {
                s += &format!(", from {}", n.url);
            }
            s.push(')');
        }
        let url = &c.needs[0].0.url;
        let all = self.candidates(url, name).unwrap_or_default();
        if c.hard {
            let mut vs: Vec<&Version> = all.iter().map(|c| &c.version).collect();
            vs.sort();
            vs.dedup();
            let vs: Vec<String> = vs.iter().map(ToString::to_string).collect();
            s += &format!(
                "\n{url} has {name} {}; none of them meets all of these",
                vs.join(", ")
            );
        }
        if !c.chains.is_empty() {
            s += "\nhow those packages came in:";
            for l in &c.chains {
                s += &format!("\n  {l}");
            }
        }
        if !c.retried.is_empty() {
            let r: Vec<String> = c.retried.iter().map(|n| format!("`{n}`")).collect();
            s += &format!(
                "\nthe other versions of {} that fit were tried too",
                r.join(", ")
            );
        }
        s += "\none build has one version of a package";
        let help = self.help(c, &all);
        s += &format!("\nhelp: {help}");
        s
    }

    fn help(&mut self, c: &Conflict, all: &[Cand]) -> String {
        let name = &c.name;
        let meets =
            |others: &[&(Need, By)], v: &Version| others.iter().all(|(n, _)| n.req.matches(v));
        for (i, (_, by)) in c.needs.iter().enumerate() {
            let others: Vec<&(Need, By)> = c
                .needs
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, x)| x)
                .collect();
            match by {
                By::Root(root) => {
                    let best = all
                        .iter()
                        .filter(|x| meets(&others, &x.version))
                        .max_by(|a, b| a.version.cmp(&b.version));
                    if let Some(b) = best {
                        return format!(
                            "{name} {v} meets the other requirements: change {root}'s to it, \
                             `\"{name}\": \"^{v}\"`",
                            v = b.version
                        );
                    }
                }
                By::Pkg(r, rv) => {
                    // A version of the requirer that gets along with the rest.
                    let Some((rn, root)) = self
                        .needs
                        .iter()
                        .find(|(k, _, b)| *k == manifest::key(r) && matches!(b, By::Root(_)))
                        .map(|(_, n, b)| (n.clone(), b.clone()))
                    else {
                        continue;
                    };
                    let Ok(rs) = self.candidates(&rn.url, r) else {
                        continue;
                    };
                    let key = manifest::key(name);
                    let ok = |x: &Cand| {
                        x.version != *rv
                            && x.needs
                                .iter()
                                .filter(|n| manifest::key(&n.name) == key)
                                .all(|n| {
                                    all.iter().any(|b| {
                                        n.req.matches(&b.version) && meets(&others, &b.version)
                                    })
                                })
                    };
                    let mut fits: Vec<&Cand> = rs.iter().filter(|x| ok(x)).collect();
                    fits.sort_by(|a, b| b.version.cmp(&a.version));
                    if let Some(x) = fits.first() {
                        return format!(
                            "`{r}` {v} gets along with the other requirements on `{name}`: \
                             change {root}'s requirement on it, `\"{r}\": \"^{v}\"`",
                            v = x.version
                        );
                    }
                }
            }
        }
        format!(
            "relax one of these requirements so they overlap, or pin `{name}` in the root \
             package at a version all of them accept"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index: `(name, version, [(dep, req)])`.
    type Index<'a> = &'a [(&'a str, &'a str, &'a [(&'a str, &'a str)])];

    fn need(name: &str, req: &str) -> Need {
        Need {
            name: name.to_owned(),
            url: "ix".to_owned(),
            req: Req::parse(req).unwrap(),
            text: req.to_owned(),
        }
    }

    fn run(index: Index<'_>, roots: &[(&str, &str)]) -> Result<Vec<String>, String> {
        let mut fetch = |_: &str, name: &str| -> Result<Vec<Cand>, String> {
            let mut v: Vec<Cand> = index
                .iter()
                .filter(|(n, _, _)| *n == name)
                .map(|(n, v, d)| Cand {
                    name: (*n).to_owned(),
                    version: Version::parse(v).unwrap(),
                    needs: d.iter().map(|(n, r)| need(n, r)).collect(),
                })
                .collect();
            v.sort_by(|a, b| b.version.cmp(&a.version));
            Ok(v)
        };
        let mut s = Solver::new(&mut fetch);
        let roots: Vec<(Need, By)> = roots
            .iter()
            .map(|(n, r)| (need(n, r), By::Root("the root package".to_owned())))
            .collect();
        match s.solve(roots) {
            Ok(p) => {
                let mut out: Vec<String> = p
                    .iter()
                    .map(|p| format!("{} {}", p.name, p.version))
                    .collect();
                out.sort();
                Ok(out)
            }
            Err(Failure::Fatal(e)) => Err(e),
            Err(Failure::Conflict(c)) => Err(s.explain(&c)),
        }
    }

    #[test]
    fn intersects_requirements() {
        // `>=0.1` alone picks 0.3.0; `^0.1` from `b` then needs 0.1.9.
        let ix: Index<'_> = &[
            ("x", "0.1.0", &[]),
            ("x", "0.1.9", &[]),
            ("x", "0.3.0", &[]),
            ("a", "1.0.0", &[("x", ">=0.1")]),
            ("b", "1.0.0", &[("x", "^0.1")]),
        ];
        let got = run(ix, &[("a", "^1"), ("b", "^1")]).unwrap();
        assert_eq!(got, ["a 1.0.0", "b 1.0.0", "x 0.1.9"]);
    }

    #[test]
    fn backs_up_to_an_older_requirer() {
        // a 1.2 needs c ^2, which d rules out; a 1.1 needs c ^1.
        let ix: Index<'_> = &[
            ("a", "1.1.0", &[("c", "^1")]),
            ("a", "1.2.0", &[("c", "^2")]),
            ("c", "1.4.0", &[]),
            ("c", "2.0.0", &[]),
            ("d", "0.5.0", &[("c", "^1.2")]),
        ];
        let got = run(ix, &[("a", "^1"), ("d", "^0.5")]).unwrap();
        assert_eq!(got, ["a 1.1.0", "c 1.4.0", "d 0.5.0"]);
    }

    #[test]
    fn explains_a_conflict() {
        let ix: Index<'_> = &[
            ("a", "1.4.0", &[("b", "^2")]),
            ("c", "0.3.0", &[("b", "^1")]),
            ("c", "0.4.0", &[("b", "^2")]),
            ("b", "1.2.0", &[]),
            ("b", "2.0.1", &[]),
        ];
        let e = run(ix, &[("a", "^1.0"), ("c", "~0.3")]).unwrap_err();
        assert!(
            e.starts_with("no version of package `b` meets every requirement on it:"),
            "{e}"
        );
        assert!(e.contains("`^2` (required by `a` 1.4.0)"), "{e}");
        assert!(e.contains("`^1` (required by `c` 0.3.0)"), "{e}");
        assert!(
            e.contains("ix has b 1.2.0, 2.0.1; none of them meets all"),
            "{e}"
        );
        assert!(
            e.contains("`a` 1.4.0: the root package requires `a` `^1.0`"),
            "{e}"
        );
        assert!(
            e.contains("help: `c` 0.4.0 gets along with the other requirements on `b`"),
            "{e}"
        );
        assert!(e.contains("`\"c\": \"^0.4.0\"`"), "{e}");
    }

    #[test]
    fn root_requirement_help() {
        let ix: Index<'_> = &[
            ("a", "1.0.0", &[("b", "^2")]),
            ("b", "1.0.0", &[]),
            ("b", "2.3.0", &[]),
        ];
        let e = run(ix, &[("a", "^1"), ("b", "^1")]).unwrap_err();
        assert!(
            e.contains("help: b 2.3.0 meets the other requirements: change the root package's"),
            "{e}"
        );
    }
}
