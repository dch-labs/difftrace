//! A loopctl observer that mirrors run activity into `tracing`, so CI logs
//! show turns, tool calls, and outcomes without touching stdout.

use loopctl::observer::LoopObserver;
use loopctl::observer::RunEndContext;
use loopctl::observer::RunStartContext;
use loopctl::observer::ToolPostContext;
use loopctl::observer::ToolPreContext;
use loopctl::observer::TurnEndContext;
use loopctl::observer::TurnStartContext;

#[derive(Default)]
pub struct LoggingObserver {
    turn_started: std::sync::Mutex<Option<std::time::Instant>>,
    tools_secs: std::sync::Mutex<f64>,
    tool_started: std::sync::Mutex<Option<std::time::Instant>>,
}

impl LoopObserver for LoggingObserver {
    fn name(&self) -> &'static str {
        "difftrace-logging"
    }

    fn on_run_start(&self, _ctx: &RunStartContext) {
        tracing::info!(target: "difftrace::review", "run started");
    }

    fn on_run_end(&self, ctx: &RunEndContext) {
        match &ctx.error {
            Some(error) => {
                tracing::warn!(target: "difftrace::review", error, "run ended");
            }
            None => {
                tracing::info!(target: "difftrace::review", success = ctx.success, "run ended");
            }
        }
    }

    fn on_turn_start(&self, ctx: &TurnStartContext) {
        if let Ok(mut started) = self.turn_started.lock() {
            *started = Some(std::time::Instant::now());
        }
        if let Ok(mut tools) = self.tools_secs.lock() {
            *tools = 0.0;
        }
        tracing::info!(target: "difftrace::review", turn = ctx.turn, "turn started");
    }

    fn on_turn_end(&self, ctx: &TurnEndContext) {
        let total = self
            .turn_started
            .lock()
            .ok()
            .and_then(|mut started| started.take())
            .map_or(0.0, |started| started.elapsed().as_secs_f64());
        let tools = self.tools_secs.lock().map_or(0.0, |tools| *tools);
        tracing::debug!(
            target: "difftrace::review",
            turn = ctx.turn,
            total_secs = total,
            tools_secs = tools,
            model_secs = (total - tools).max(0.0),
            "turn timing; model ≈ turn minus tools"
        );
        tracing::info!(target: "difftrace::review", turn = ctx.turn, "turn ended");
    }

    fn on_tool_pre(&self, ctx: &ToolPreContext) {
        if let Ok(mut started) = self.tool_started.lock() {
            *started = Some(std::time::Instant::now());
        }
        tracing::info!(
            target: "difftrace::review",
            turn = ctx.turn,
            tool = ctx.tool.as_str(),
            "tool call"
        );
    }

    fn on_tool_post(&self, ctx: &ToolPostContext) {
        let elapsed = self
            .tool_started
            .lock()
            .ok()
            .and_then(|mut started| started.take())
            .map_or(0.0, |started| started.elapsed().as_secs_f64());
        if let Ok(mut tools) = self.tools_secs.lock() {
            *tools += elapsed;
        }
        tracing::debug!(
            target: "difftrace::review",
            turn = ctx.turn,
            tool = ctx.tool.as_str(),
            secs = elapsed,
            errored = ctx.is_error,
            "tool timing"
        );
        tracing::info!(
            target: "difftrace::review",
            turn = ctx.turn,
            tool = ctx.tool.as_str(),
            errored = ctx.is_error,
            "tool result"
        );
    }
}

pub struct TeeWriter {
    file: std::sync::Mutex<std::fs::File>,
}

pub struct TeeSink {
    file: Option<std::fs::File>,
}

impl std::io::Write for TeeSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(file) = &mut self.file {
            let _ignored = file.write_all(buf);
        }
        std::io::stderr().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            let _ignored = file.flush();
        }
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeWriter {
    type Writer = TeeSink;

    fn make_writer(&'a self) -> Self::Writer {
        TeeSink {
            file: self.file.lock().ok().and_then(|file| file.try_clone().ok()),
        }
    }
}

// The HTTP stack's debug output is connection noise; a bare verbosity
// level always quiets it, keeping debug for difftrace's own diagnostics.
const HTTP_QUIET: &str = ",reqwest=info,hyper=info,hyper_util=info,rustls=info";

#[must_use]
pub fn filter_directive(configured: &str, rust_log: Option<&str>) -> String {
    if configured.is_empty() {
        return match rust_log.filter(|value| !value.is_empty()) {
            Some(value) if value == "debug" || value == "trace" => {
                format!("{value}{HTTP_QUIET}")
            }
            Some(value) => value.to_owned(),
            None => "info".to_owned(),
        };
    }
    if configured == "debug" || configured == "trace" {
        return format!("{configured}{HTTP_QUIET}");
    }
    configured.to_owned()
}

#[must_use]
pub fn build_filter() -> tracing_subscriber::EnvFilter {
    let directive = filter_directive(
        &crate::config::resolve_log_level(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    tracing_subscriber::EnvFilter::try_new(directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

#[must_use]
pub fn install_local(log: std::fs::File) -> bool {
    let result = tracing_subscriber::fmt()
        .with_env_filter(build_filter())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(TeeWriter {
            file: std::sync::Mutex::new(log),
        })
        .try_init();
    if let Err(err) = &result {
        eprintln!("difftrace: cannot install the log subscriber: {err}");
    }
    result.is_ok()
}

#[must_use]
pub fn local_review_log(
    reviews_dir: &std::path::Path,
    checkout: &str,
    when: chrono::DateTime<chrono::Local>,
    head_sha: &str,
) -> std::path::PathBuf {
    reviews_dir.join(format!(
        "{checkout}-{}-{}.log",
        when.format("%Y-%m-%dT%H-%M-%S"),
        crate::review::registry::short_sha(head_sha)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_review_log_name_carries_checkout_time_and_head()
    -> Result<(), Box<dyn std::error::Error>> {
        use chrono::TimeZone;
        let when = chrono::Local
            .with_ymd_and_hms(2026, 9, 8, 16, 5, 0)
            .single()
            .ok_or("the fixture instant must exist")?;
        let path = local_review_log(
            std::path::Path::new("/tmp/reviews"),
            "dch",
            when,
            "9f3b2c1full",
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("dch-2026-09-08T16-05-00-9f3b2c1.log"),
            "the name names the checkout, sorts by time, and names the reviewed head"
        );
        Ok(())
    }

    #[test]
    fn a_bare_rust_log_level_quiets_the_http_stack() {
        assert_eq!(
            filter_directive("", Some("debug")),
            format!("debug{HTTP_QUIET}"),
            "a bare verbosity level means difftrace's own diagnostics"
        );
        assert_eq!(
            filter_directive("", Some("trace")),
            format!("trace{HTTP_QUIET}")
        );
    }

    #[test]
    fn a_directive_rust_log_passes_through_untouched() {
        assert_eq!(
            filter_directive("", Some("difftrace=trace,hyper=trace")),
            "difftrace=trace,hyper=trace",
            "targeted directives are the raw escape hatch"
        );
        assert_eq!(filter_directive("", Some("")), "info");
        assert_eq!(filter_directive("", None), "info");
    }

    #[test]
    fn a_configured_level_ignores_rust_log_and_clamps_only_at_debug() {
        assert_eq!(
            filter_directive("debug", Some("hyper=trace")),
            format!("debug{HTTP_QUIET}"),
            "an explicit setting wins over the environment"
        );
        assert_eq!(filter_directive("warn", Some("debug")), "warn");
    }

    #[test]
    fn a_span_close_line_carries_the_busy_time() {
        let (logs, _guard) = test_support::install();
        {
            let span = tracing::debug_span!("probe_timing_span");
            let entered = span.enter();
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(entered);
            drop(span);
        }
        let text = logs.text();
        assert!(
            text.contains("close") && text.contains("probe_timing_span"),
            "span closes are logged with the span name: {text}"
        );
        assert!(
            text.contains("time.busy"),
            "span closes carry the busy-time measurement: {text}"
        );
    }

    #[test]
    fn the_tee_reaches_the_file() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "difftrace-tee-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ));
        let file = std::fs::File::create(&path)?;
        {
            let tee = TeeWriter {
                file: std::sync::Mutex::new(file),
            };
            let mut sink = tracing_subscriber::fmt::MakeWriter::make_writer(&tee);
            std::io::Write::write_all(&mut sink, b"review diagnostics\n")?;
            std::io::Write::flush(&mut sink)?;
        }
        let text = std::fs::read_to_string(&path)?;
        let _unused = std::fs::remove_file(&path);
        assert_eq!(
            text, "review diagnostics\n",
            "the file sink receives every byte"
        );
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    #[derive(Clone, Default)]
    pub(crate) struct SharedLogBuffer {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedLogBuffer {
        pub(crate) fn text(&self) -> String {
            self.inner
                .lock()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        }
    }

    impl Write for SharedLogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner
                .lock()
                .map_err(|_| std::io::Error::other("poisoned log buffer"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // One process-global subscriber routes to the installing thread's buffer.
    // A thread-local dispatcher races with tracing's shared callsite-interest
    // cache: a concurrent test evaluating a callsite with no dispatcher
    // poisons it to "never" and that event is silently lost everywhere. A
    // global registration rebuilds the cache once, and the routing keeps
    // parallel installs isolated to their own buffers. A log test on a
    // multi-thread tokio runtime would see nothing from worker-thread
    // emissions — log capture assumes the installing thread does the
    // emitting.
    static ROUTER: OnceLock<()> = OnceLock::new();

    thread_local! {
        static BUFFER: RefCell<Option<SharedLogBuffer>> = const { RefCell::new(None) };
    }

    #[derive(Clone, Copy, Default)]
    struct RoutedWriter;

    impl Write for RoutedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let routed = BUFFER.with(|slot| slot.borrow().as_ref().cloned());
            match routed {
                Some(mut buffer) => buffer.write(buf),
                None => Ok(buf.len()),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub(crate) struct RoutedGuard;

    impl Drop for RoutedGuard {
        fn drop(&mut self) {
            BUFFER.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(crate) fn install() -> (SharedLogBuffer, RoutedGuard) {
        ROUTER.get_or_init(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
                .with_writer(|| RoutedWriter)
                .with_ansi(false)
                .finish();
            assert!(
                tracing::subscriber::set_global_default(subscriber).is_ok(),
                "the process-global log router is already claimed"
            );
        });
        let buffer = SharedLogBuffer::default();
        BUFFER.with(|slot| *slot.borrow_mut() = Some(buffer.clone()));
        (buffer, RoutedGuard)
    }
}
