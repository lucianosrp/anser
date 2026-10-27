//! The one version string, stamped at build time (build.rs): FINANCE_VERSION
//! when set, else `git describe`, else the crate's version.

/// What this build is: "v0.1.0-3-gabc1234", "0.1.0+gabc1234" before any tag.
pub const VERSION: &str = env!("FINANCE_VERSION");

#[cfg(test)]
include!("../version_string.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins() {
        assert_eq!(resolve(Some("1.2.3"), Some("v0.1.0"), "0.1.0"), "1.2.3");
        assert_eq!(resolve(Some("  "), Some("v0.1.0"), "0.1.0"), "v0.1.0");
    }

    #[test]
    fn a_tag_is_kept_as_printed() {
        assert_eq!(resolve(None, Some("v0.1.0"), "0.1.0"), "v0.1.0");
        assert_eq!(resolve(None, Some("v0.1.0-3-gabc1234-dirty"), "0.1.0"), "v0.1.0-3-gabc1234-dirty");
    }

    #[test]
    fn before_any_tag_it_is_the_crate_version_and_the_hash() {
        assert_eq!(resolve(None, Some("abc1234"), "0.1.0"), "0.1.0+gabc1234");
        assert_eq!(resolve(None, Some("abc1234-dirty"), "0.1.0"), "0.1.0+gabc1234-dirty");
    }

    #[test]
    fn no_git_is_the_crate_version() {
        assert_eq!(resolve(None, None, "0.1.0"), "0.1.0");
        assert_eq!(resolve(None, Some(""), "0.1.0"), "0.1.0");
    }

    #[test]
    fn stamped() {
        assert!(!VERSION.is_empty());
    }
}
