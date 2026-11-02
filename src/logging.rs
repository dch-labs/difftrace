//! Run transcripts and RUST_LOG-filtered diagnostics.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::DtError;
use crate::state::file::sanitize_label;

const HTTP_QUIET: &str = ",reqwest=info,hyper=info,hyper_util=info,rustls=info";

#[must_use]
pub fn filter_directive(rust_log: Option<&str>) -> String {
    match rust_log.filter(|value| !value.is_empty()) {
        Some(value) if value == "debug" || value == "trace" => format!("{value}{HTTP_QUIET}"),
        Some(value) => value.to_owned(),
        None => "info".to_owned(),
    }
}

#[must_use]
pub fn build_filter() -> tracing_subscriber::EnvFilter {
    let directive = filter_directive(std::env::var("RUST_LOG").ok().as_deref());
    tracing_subscriber::EnvFilter::try_new(directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

pub fn init_diagnostics() {
    let result = tracing_subscriber::fmt()
        .with_env_filter(build_filter())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .try_init();
    if let Err(err) = result {
        eprintln!("difftrace: cannot install the log subscriber: {err}");
    }
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[must_use]
pub fn transcript_path(dir: &Path, checkout: &str, sha: &str) -> PathBuf {
    dir.join(format!(
        "{}-{}-{}.log",
        sanitize_label(checkout),
        timestamp(),
        sanitize_label(sha)
    ))
}

pub fn write_transcript(
    dir: &Path,
    checkout: &str,
    sha: &str,
    body: &str,
) -> Result<PathBuf, DtError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| DtError::Config(format!("could not create {}: {err}", dir.display())))?;
    let base = format!(
        "{}-{}-{}",
        sanitize_label(checkout),
        timestamp(),
        sanitize_label(sha)
    );
    let mut path = dir.join(format!("{base}.log"));
    for attempt in 1..1000 {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{base}.{attempt}.log"));
    }
    let text = format!(
        "# difftrace {} transcript\n{body}",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(&path, text)
        .map_err(|err| DtError::Config(format!("could not write {}: {err}", path.display())))?;
    tracing::info!(
        target: "difftrace::logging",
        transcript = path.display().to_string(),
        "transcript written"
    );
    Ok(path)
}

pub fn write_report(dir: &Path, checkout: &str, sha: &str, body: &str) -> Result<PathBuf, DtError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| DtError::Config(format!("could not create {}: {err}", dir.display())))?;
    let base = format!(
        "{}-{}-{}",
        sanitize_label(checkout),
        timestamp(),
        sanitize_label(sha)
    );
    let mut path = dir.join(format!("{base}.report.json"));
    for attempt in 1..1000 {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{base}.{attempt}.report.json"));
    }
    std::fs::write(&path, body)
        .map_err(|err| DtError::Config(format!("could not write {}: {err}", path.display())))?;
    tracing::info!(
        target: "difftrace::logging",
        report = path.display().to_string(),
        "report written"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_directive_quiets_http_at_debug_and_defaults_to_info() {
        assert_eq!(filter_directive(None), "info");
        assert_eq!(filter_directive(Some("")), "info");
        assert_eq!(filter_directive(Some("warn")), "warn");
        assert_eq!(
            filter_directive(Some("debug")),
            format!("debug{HTTP_QUIET}")
        );
        assert_eq!(
            filter_directive(Some("trace")),
            format!("trace{HTTP_QUIET}")
        );
    }

    #[test]
    fn transcripts_write_a_versioned_header_and_a_sanitized_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("difftrace-transcript-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        let path = write_transcript(
            &dir,
            "../../evil/../checkout",
            "abc123",
            "## Verdict\nbody\n",
        )?;
        let text = std::fs::read_to_string(&path)?;
        let name = path
            .file_name()
            .map_or_else(String::new, |raw| raw.to_string_lossy().into_owned());
        assert!(
            text.starts_with(&format!(
                "# difftrace {} transcript\n",
                env!("CARGO_PKG_VERSION")
            )),
            "the header names the writing version: {text}"
        );
        assert!(text.contains("## Verdict\nbody"));
        assert!(
            !name.contains('/'),
            "a hostile checkout label cannot escape the transcript dir: {name}"
        );
        assert!(
            name.ends_with("-abc123.log"),
            "the sha rides the name: {name}"
        );
        let second = write_transcript(
            &dir,
            "../../evil/../checkout",
            "abc123",
            "## Verdict\ntwo\n",
        )?;
        assert_ne!(
            path, second,
            "two writes inside one second never overwrite each other"
        );
        assert!(
            std::fs::read_to_string(&path).is_ok_and(|kept| kept.contains("## Verdict\nbody")),
            "the earlier transcript survives the second write"
        );
        assert!(
            std::fs::read_to_string(&second).is_ok_and(|fresh| fresh.contains("## Verdict\ntwo")),
            "the second transcript lands in its own file"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
