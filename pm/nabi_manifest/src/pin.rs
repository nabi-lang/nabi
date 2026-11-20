//! Toolchain pins (a manifest's `toolchain`) and the toolchains a
//! `nabi_version` admits.

use crate::VERSION;

/// `pin` (`"0.3"`) accepts `version` (`"0.3"`, `"0.3.1"`).
fn matches(pin: &str, version: &str) -> bool {
    version == pin
        || version
            .strip_prefix(pin)
            .is_some_and(|r| r.starts_with('.'))
}

/// What a pin may be, for messages.
pub const PIN_FORMS: &str =
    "write a version (`0.1`), a requirement (`^0.1.0`) or a toolchain directory name";

/// A toolchain pin: a version or a prefix of one (`0.1`,
/// `0.1.5`), a requirement (`^0.1.5`, `~0.1.5`, `=0.1.5`), or a toolchain
/// directory name (`0.1.5-<triple>`), which `find` matches by name.
#[derive(Clone, Debug)]
pub enum ToolchainReq {
    Prefix(String),
    Req(crate::semver::Req),
}

impl ToolchainReq {
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("`{s}` is not a toolchain pin; {PIN_FORMS}");
        if s.starts_with(['^', '~', '=', '<', '>']) {
            return crate::semver::Req::parse(s)
                .map(Self::Req)
                .map_err(|_| bad());
        }
        // `0.1`, `0.1.5`, `0.1.0-rc.1`, `0.1.5-<triple>`.
        let core = s.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        let numeric = (1..=3).contains(&parts.len())
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        let chars = s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
        if numeric && chars {
            Ok(Self::Prefix(s.to_owned()))
        } else {
            Err(bad())
        }
    }

    pub fn matches(&self, version: &str) -> bool {
        match self {
            Self::Prefix(p) => matches(p, version),
            Self::Req(r) => crate::semver::Version::parse(version).is_ok_and(|v| r.matches(&v)),
        }
    }
}

/// `v`'s series: `0.MINOR` while the major version is 0, else `MAJOR`.
pub fn series(v: &str) -> Option<String> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse::<u64>().ok()
    });
    let major = parts.next()??;
    let minor = parts.next();
    if parts.any(|p| p.is_none()) {
        return None;
    }
    match (major, minor) {
        (0, Some(Some(m))) => Some(format!("0.{m}")),
        (0, _) | (_, Some(None)) => None,
        (n, _) => Some(n.to_string()),
    }
}

/// A `nabi_version`: the oldest toolchain a package builds with, a full
/// `x.y.z`.
pub fn nabi_version(s: &str) -> Result<[u64; 3], String> {
    let parts: Vec<&str> = s.split('.').collect();
    let ok = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return Err(format!(
            "`nabi_version: {s:?}` is not a toolchain version; write all three parts, e.g. \
             `nabi_version: \"{VERSION}\"`"
        ));
    }
    let n = |i: usize| parts[i].parse::<u64>().unwrap_or_default();
    Ok([n(0), n(1), n(2)])
}

/// Toolchain `version` builds what needs `nabi_version`: it is that one or
/// later, within its series (`0.y` below 1.0, else `x`), where nothing
/// breaks. A pre-release counts as its release.
pub fn builds_with(nabi_version: &str, version: &str) -> bool {
    let (Ok(min), Some(core)) = (
        self::nabi_version(nabi_version),
        version.split(['-', '+']).next(),
    ) else {
        return false;
    };
    let Ok(have) = self::nabi_version(core) else {
        return false;
    };
    let same = if min[0] == 0 {
        have[0] == 0 && have[1] == min[1]
    } else {
        have[0] == min[0]
    };
    same && have >= min
}

/// The toolchains `nabi_version` admits, in words: `nabi 0.1.3 or a later
/// 0.1.x`.
pub fn admits(nabi_version: &str) -> String {
    match self::nabi_version(nabi_version) {
        Ok([0, m, _]) => format!("nabi {nabi_version} or a later 0.{m}.x"),
        Ok([x, ..]) => format!("nabi {nabi_version} or a later {x}.x"),
        Err(_) => format!("nabi {nabi_version}"),
    }
}

/// `pin` accepts `version`; a pin that does not parse accepts none.
pub fn pin_matches(pin: &str, version: &str) -> bool {
    ToolchainReq::parse(pin).is_ok_and(|r| r.matches(version))
}

#[cfg(test)]
mod tests {
    use super::{ToolchainReq, admits, builds_with, nabi_version, series};

    #[test]
    fn nabi_version_is_a_series_floor() {
        assert!(builds_with("0.1.3", "0.1.3"));
        assert!(builds_with("0.1.3", "0.1.9"));
        assert!(builds_with("0.1.3", "0.1.4-dev"));
        assert!(!builds_with("0.1.3", "0.1.2"));
        assert!(!builds_with("0.1.3", "0.2.0"));
        assert!(builds_with("1.4.2", "1.9.0"));
        assert!(!builds_with("1.4.2", "2.0.0"));
        assert!(!builds_with("0.1", "0.1.3"));
        assert!(nabi_version("0.1").is_err() && nabi_version("0.1.0-rc.1").is_err());
        assert_eq!(admits("0.1.3"), "nabi 0.1.3 or a later 0.1.x");
        assert_eq!(admits("1.4.2"), "nabi 1.4.2 or a later 1.x");
    }

    #[test]
    fn pins_match_prefixes() {
        assert!(super::matches("0.3", "0.3.1"));
        assert!(super::matches("0.3.1", "0.3.1"));
        assert!(!super::matches("0.3", "0.30.0"));
        assert!(super::matches("0.1", "0.1.0-rc.1"));
        assert!(!super::matches("0.1.0", "0.1.0-rc.1"));
        assert!(!super::matches("0.3.1", "0.3"));
    }

    #[test]
    fn req_matches() {
        let m = |pin: &str, v: &str| ToolchainReq::parse(pin).unwrap().matches(v);
        // Prefixes keep their meaning.
        assert!(m("0.1", "0.1.5") && m("0.1.5", "0.1.5") && !m("0.1", "0.10.0"));
        assert!(m("^0.1.5", "0.1.9") && !m("^0.1.5", "0.1.4") && !m("^0.1.5", "0.2.0"));
        assert!(m("~0.1.5", "0.1.7") && !m("~0.1.5", "0.2.0"));
        assert!(m("=0.1.5", "0.1.5") && !m("=0.1.5", "0.1.6"));
        // A directory name parses; `find` matches it by name.
        let dir = "0.1.5-x86_64-unknown-linux-gnu";
        assert!(ToolchainReq::parse(dir).is_ok() && !m(dir, "0.1.5"));
        for bad in ["banana", "", "latest", "^x", "0.1.x", "v0.1"] {
            let e = ToolchainReq::parse(bad).unwrap_err();
            assert!(e.contains("write a version (`0.1`)"), "{bad}: {e}");
        }
    }

    #[test]
    fn series_of_versions() {
        assert_eq!(series("0.1.5").as_deref(), Some("0.1"));
        assert_eq!(series("0.1").as_deref(), Some("0.1"));
        assert_eq!(series("0.2.0-rc.1").as_deref(), Some("0.2"));
        assert_eq!(series("1.4.2").as_deref(), Some("1"));
        assert_eq!(series("9").as_deref(), Some("9"));
        for none in ["0", "x.1", "0.x", "", "0.1.y"] {
            assert_eq!(series(none), None, "{none}");
        }
    }
}
