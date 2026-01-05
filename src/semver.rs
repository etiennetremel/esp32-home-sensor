/// Semantic version representation for comparing firmware versions.
/// Supports standard semver format: major.minor.patch[-prerelease]
/// Examples: "1.2.3", "v1.2.3", "1.2.3-beta", "v1.2.3-rc.1"
#[derive(Debug, PartialEq, Eq)]
pub struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
    /// Pre-release identifier (e.g., "beta.0", "rc.1", "alpha")
    /// None means it's a stable release (stable > pre-release)
    pre_release: Option<PreRelease>,
}

/// Pre-release version component (e.g., "beta.1", "rc.2", "alpha")
#[derive(Debug, PartialEq, Eq)]
struct PreRelease {
    /// The type of pre-release (alpha < beta < rc < other)
    kind: PreReleaseKind,
    /// Optional numeric suffix (e.g., the "1" in "beta.1")
    number: Option<u32>,
}

/// Pre-release type ordering: alpha < beta < rc < other < stable
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum PreReleaseKind {
    Alpha,
    Beta,
    Rc,
    Other, // Unknown pre-release types sort after rc but before stable
}

impl PreRelease {
    /// Parse a pre-release string like "beta", "beta.0", "rc1", "alpha"
    fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();

        // Try to find a known pre-release kind
        let (kind, remainder) = if s.starts_with("alpha") {
            (PreReleaseKind::Alpha, &s[5..])
        } else if s.starts_with("beta") {
            (PreReleaseKind::Beta, &s[4..])
        } else if s.starts_with("rc") {
            (PreReleaseKind::Rc, &s[2..])
        } else {
            (PreReleaseKind::Other, s.as_str())
        };

        // Parse optional numeric suffix (handles ".0", "0", ".1", "1", etc.)
        let number = if remainder.is_empty() {
            None
        } else {
            let num_str = remainder.trim_start_matches('.');
            num_str.parse().ok()
        };

        Some(PreRelease { kind, number })
    }

    /// Compare pre-releases by kind first, then by number.
    fn cmp(&self, other: &PreRelease) -> core::cmp::Ordering {
        use core::cmp::Ordering;

        // First compare by kind (alpha < beta < rc < other)
        match self.kind.cmp(&other.kind) {
            Ordering::Equal => {}
            ord => return ord,
        }

        // Then by number (None < Some(0) < Some(1) < ...)
        match (&self.number, &other.number) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl SemVer {
    /// Parse a semver string like "1.2.3", "v1.2.3", "1.2.3-beta", "v1.2.3-beta.0"
    pub fn parse(version: &str) -> Option<Self> {
        let version = version.trim();

        // Strip optional 'v' or 'V' prefix
        let version = version
            .strip_prefix('v')
            .or_else(|| version.strip_prefix('V'))
            .unwrap_or(version);

        // Split on '-' to separate version from pre-release
        let (version_part, pre_release_part) = match version.split_once('-') {
            Some((v, p)) => (v, Some(p)),
            None => (version, None),
        };

        let mut parts = version_part.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;

        // Ensure no extra parts in version (e.g., reject "1.2.3.4")
        if parts.next().is_some() {
            return None;
        }

        let pre_release = pre_release_part.and_then(PreRelease::parse);

        Some(SemVer {
            major,
            minor,
            patch,
            pre_release,
        })
    }

    /// Returns true if self is strictly greater than other.
    /// Stable releases are greater than pre-releases with same version.
    pub fn is_greater_than(&self, other: &SemVer) -> bool {
        use core::cmp::Ordering;

        // Compare major.minor.patch first
        if self.major != other.major {
            return self.major > other.major;
        }
        if self.minor != other.minor {
            return self.minor > other.minor;
        }
        if self.patch != other.patch {
            return self.patch > other.patch;
        }

        // Same major.minor.patch - compare pre-release
        // A stable release (no pre-release) is greater than any pre-release
        match (&self.pre_release, &other.pre_release) {
            (None, None) => false,    // Equal versions
            (None, Some(_)) => true,  // Stable > pre-release
            (Some(_), None) => false, // Pre-release < stable
            (Some(a), Some(b)) => a.cmp(b) == Ordering::Greater,
        }
    }
}
