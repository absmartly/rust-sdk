//! Semantic version comparison used by the `semver_*` operators. Mirrors the
//! JavaScript and Java SDKs: a leading `v`/`V` and build metadata are ignored,
//! missing core parts count as 0, numeric identifiers compare numerically
//! (leading zeros ignored) and rank below alphanumeric ones, and a release
//! ranks above any of its pre-releases.
//!
//! Parsing is as permissive as the other SDKs', so they all order the same
//! inputs the same way: a non-numeric core part (as in `1.x`) is compared as an
//! alphanumeric identifier, and an empty pre-release (`1.0.0-`) is a release.

use std::cmp::Ordering;

struct Version<'a> {
    parts: Vec<&'a str>,
    pre_release: &'a str,
}

fn parse(version: &str) -> Option<Version<'_>> {
    let v = version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version);
    let v = v.split_once('+').map_or(v, |(core, _)| core);
    if v.is_empty() {
        return None;
    }

    let (core, pre_release) = v.split_once('-').unwrap_or((v, ""));
    if core.is_empty() {
        return None;
    }

    Some(Version {
        parts: core.split('.').collect(),
        pre_release,
    })
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn strip_leading_zeros(s: &str) -> &str {
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() {
        "0"
    } else {
        trimmed
    }
}

fn compare_identifiers(a: &str, b: &str) -> Ordering {
    match (is_numeric(a), is_numeric(b)) {
        (true, true) => {
            let (a, b) = (strip_leading_zeros(a), strip_leading_zeros(b));
            a.len().cmp(&b.len()).then_with(|| a.cmp(b))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

/// Compare two version strings. `None` when either has an empty version core
/// after removing a leading `v`/`V` and build metadata (for example `""`,
/// `"v"`, `"+build"` or `"-beta"`).
pub fn compare(lhs: &str, rhs: &str) -> Option<Ordering> {
    let l = parse(lhs)?;
    let r = parse(rhs)?;

    let len = l.parts.len().max(r.parts.len());
    for i in 0..len {
        let lp = l.parts.get(i).copied().unwrap_or("0");
        let rp = r.parts.get(i).copied().unwrap_or("0");
        let result = compare_identifiers(lp, rp);
        if result != Ordering::Equal {
            return Some(result);
        }
    }

    match (l.pre_release.is_empty(), r.pre_release.is_empty()) {
        (true, true) => return Some(Ordering::Equal),
        (true, false) => return Some(Ordering::Greater),
        (false, true) => return Some(Ordering::Less),
        (false, false) => {}
    }

    let lpre: Vec<&str> = l.pre_release.split('.').collect();
    let rpre: Vec<&str> = r.pre_release.split('.').collect();
    for i in 0..lpre.len().max(rpre.len()) {
        match (lpre.get(i), rpre.get(i)) {
            (None, _) => return Some(Ordering::Less),
            (_, None) => return Some(Ordering::Greater),
            (Some(a), Some(b)) => {
                let result = compare_identifiers(a, b);
                if result != Ordering::Equal {
                    return Some(result);
                }
            }
        }
    }
    Some(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test]
    fn test_core_versions() {
        assert_eq!(compare("1.2.3", "1.2.3"), Some(Equal));
        assert_eq!(compare("1.2.3", "1.2.4"), Some(Less));
        assert_eq!(compare("1.10.0", "1.9.0"), Some(Greater));
        assert_eq!(compare("2.0.0", "10.0.0"), Some(Less));
        assert_eq!(compare("1", "1.0.0"), Some(Equal));
        assert_eq!(compare("1.0.1", "1"), Some(Greater));
        assert_eq!(compare("01.002.3", "1.2.3"), Some(Equal));
        assert_eq!(compare("00", "0"), Some(Equal));
    }

    #[test]
    fn test_prefix_and_build_metadata_ignored() {
        assert_eq!(compare("v1.2.3", "1.2.3"), Some(Equal));
        assert_eq!(compare("V1.2.3", "v1.2.3"), Some(Equal));
        assert_eq!(compare("1.2.3+build.5", "1.2.3+other"), Some(Equal));
        assert_eq!(compare("1.2.3-beta+build", "1.2.3-beta"), Some(Equal));
    }

    #[test]
    fn test_pre_release_ordering() {
        assert_eq!(compare("1.0.0-alpha", "1.0.0"), Some(Less));
        assert_eq!(compare("1.0.0", "1.0.0-rc.1"), Some(Greater));
        assert_eq!(compare("1.0.0-alpha", "1.0.0-alpha.1"), Some(Less));
        assert_eq!(compare("1.0.0-alpha.1", "1.0.0-alpha.beta"), Some(Less));
        assert_eq!(compare("1.0.0-beta.2", "1.0.0-beta.11"), Some(Less));
        assert_eq!(compare("1.0.0-rc.1", "1.0.0-beta.11"), Some(Greater));
        assert_eq!(compare("1.0.0-", "1.0.0"), Some(Equal));
    }

    #[test]
    fn test_permissive_core() {
        assert_eq!(compare("1.x", "1.0"), Some(Greater));
        assert_eq!(compare("abc", "1"), Some(Greater));
        assert_eq!(compare("1..0", "1.0.0"), Some(Greater));
    }

    #[test]
    fn test_empty_core_is_none() {
        assert_eq!(compare("", "1.0.0"), None);
        assert_eq!(compare("1.0.0", "v"), None);
        assert_eq!(compare("+build", "1"), None);
        assert_eq!(compare("-beta", "1"), None);
    }
}
