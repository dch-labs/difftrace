//! The review tools: the agent's loopctl `Tool` surface over the
//! `PrGateway` seam and the diff index. `submit_review` is the only
//! write-class tool, and it grounds every finding through
//! `DiffIndex::clamp_to_hunk` before anything is posted.

pub mod comments;
pub mod file_diff;
pub mod overview;
pub mod read_file;
pub mod submit;

#[cfg(test)]
pub(crate) mod fake_gateway;

use std::sync::Arc;

use loopctl::tool::ToolRegistry;

use crate::diff::DiffIndex;
use crate::github::PrGateway;

pub struct ReviewScope {
    pub gateway: Arc<dyn PrGateway>,
    pub index: Arc<DiffIndex>,
    pub pr: u64,
    pub head_sha: String,
}

impl ReviewScope {
    #[must_use]
    pub fn new(
        gateway: Arc<dyn PrGateway>,
        index: Arc<DiffIndex>,
        pr: u64,
        head_sha: impl Into<String>,
    ) -> Self {
        Self {
            gateway,
            index,
            pr,
            head_sha: head_sha.into(),
        }
    }

    #[must_use]
    pub fn batch_registry(
        &self,
        record: crate::review::record::FindingsSlot,
        max_findings_per_file: usize,
    ) -> ToolRegistry {
        let scope = Arc::new(Self {
            gateway: Arc::clone(&self.gateway),
            index: Arc::clone(&self.index),
            pr: self.pr,
            head_sha: self.head_sha.clone(),
        });
        let mut registry = ToolRegistry::new();
        registry.register(overview::OverviewTool::new(Arc::clone(&scope)));
        registry.register(file_diff::FileDiffTool::new(Arc::clone(&scope)));
        registry.register(read_file::ReadFileTool::new(Arc::clone(&scope)));
        registry.register(comments::ListCommentsTool::new(Arc::clone(&scope)));
        registry.register(crate::review::RecordFindingsTool::new(
            record,
            max_findings_per_file,
        ));
        registry
    }

    #[must_use]
    pub fn registry(&self, max_findings_per_file: usize) -> ToolRegistry {
        let scope = Arc::new(Self {
            gateway: Arc::clone(&self.gateway),
            index: Arc::clone(&self.index),
            pr: self.pr,
            head_sha: self.head_sha.clone(),
        });
        let mut registry = ToolRegistry::new();
        registry.register(overview::OverviewTool::new(Arc::clone(&scope)));
        registry.register(file_diff::FileDiffTool::new(Arc::clone(&scope)));
        registry.register(read_file::ReadFileTool::new(Arc::clone(&scope)));
        registry.register(comments::ListCommentsTool::new(Arc::clone(&scope)));
        registry.register(submit::SubmitReviewTool::new(scope, max_findings_per_file));
        registry
    }

    #[must_use]
    pub fn chat_registry(&self) -> ToolRegistry {
        let scope = Arc::new(Self {
            gateway: Arc::clone(&self.gateway),
            index: Arc::clone(&self.index),
            pr: self.pr,
            head_sha: self.head_sha.clone(),
        });
        let mut registry = ToolRegistry::new();
        registry.register(overview::OverviewTool::new(Arc::clone(&scope)));
        registry.register(file_diff::FileDiffTool::new(Arc::clone(&scope)));
        registry.register(read_file::ReadFileTool::new(scope));
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::fake_gateway::FakeGateway;

    #[test]
    fn the_registry_carries_the_five_review_tools() {
        let scope = ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::empty()),
            1,
            "h",
        );
        let registry = scope.registry(5);
        let names: Vec<&str> = registry
            .all_tools()
            .iter()
            .map(|tool| tool.name())
            .collect();
        assert_eq!(
            names,
            vec![
                "get_pr_overview",
                "get_file_diff",
                "read_file_at_head",
                "list_review_comments",
                "submit_review"
            ]
        );
    }
}
