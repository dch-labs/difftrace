//! Smoke tests for the crate skeleton.

#[test]
fn the_package_version_has_major_and_minor() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(version.split('.').count() >= 2);
}

#[test]
fn the_crate_name_is_difftrace() {
    let name = env!("CARGO_PKG_NAME");
    assert_eq!(name, "difftrace");
}
