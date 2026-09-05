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
pub struct LoggingObserver;

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
        tracing::info!(target: "difftrace::review", turn = ctx.turn, "turn started");
    }

    fn on_turn_end(&self, ctx: &TurnEndContext) {
        tracing::info!(target: "difftrace::review", turn = ctx.turn, "turn ended");
    }

    fn on_tool_pre(&self, ctx: &ToolPreContext) {
        tracing::info!(
            target: "difftrace::review",
            turn = ctx.turn,
            tool = ctx.tool.as_str(),
            "tool call"
        );
    }

    fn on_tool_post(&self, ctx: &ToolPostContext) {
        tracing::info!(
            target: "difftrace::review",
            turn = ctx.turn,
            tool = ctx.tool.as_str(),
            errored = ctx.is_error,
            "tool result"
        );
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
                .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
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
