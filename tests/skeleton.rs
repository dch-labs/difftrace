//! Smoke tests for the crate skeleton.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::missing_panics_doc,
        clippy::missing_errors_doc,
    )
)]

#[test]
fn test_package_version_has_major_and_minor() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(version.split('.').count() >= 2);
}

#[test]
fn test_crate_name_is_difftrace() {
    let name = env!("CARGO_PKG_NAME");
    assert_eq!(name, "difftrace");
}
