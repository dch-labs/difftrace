//! Gateway-backed judge tools under one per-region call budget.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use loopctl::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSchema};
use serde_json::{Value, json};

use crate::gateway::PrGateway;

pub const SEARCH_CAP: usize = 20;

pub struct ReadFileAtHead {
    gateway: Arc<dyn PrGateway>,
    budget: Arc<AtomicUsize>,
    cap: usize,
}

pub struct SearchAtHead {
    gateway: Arc<dyn PrGateway>,
    budget: Arc<AtomicUsize>,
    cap: usize,
}

impl ReadFileAtHead {
    pub fn new(gateway: Arc<dyn PrGateway>, budget: Arc<AtomicUsize>, cap: usize) -> Self {
        Self {
            gateway,
            budget,
            cap,
        }
    }
}

impl SearchAtHead {
    pub fn new(gateway: Arc<dyn PrGateway>, budget: Arc<AtomicUsize>, cap: usize) -> Self {
        Self {
            gateway,
            budget,
            cap,
        }
    }
}

fn spend(budget: &AtomicUsize, cap: usize) -> Option<String> {
    let used = budget.fetch_add(1, Ordering::Relaxed);
    if used >= cap {
        return Some(format!(
            "the region tool budget of {cap} calls is exhausted; finish and return the report"
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
        "Read one file's full content at the pinned head commit; input {\"path\": \"src/a.rs\"}"
    }

    fn schema(&self) -> ToolSchema {
        read_schema(self.name(), self.description(), "path")
    }

    fn call(
        &self,
        input: Value,
        _context: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        let gateway = Arc::clone(&self.gateway);
        let budget = Arc::clone(&self.budget);
        let cap = self.cap;
        Box::pin(async move {
            if let Some(refusal) = spend(&budget, cap) {
                return Ok(ToolOutput::success(refusal));
            }
            let Some(path) = input.get("path").and_then(Value::as_str) else {
                return Ok(ToolOutput::success(
                    "expected {\"path\": \"src/a.rs\"} as the input".to_owned(),
                ));
            };
            match gateway.read_file_at_head(path) {
                Ok(snapshot) => Ok(ToolOutput::success(snapshot.content)),
                Err(err) => Ok(ToolOutput::success(format!("read failed: {err}"))),
            }
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
        Box::pin(async move {
            if let Some(refusal) = spend(&budget, cap) {
                return Ok(ToolOutput::success(refusal));
            }
            let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
                return Ok(ToolOutput::success(
                    "expected {\"pattern\": \"fn target(\"} as the input".to_owned(),
                ));
            };
            match gateway.search_at_head(pattern, SEARCH_CAP) {
                Ok(hits) if hits.is_empty() => Ok(ToolOutput::success("no matches".to_owned())),
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
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(ReadFileAtHead::new(
        Arc::clone(&gateway),
        Arc::clone(&budget),
        cap,
    ));
    if search {
        registry.register(SearchAtHead::new(gateway, budget, cap));
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn tool_calls_beyond_the_cap_are_refused() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: "fn a() {}".to_owned(),
        });
        let tool = ReadFileAtHead::new(Arc::clone(&gateway), Arc::new(AtomicUsize::new(0)), 2);
        let context = ToolContext::default();
        let input = json!({"path": "src/a.rs"});
        let first = tool.call(input.clone(), &context).await;
        assert!(
            matches!(&first, Ok(output) if output.text_content().contains("fn a() {}")),
            "the first call reads: {first:?}"
        );
        let _unused = tool.call(input.clone(), &context).await;
        let third = tool.call(input, &context).await;
        assert!(
            matches!(&third, Ok(output) if output.text_content().contains("tool budget of 2 calls is exhausted")),
            "the third call is a refusal naming the budget: {third:?}"
        );
    }

    #[test]
    fn the_search_tool_is_absent_when_disabled() {
        let gateway: Arc<dyn PrGateway> = Arc::new(FakeGateway {
            content: String::new(),
        });
        let read_only = region_registry(
            Arc::clone(&gateway),
            Arc::new(AtomicUsize::new(0)),
            10,
            false,
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
        let both = region_registry(gateway, Arc::new(AtomicUsize::new(0)), 10, true);
        assert_eq!(both.all_schemas().len(), 2);
    }
}
