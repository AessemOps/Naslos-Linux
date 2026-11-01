//! Strict semantic versions for release selection (spec FR-UPD).
//!
//! Only `X.Y.Z` (with an optional leading `v`) is accepted: the installer and
//! the web upgrader both ignore non-semver tags so a moving `latest` can never
//! become a release.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse `X.Y.Z` or `vX.Y.Z`; anything else (pre-release, build metadata,
    /// extra components) is rejected.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.strip_prefix('v').unwrap_or(input);
        let mut parts = s.split('.');
        let major = numeric(parts.next()?)?;
        let minor = numeric(parts.next()?)?;
        let patch = numeric(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// Parse a release tag: it MUST carry the leading `v` (the installer's
    /// resolution rule) and be strict `vX.Y.Z`.
    pub fn parse_tag(tag: &str) -> Option<Self> {
        let rest = tag.strip_prefix('v')?;
        Self::parse(rest)
    }

    pub fn as_tag(&self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// Difference in minor versions (major counts as 1000 minors). Negative
    /// when `self` is older than `other`.
    pub fn minor_distance(&self, other: &Self) -> i64 {
        (self.major as i64 * 1000 + self.minor as i64)
            - (other.major as i64 * 1000 + other.minor as i64)
    }
}

fn numeric(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_semver_only() {
        assert_eq!(Version::parse("1.2.3").unwrap().as_tag(), "v1.2.3");
        assert_eq!(Version::parse("v1.2.3").unwrap().to_string(), "1.2.3");
        assert!(Version::parse("1.2").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.2.3-rc1").is_none());
        assert!(Version::parse("latest").is_none());
        assert!(Version::parse("v1.2.3").is_some());
        assert!(Version::parse_tag("1.2.3").is_none());
        assert!(Version::parse_tag("v1.2.3").is_some());
        assert!(Version::parse_tag("v01.2.3").is_some());
    }

    #[test]
    fn ordering_and_minor_distance() {
        let a = Version::parse("1.37.0").unwrap();
        let b = Version::parse("1.38.0").unwrap();
        assert!(a < b);
        assert_eq!(b.minor_distance(&a), 1);
        assert_eq!(a.minor_distance(&b), -1);
        assert_eq!(Version::parse("2.0.0").unwrap().minor_distance(&a), 963);
    }
}
