//! Pins that the resolved loopctl dependency carries the engine
//! hardening this agent's judge loops depend on.
//!
//! loopctl rides as an unpinned git dependency: `cargo update` can
//! re-resolve it at any time, and an older resolve silently reintroduces
//! the review-run pathology this crate has already paid for once — a
//! 0.3-line engine whose compaction dropped judge reasoning mid-run, so
//! judges re-read and re-asked the same regions until the loop detector
//! fired on every turn. The hardening (turn-scoped reasoning retention,
//! the compaction progress ledger, compaction durability, typed
//! retention) shipped across the 0.4 line; this file holds the floor by
//! reading the git-sourced loopctl entry out of `Cargo.lock` — a lock
//! shape this parser does not recognize fails the pin, because an
//! unreadable floor is not a held one.

use std::path::PathBuf;

const LOOPCTL_FLOOR: (u64, u64, u64) = (0, 4, 0);

fn lockfile() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock")
}

fn resolved_loopctl_version(lock: &str) -> Option<(u64, u64, u64)> {
    let entry = lock
        .split("[[package]]")
        .find(|entry| entry.contains("name = \"loopctl\"") && entry.contains("source = \"git+"))?;
    let version_line = entry.lines().find(|l| l.starts_with("version ="))?;
    let quoted = version_line.split('"').nth(1)?;
    let mut parts = quoted.split('.');
    Some((
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
    ))
}

fn resolution_report(lock_text: Option<String>) -> String {
    let Some(lock) = lock_text else {
        return "Cargo.lock was unreadable beside the manifest".to_owned();
    };
    match resolved_loopctl_version(&lock) {
        Some(version) => format!(
            "resolved loopctl {}.{}.{} against the {}.{}.{} floor",
            version.0, version.1, version.2, LOOPCTL_FLOOR.0, LOOPCTL_FLOOR.1, LOOPCTL_FLOOR.2
        ),
        None => "the lock holds no git-sourced loopctl package entry this \
                 parser recognizes"
            .to_owned(),
    }
}

#[test]
fn the_resolved_loopctl_carries_the_engine_hardening() {
    let lock_text = std::fs::read_to_string(lockfile()).ok();
    let held = lock_text
        .as_deref()
        .and_then(resolved_loopctl_version)
        .is_some_and(|version| version >= LOOPCTL_FLOOR);
    assert!(
        held,
        "the judge loops would lose reasoning across compaction and \
         repeat turns until the loop detector fires — {report}; \
         re-resolve with `cargo update -p loopctl` on a current master",
        report = resolution_report(lock_text),
    );
}

#[test]
fn the_version_parser_reads_the_shape_the_lock_writes() {
    let lock = concat!(
        "[[package]]\n",
        "name = \"serde\"\n",
        "version = \"1.0.219\"\n",
        "\n",
        "[[package]]\n",
        "name = \"loopctl\"\n",
        "version = \"0.4.1\"\n",
        "source = \"git+https://github.com/dch-labs/loopctl#d365f3e8\"\n",
    );
    assert_eq!(
        resolved_loopctl_version(lock),
        Some((0, 4, 1)),
        "the git-sourced entry is the one read, not the first package"
    );
    assert!(
        resolution_report(Some("not a lockfile".to_owned())).contains("no git-sourced loopctl"),
        "an unrecognized shape reads as no version, never a stale one"
    );
}
