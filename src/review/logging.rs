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
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::Mutex;

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

    pub(crate) fn install() -> (SharedLogBuffer, tracing::subscriber::DefaultGuard) {
        let buffer = SharedLogBuffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (buffer, guard)
    }
}
