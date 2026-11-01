//! Gateway-backed judge tools under one per-region call budget: the read
//! tool delegates to loopctl's paged [`ReadTool`] over the pinned head, so
//! no single view can dominate the context, and a repeated identical call
//! answers with a target-naming pointer so the engine's loop-detector
//! signature never re-forms.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use loopctl::tool::builtin::read::{ContentSource, ReadTool, SourceContent};
use loopctl::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSchema};
use serde_json::{Value, json};

use crate::gateway::PrGateway;

pub const SEARCH_CAP: usize = 20;

#[must_use]
pub fn unchanged_notice(ask: u32, target: &str) -> String {
    format!(
        "unchanged (ask #{ask} of {target}): the {ask}. call of this exact \
         target returns the same result as before — re-examine the content \
         already above instead of calling again"
    )
}

#[derive(Default)]
pub struct SeenCalls {
    reads: Mutex<std::collections::HashMap<String, u32>>,

    searches: Mutex<std::collections::HashMap<String, u32>>,
}

impl SeenCalls {
    pub fn read_ask(&self, ask: &str) -> u32 {
        let mut seen = self
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = seen.entry(ask.to_owned()).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    pub fn search_ask(&self, pattern: &str) -> u32 {
        let mut seen = self
            .searches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ask = seen.entry(pattern.to_owned()).or_insert(0);
        *ask = ask.saturating_add(1);
        *ask
    }
}

pub struct PinnedHeadSource {
    gateway: Arc<dyn PrGateway>,
}

impl PinnedHeadSource {
    #[must_use]
    pub fn new(gateway: Arc<dyn PrGateway>) -> Self {
        Self { gateway }
    }
}

impl ContentSource for PinnedHeadSource {
    fn read<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<SourceContent, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if path.trim().is_empty() {
                return Err(ToolError::InvalidInput(
                    "an empty path names no file at the pinned head".to_owned(),
                ));
            }
            let gateway = Arc::clone(&self.gateway);
            let target = path.to_owned();
            let snapshot = tokio::task::spawn_blocking(move || gateway.read_file_at_head(&target))
                .await
                .map_err(|err| {
                    ToolError::Execution(format!("the pinned read task failed to join: {err}"))
                })?
                .map_err(|err| ToolError::Execution(err.to_string()))?;
            Ok(SourceContent::Text(snapshot.content))
        })
    }
}

pub struct ReadFileAtHead {
    inner: ReadTool<PinnedHeadSource>,
    budget: Arc<AtomicUsize>,
    cap: usize,
    seen: Arc<SeenCalls>,
    served: Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
}

pub struct SearchAtHead {
    gateway: Arc<dyn PrGateway>,
    budget: Arc<AtomicUsize>,
    cap: usize,
    seen: Arc<SeenCalls>,
}

fn ask_key(input: &Value, path: &str) -> String {
    let window = |field: &str| {
        input
            .get(field)
            .map_or_else(String::new, ToString::to_string)
    };
    format!(
        "{path}|{}|{}|{}",
        window("offset"),
        window("limit"),
        window("line_range")
    )
}

impl ReadFileAtHead {
    pub fn new(
        gateway: Arc<dyn PrGateway>,
        budget: Arc<AtomicUsize>,
        cap: usize,
        seen: Arc<SeenCalls>,
        served: Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
    ) -> Self {
        Self {
            inner: ReadTool::new(PinnedHeadSource::new(gateway)),
            budget,
            cap,
            seen,
            served,
        }
    }
}

#[must_use]
pub fn served_window(viewed: &str) -> Option<(u32, u32)> {
    let mut first: Option<u32> = None;
    let mut last: u32 = 0;
    for line in viewed.lines() {
        let number = line
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse::<u32>().ok());
        if let Some(number) = number {
            first = Some(first.unwrap_or(number));
            last = number;
        }
    }
    first.map(|first| (first, last))
}

fn record_served(
    served: &std::sync::Mutex<crate::contracts::ServedReads>,
    file: &str,
    window: (u32, u32),
) {
    let mut log = served
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    log.entry(file.to_owned()).or_default().push(window);
}

impl SearchAtHead {
    pub fn new(
        gateway: Arc<dyn PrGateway>,
        budget: Arc<AtomicUsize>,
        cap: usize,
        seen: Arc<SeenCalls>,
    ) -> Self {
        Self {
            gateway,
            budget,
            cap,
            seen,
        }
    }
}

fn spend(budget: &AtomicUsize, cap: usize, target: &str) -> Option<String> {
    let used = budget.fetch_add(1, Ordering::Relaxed);
    if used >= cap {
        return Some(format!(
            "the region tool budget of {cap} calls is exhausted (refused: {target}); finish and return the report"
        ));
    }
    None
}

fn read_schema(name: &str, description: &str, field: &str) -> ToolSchema {
    ToolSchema::new(
        name,
        description,
        json!({
            "type": "object",
            "properties": { field: { "type": "string" } },
            "required": [field]
        }),
    )
}

impl Tool for ReadFileAtHead {
    fn name(&self) -> &'static str {
        "read_file_at_head"
    }

    fn description(&self) -> &'static str {
        "Read a file at the pinned head commit, numbered line by line; input {\"path\": \"src/a.rs\", \"offset\": 1, \"limit\": 400}. Whole content when it fits; a cut view always names where reading continues — page with offset instead of re-reading"
    }

    fn schema(&self) -> ToolSchema {
        let inner = self.inner.schema();
        ToolSchema::new(self.name(), self.description(), inner.input_schema)
    }

    fn call(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        let path = match input.get("path").and_then(Value::as_str) {
            Some(path) => path.to_owned(),
            None => {
                return Box::pin(async move {
                    Ok(ToolOutput::success(
                        "expected {\"path\": \"src/a.rs\"} as the input".to_owned(),
                    ))
                });
            }
        };
        let ask = self.seen.read_ask(&ask_key(&input, &path));
        if ask > 1 {
            let notice = unchanged_notice(ask, &path);
            return Box::pin(async move { Ok(ToolOutput::success(notice)) });
        }
        if let Some(refusal) = spend(&self.budget, self.cap, &path) {
            return Box::pin(async move { Ok(ToolOutput::success(refusal)) });
        }
        let viewed = self.inner.call(input, context);
        let served = Arc::clone(&self.served);
        Box::pin(async move {
            let viewed = viewed.await?;
            let text = viewed.text_content();
            if let Some(window) = served_window(&text) {
                record_served(&served, &path, window);
            }
            Ok(ToolOutput::success(format!("{path}\n{text}")))
        })
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

impl Tool for SearchAtHead {
    fn name(&self) -> &'static str {
        "search_at_head"
    }

    fn description(&self) -> &'static str {
        "Search the pinned head tree with a POSIX basic regex; input {\"pattern\": \"fn name(\"}"
    }

    fn schema(&self) -> ToolSchema {
        read_schema(self.name(), self.description(), "pattern")
    }

    fn call(
        &self,
        input: Value,
        _context: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        let gateway = Arc::clone(&self.gateway);
        let budget = Arc::clone(&self.budget);
        let cap = self.cap;
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
                return Ok(ToolOutput::success(
                    "expected {\"pattern\": \"fn target(\"} as the input".to_owned(),
                ));
            };
            let ask = seen.search_ask(pattern);
            if ask > 1 {
                return Ok(ToolOutput::success(unchanged_notice(ask, pattern)));
            }
            if let Some(refusal) = spend(&budget, cap, pattern) {
                return Ok(ToolOutput::success(refusal));
            }
            match gateway.search_at_head(pattern, SEARCH_CAP) {
                Ok(hits) if hits.is_empty() => {
                    Ok(ToolOutput::success(format!("no matches for {pattern}")))
                }
                Ok(hits) => {
                    let lines = hits
                        .iter()
                        .map(|hit| format!("{}:{}: {}", hit.file, hit.line, hit.text))
                        .collect::<Vec<_>>();
                    Ok(ToolOutput::success(lines.join("\n")))
                }
                Err(err) => Ok(ToolOutput::success(format!("search failed: {err}"))),
            }
        })
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}

#[must_use]
pub fn region_registry(
    gateway: Arc<dyn PrGateway>,
    budget: Arc<AtomicUsize>,
    cap: usize,
    search: bool,
    served: &Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
) -> ToolRegistry {
    let registry = ToolRegistry::new();
    let seen = Arc::new(SeenCalls::default());
    registry.register(ReadFileAtHead::new(
        Arc::clone(&gateway),
        Arc::clone(&budget),
        cap,
        Arc::clone(&seen),
        Arc::clone(served),
    ));
    if search {
        registry.register(SearchAtHead::new(gateway, budget, cap, seen));
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_served() -> Arc<std::sync::Mutex<crate::contracts::ServedReads>> {
        Arc::new(std::sync::Mutex::new(crate::contracts::ServedReads::new()))
    }
    use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, SearchHit};
    use crate::error::DtError;

    struct FakeGateway {
        content: String,
    }

    impl PrGateway for FakeGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
            Ok(FileSnapshot {
                path: path.to_owned(),
                content: self.content.clone(),
            })
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(Vec::new())
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            Ok(vec![SearchHit {
                file: "src/a.rs".to_owned(),
                line: 2,
                text: "fn target() {}".to_owned(),
            }])
        }
    }

    struct FailingReadGateway;

    impl PrGateway for FailingReadGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
            Err(DtError::Gateway(format!("no file {path:?} at head")))
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(Vec::new())
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            Ok(Vec::new())
        }
    }

    fn big_content() -> String {
        (1..=1000)
            .map(|line| format!("L{line}Q"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn read_text(tool: &ReadFileAtHead, input: Value, context: &ToolContext) -> String {
        tool.call(input, context)
            .await
            .as_ref()
            .map_or(String::new(), ToolOutput::text_content)
    }

    #[tokio::test]
    async fn a_default_read_pages_large_files_instead_of_shipping_them_whole() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: big_content(),
        });
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let viewed = read_text(&tool, json!({"path": "src/big.rs"}), &context).await;
        assert!(
            viewed.contains("src/big.rs") && viewed.contains("L1Q"),
            "the view is anchored to its path and starts at the first line: {viewed}"
        );
        assert!(
            viewed.matches('Q').count() <= 400,
            "one default view carries at most the 400-line window, never the whole file"
        );
        assert!(
            !viewed.contains("L401Q"),
            "the second window does not ride the first read: {viewed}"
        );
    }

    #[tokio::test]
    async fn an_offset_read_continues_where_the_default_window_stopped() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: big_content(),
        });
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let page = read_text(
            &tool,
            json!({"path": "src/big.rs", "offset": 401}),
            &context,
        )
        .await;
        assert!(
            page.contains("L401Q") && !page.contains("L1Q"),
            "the offset window starts where it was asked to, without re-shipping the first page: {page}"
        );
    }

    #[tokio::test]
    async fn repeat_detection_keys_on_the_window_not_the_path() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: big_content(),
        });
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let first = read_text(&tool, json!({"path": "src/big.rs", "offset": 1}), &context).await;
        let second = read_text(&tool, json!({"path": "src/big.rs", "offset": 2}), &context).await;
        assert!(
            first.contains("L1Q") && second.contains("L2Q"),
            "different windows of one file are fresh reads, never the notice"
        );
        let repeat = read_text(&tool, json!({"path": "src/big.rs", "offset": 1}), &context).await;
        assert!(
            repeat.contains("ask #2 of src/big.rs")
                && repeat.contains("re-examine the content already above"),
            "the identical window repeats into the notice: {repeat}"
        );
    }

    #[tokio::test]
    async fn a_served_read_records_the_window_the_judge_saw() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: big_content(),
        });
        let served = test_served();
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            Arc::clone(&served),
        );
        let context = ToolContext::default();
        let first = read_text(&tool, json!({"path": "src/big.rs"}), &context).await;
        assert!(
            first.matches('Q').count() >= 1,
            "the view carries numbered lines: {first}"
        );
        let page = read_text(
            &tool,
            json!({"path": "src/big.rs", "offset": 401}),
            &context,
        )
        .await;
        let log = served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let windows = log.get("src/big.rs").cloned().unwrap_or_default();
        assert_eq!(
            windows.len(),
            2,
            "one window per served view, repeats included until merged by the authority: {windows:?}"
        );
        assert!(
            windows
                .first()
                .is_some_and(|(lo, hi)| *lo == 1 && *hi == 400),
            "the default view records its own first and last served line, line one included: {windows:?}"
        );
        assert!(
            windows
                .get(1)
                .is_some_and(|(lo, hi)| *lo == 401 && *hi > 401),
            "the offset view records the window it actually served: {windows:?}"
        );
        let _ = page;
    }

    #[test]
    fn the_served_window_parser_reads_only_numbered_lines() {
        assert_eq!(
            served_window("     1\tfn a() {}\n     2\tfn b() {}"),
            Some((1, 2)),
            "a whole-file view carries no header: numbered line one is the first line"
        );
        assert_eq!(
            served_window(
                "[Lines before offset 399 omitted — use offset=1 to read from the start]\n\n  399\tlet x = 1;\n... [1 line truncated] ...\n  401\tlet y = 2;"
            ),
            Some((399, 401)),
            "the offset header and marker lines never break the span"
        );
        assert_eq!(
            served_window("src/a.rs\nno numbers here"),
            None,
            "a view without numbered lines records nothing rather than guessing"
        );
    }

    #[tokio::test]
    async fn a_failed_pinned_read_surfaces_as_a_tool_error() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FailingReadGateway);
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let outcome = tool.call(json!({"path": "src/gone.rs"}), &context).await;
        assert!(
            matches!(&outcome, Err(err) if err.to_string().contains("src/gone.rs")),
            "a gateway read failure reaches the model as an error naming the path: {outcome:?}"
        );
    }

    #[test]
    fn the_read_schema_advertises_the_paging_fields_under_the_pinned_name() -> Result<(), DtError> {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: String::new(),
        });
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let schema = tool.schema();
        assert_eq!(
            schema.tool, "read_file_at_head",
            "the pinned-head name stays the registered one"
        );
        let props = schema
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| DtError::Gateway("the schema carries no properties".to_owned()))?;
        for field in ["path", "offset", "limit", "line_range"] {
            assert!(
                props.keys().any(|name| name.as_str() == field),
                "the {field} paging field is advertised"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn tool_calls_beyond_the_cap_are_refused() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: "fn a() {}".to_owned(),
        });
        let tool = ReadFileAtHead::new(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            3,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let first = tool.call(json!({"path": "src/a.rs"}), &context).await;
        assert!(
            matches!(&first, Ok(output) if output.text_content().contains("fn a() {}")),
            "the first call reads: {first:?}"
        );
        let _unused = tool.call(json!({"path": "src/b.rs"}), &context).await;
        let _unused = tool.call(json!({"path": "src/c.rs"}), &context).await;
        let fourth = tool.call(json!({"path": "src/d.rs"}), &context).await;
        assert!(
            matches!(&fourth, Ok(output) if output.text_content().contains("tool budget of 3 calls is exhausted")),
            "the fourth call is a refusal naming the budget: {fourth:?}"
        );
    }

    #[tokio::test]
    async fn post_budget_refusals_for_different_targets_never_share_text() {
        // The detector's triple degenerates to (tool, result-text)
        // here, so every refusal must name its target.
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: "fn a() {}".to_owned(),
        });
        let tool = ReadFileAtHead::new(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            1,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let _unused = tool.call(json!({"path": "src/first.rs"}), &context).await;
        let second = tool.call(json!({"path": "src/second.rs"}), &context).await;
        let third = tool.call(json!({"path": "src/third.rs"}), &context).await;
        let second_text = second
            .as_ref()
            .map_or(String::new(), ToolOutput::text_content);
        let third_text = third
            .as_ref()
            .map_or(String::new(), ToolOutput::text_content);
        assert!(
            second_text.contains("refused: src/second.rs"),
            "the refusal names its target: {second_text}"
        );
        assert_ne!(
            second_text, third_text,
            "refusals for different targets never share text — the generic \n             signature cannot fold them into one repetition triple: \n             {second_text} vs {third_text}"
        );
    }

    #[tokio::test]
    async fn empty_searches_for_different_patterns_never_share_text() {
        // A bare "no matches" for three patterns is a detector triple;
        // each empty answer names its pattern.
        let gateway: Arc<dyn PrGateway> = Arc::new(EmptySearchGateway);
        let tool = SearchAtHead::new(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
        );
        let context = ToolContext::default();
        let first = tool
            .call(json!({"pattern": "alpha"}), &context)
            .await
            .as_ref()
            .map_or(String::new(), ToolOutput::text_content);
        let second = tool
            .call(json!({"pattern": "beta"}), &context)
            .await
            .as_ref()
            .map_or(String::new(), ToolOutput::text_content);
        assert_eq!(
            first, "no matches for alpha",
            "the answer names its pattern"
        );
        assert_ne!(
            first, second,
            "empty answers for different patterns never share text"
        );
    }

    #[tokio::test]
    async fn a_repeat_read_answers_with_a_pointer_not_the_bytes() {
        // A repeat must answer with the notice, never the same bytes,
        // or the detector's (tool, param, result) triple re-forms.
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: "fn a() {}".to_owned(),
        });
        let tool = ReadFileAtHead::new(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let input = json!({"path": "src/a.rs"});
        let mut answers: Vec<String> = Vec::new();
        for _ask in 0..5 {
            let result = tool.call(input.clone(), &context).await;
            answers.push(
                result
                    .as_ref()
                    .map_or(String::new(), ToolOutput::text_content),
            );
        }
        let first_answer = answers.first().map(String::as_str).unwrap_or_default();
        assert!(
            first_answer.contains("src/a.rs") && first_answer.contains("fn a() {}"),
            "the first call reads through the pinned window: {first_answer}"
        );
        for (index, pair) in answers.windows(2).enumerate() {
            let left = pair.first().map(String::as_str).unwrap_or_default();
            let right = pair.last().map(String::as_str).unwrap_or_default();
            assert_ne!(
                left, right,
                "no two consecutive answers share text — the generic \
                 signature cannot fold them into a repetition triple: \
                 {left} then {right}"
            );
            if index >= 1 {
                assert!(
                    right.contains(&format!("ask #{}", index.saturating_add(2))),
                    "each repeat names its ask count: {right}"
                );
            }
        }
        assert!(
            answers
                .iter()
                .skip(1)
                .all(|answer| answer.contains("re-examine the content already above")),
            "every repeat points back at the content in context"
        );
        let other = tool.call(json!({"path": "src/other.rs"}), &context).await;
        assert!(
            matches!(&other, Ok(output) if output.text_content().contains("src/other.rs")
                && output.text_content().contains("fn a() {}")
                && !output.text_content().contains("unchanged")),
            "a different path is a fresh call, never the notice: {other:?}"
        );
    }

    struct EmptySearchGateway;

    impl PrGateway for EmptySearchGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, _path: &str) -> Result<FileSnapshot, DtError> {
            Ok(FileSnapshot {
                path: String::new(),
                content: String::new(),
            })
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(Vec::new())
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn a_repeat_search_answers_with_the_notice() {
        // Identical patterns re-answer with the notice so the result
        // hash differs from the first answer.
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: String::new(),
        });
        let tool = SearchAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
        );
        let context = ToolContext::default();
        let input = json!({"pattern": "fn target("});
        let first = tool.call(input.clone(), &context).await;
        let second = tool.call(input, &context).await;
        assert!(
            matches!(&first, Ok(output) if output.text_content().contains("src/a.rs:2")),
            "the first call searches: {first:?}"
        );
        assert!(
            matches!(&second, Ok(output) if output.text_content() == unchanged_notice(2, "fn target(")),
            "the repeat answers with the notice: {second:?}"
        );
    }

    #[tokio::test]
    async fn repeat_notices_for_different_targets_never_share_text() {
        // Distinct targets asked once each: the notice must name the
        // target or the (tool, result-text) signature folds them.
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: "fn a() {}".to_owned(),
        });
        let tool = ReadFileAtHead::new(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::new(SeenCalls::default()),
            test_served(),
        );
        let context = ToolContext::default();
        let paths = ["src/a.rs", "src/b.rs", "src/c.rs"];
        let mut first_round: Vec<String> = Vec::new();
        for path in paths {
            let result = tool.call(json!({"path": path}), &context).await;
            first_round.push(
                result
                    .as_ref()
                    .map_or(String::new(), ToolOutput::text_content),
            );
        }
        for (path, first) in paths.iter().zip(first_round.iter()) {
            assert!(
                first.contains(path) && first.contains("fn a() {}"),
                "the first ask of {path} reads the content under its own anchor: {first}"
            );
        }
        for (index, left) in first_round.iter().enumerate() {
            for right in first_round.iter().skip(index.saturating_add(1)) {
                assert_ne!(
                    left, right,
                    "first asks of different paths never share text — the path anchor keeps them distinct"
                );
            }
        }
        let mut second_round: Vec<String> = Vec::new();
        for path in paths {
            let result = tool.call(json!({"path": path}), &context).await;
            second_round.push(
                result
                    .as_ref()
                    .map_or(String::new(), ToolOutput::text_content),
            );
        }
        for (path, notice) in paths.iter().zip(second_round.iter()) {
            assert!(
                notice.contains(&format!("ask #2 of {path}")),
                "the repeat for {path} names its own target: {notice}"
            );
        }
        for (index, left) in second_round.iter().enumerate() {
            for right in second_round.iter().skip(index.saturating_add(1)) {
                assert_ne!(
                    left, right,
                    "second asks of different targets never share text — the \
                     generic tool signature cannot fold them into one \
                     repetition triple"
                );
            }
        }
    }

    #[test]
    fn the_search_tool_is_absent_when_disabled() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: String::new(),
        });
        let served = test_served();
        let read_only = region_registry(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            10,
            false,
            &served,
        );
        assert_eq!(
            read_only
                .all_schemas()
                .iter()
                .map(|schema| schema.tool.as_str())
                .collect::<Vec<_>>(),
            vec!["read_file_at_head"],
            "a disabled search tool is never advertised"
        );
        let both = region_registry(
            gateway,
            Arc::new(AtomicUsize::new(0)),
            10,
            true,
            &test_served(),
        );
        assert_eq!(both.all_schemas().len(), 2);
    }
}
