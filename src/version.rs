//! Compile-time version info.
//!
//! `CARGO_PKG_VERSION` comes from `Cargo.toml`; `NJUSKA_GIT_SHA` is emitted
//! by `build.rs` (short SHA of HEAD, or `"unknown"` outside a git checkout).
//! `concat!` + `env!` glue them together at compile time — `VERSION` is a
//! true `&'static str` in read-only data, no runtime formatting.

/// Human-readable version string, e.g. `0.1.0 (5371cfe)`.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("NJUSKA_GIT_SHA"), ")");

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn version_contains_cargo_pkg_version() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
        // The SHA part is either a hex string or the "unknown" fallback —
        // both are non-empty and wrapped in parentheses.
        assert!(VERSION.contains('('), "{VERSION}");
        assert!(VERSION.ends_with(')'), "{VERSION}");
    }
}
