//! Switchback experiments (R33, cluster-defined), ported from java-sdk's
//! `internal/Switchback*` classes and the Collector they mirror. See the
//! README section "Switchback Experiments" for the serving rules.
//!
//! [`assign_switchback`], [`base_hash`] and [`fisher_yates_permutation`] are
//! the platform's balanced-block draw, pinned by its golden vectors.

use serde_json::Value;

use crate::murmur3::murmur3_32;

const SEPARATOR: char = '\u{1f}';
const NORMALIZER: f64 = 1.0 / 0xffff_ffffu32 as f64;

/// The variant a switchback serves cluster `cluster_key` in period
/// `period_index`: entry `period_index mod arm_count` of the permutation for
/// cycle `period_index div arm_count` (floor division). `seed` is
/// `(seedHi << 32) | seedLo`. The key is the raw cluster value, never
/// hashed. `None` when `arm_count` is less than 2.
pub fn assign_switchback(
    cluster_key: &str,
    period_index: i64,
    seed: i64,
    arm_count: usize,
) -> Option<usize> {
    let arms = i64::try_from(arm_count).ok().filter(|&n| n >= 2)?;
    let cycle_index = period_index.div_euclid(arms);
    let cycle_slot = period_index.rem_euclid(arms) as usize;
    let permutation = fisher_yates_permutation(arm_count, base_hash(cluster_key, cycle_index, seed))?;
    Some(permutation[cycle_slot])
}

/// `murmur3_32(le64(seed) ++ le32(murmur3_32(utf8(key + "\u{1f}" + cycle_index))))`.
pub fn base_hash(cluster_key: &str, cycle_index: i64, seed: i64) -> u32 {
    let key = format!("{}{}{}", cluster_key, SEPARATOR, cycle_index);
    let key_hash = murmur3_32(key.as_bytes(), 0);
    let mut buffer = [0u8; 12];
    buffer[0..8].copy_from_slice(&seed.to_le_bytes());
    buffer[8..12].copy_from_slice(&key_hash.to_le_bytes());
    murmur3_32(&buffer, 0)
}

/// The Fisher-Yates permutation of `0..arm_count` that `base_hash` selects.
/// `None` when `arm_count` is less than 2.
pub fn fisher_yates_permutation(arm_count: usize, base_hash: u32) -> Option<Vec<usize>> {
    if arm_count < 2 {
        return None;
    }
    let mut permutation: Vec<usize> = (0..arm_count).collect();
    let mut buffer = [0u8; 8];
    buffer[0..4].copy_from_slice(&base_hash.to_le_bytes());
    for i in (1..arm_count).rev() {
        buffer[4..8].copy_from_slice(&(i as u32).to_le_bytes());
        let prob = f64::from(murmur3_32(&buffer, 0)) * NORMALIZER;
        // prob == 1.0 would map to i + 1.
        let j = ((prob * (i + 1) as f64).floor() as usize).min(i);
        permutation.swap(i, j);
    }
    Some(permutation)
}

/// The cluster key an attribute value gives, or `None`: a non-empty string
/// as-is, and an integer as its base-10 text. Every floating-point number
/// (`3.0` included), boolean, object, array, empty string and null gives
/// none. serde_json reads `-0` and integers beyond the `u64` range as
/// floating-point numbers, so they give none too.
pub fn cluster_key(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else {
                n.as_u64().map(|u| u.to_string())
            }
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Key(String),
    /// An unquoted segment that parses as an integer: it indexes an array.
    Index(i32),
}

/// A cluster attribute path, read like the Collector's
/// `SwitchbackAttributePath`: `/` separates segments, a segment quoted with
/// `'` or `"` is a literal key, and an unquoted integer segment indexes an
/// array. Any `.` (quoted or not) or an unterminated quote makes the path
/// malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributePath {
    segments: Vec<Segment>,
}

impl AttributePath {
    /// The path, or `None` when it is malformed, empty, or has an array
    /// index that is negative, first, last, or right after another index.
    pub fn parse(path: &str) -> Option<Self> {
        if path.contains('.') {
            return None;
        }
        let mut segments = Vec::new();
        let mut rest = path;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('/') {
                rest = after;
                continue;
            }
            let quote = rest.as_bytes()[0];
            if quote == b'\'' || quote == b'"' {
                let end = rest[1..].find(quote as char)? + 1;
                segments.push(Segment::Key(rest[1..end].to_string()));
                rest = &rest[end + 1..];
            } else {
                let end = rest.find('/').unwrap_or(rest.len());
                let key = &rest[..end];
                segments.push(match try_parse_int(key) {
                    Some(index) => Segment::Index(index),
                    None => Segment::Key(key.to_string()),
                });
                rest = &rest[end..];
            }
        }
        let n = segments.len();
        for (i, segment) in segments.iter().enumerate() {
            if let Segment::Index(index) = segment {
                if *index < 0 || i == 0 || i == n - 1 || matches!(segments[i - 1], Segment::Index(_)) {
                    return None;
                }
            }
        }
        if n == 0 {
            return None;
        }
        Some(Self { segments })
    }

    /// The attribute name the path starts at.
    pub fn root_key(&self) -> &str {
        match &self.segments[0] {
            Segment::Key(key) => key,
            Segment::Index(_) => unreachable!("a parsed path starts at a key"),
        }
    }

    /// The value below [`AttributePath::root_key`] in the attribute value
    /// `root`, or `None` when the path does not lead to one.
    pub fn resolve_attribute<'a>(&self, root: &'a Value) -> Option<&'a Value> {
        self.resolve_from(root, 1)
    }

    /// The value at the whole path in an object of attributes.
    pub fn resolve<'a>(&self, attributes: &'a Value) -> Option<&'a Value> {
        self.resolve_from(attributes, 0)
    }

    fn resolve_from<'a>(&self, root: &'a Value, first: usize) -> Option<&'a Value> {
        let mut current = root;
        for segment in &self.segments[first..] {
            current = match (segment, current) {
                (Segment::Index(index), Value::Array(items)) => items.get(*index as usize)?,
                (Segment::Key(key), Value::Object(map)) => map.get(key)?,
                _ => return None,
            };
        }
        Some(current)
    }
}

/// Guava `Ints.tryParse`: an optional `-`, then one or more ASCII digits,
/// within the `i32` range. `+1` and non-ASCII digits are not numbers; `-0`
/// is 0.
fn try_parse_int(s: &str) -> Option<i32> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A switchback's sources in declared order, or `None` when the cluster map
/// is missing, empty or has a path that is not supported (the legacy path).
pub(crate) fn sources(clusters: Option<&[(String, String)]>) -> Option<Vec<(String, AttributePath)>> {
    let clusters = clusters.filter(|c| !c.is_empty())?;
    clusters
        .iter()
        .map(|(unit_type, path)| Some((unit_type.clone(), AttributePath::parse(path)?)))
        .collect()
}

/// Which unit serves a switchback and under which cluster key (the
/// Collector's `ContextAPI.switchbackServing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Serving {
    /// No context unit of a linked type: the experiment does not apply.
    Absent,
    /// No value, units that disagree, or a value outside the allow-list:
    /// control under the first linked unit, with no exposure.
    Ineligible { unit_type: String },
    Eligible { unit_type: String, cluster_key: String },
}

/// Walk the sources in declared order. For each one with a context unit,
/// read its value with `read`; the first value is the cluster key and its
/// unit serves, and a later different value means the units disagree.
pub(crate) fn resolve_serving(
    sources: &[(String, AttributePath)],
    allowed: Option<&[String]>,
    has_unit: impl Fn(&str) -> bool,
    read: impl Fn(&AttributePath) -> Option<String>,
) -> Serving {
    let mut first: Option<&str> = None;
    let mut serving: Option<(&str, String)> = None;
    let mut disagreement = false;
    for (unit_type, path) in sources {
        if !has_unit(unit_type) {
            continue;
        }
        first.get_or_insert(unit_type);
        let Some(value) = read(path) else {
            continue;
        };
        match &serving {
            None => serving = Some((unit_type, value)),
            Some((_, key)) if *key != value => disagreement = true,
            Some(_) => {}
        }
    }
    let Some(first) = first else {
        return Serving::Absent;
    };
    match serving {
        Some((unit_type, cluster_key))
            if !disagreement && allowed.is_none_or(|a| a.is_empty() || a.contains(&cluster_key)) =>
        {
            Serving::Eligible {
                unit_type: unit_type.to_string(),
                cluster_key,
            }
        }
        _ => Serving::Ineligible {
            unit_type: first.to_string(),
        },
    }
}

/// The Collector's path for a missing, empty or unreadable cluster map: a
/// unit of the declared `unitType` only identifies an ineligible control.
pub(crate) fn legacy_serving(unit_type: Option<&str>, has_unit: impl Fn(&str) -> bool) -> Serving {
    match unit_type {
        Some(unit_type) if has_unit(unit_type) => Serving::Ineligible {
            unit_type: unit_type.to_string(),
        },
        _ => Serving::Absent,
    }
}

#[cfg(test)]
#[path = "switchback_tests.rs"]
mod tests;
