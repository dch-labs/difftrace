//! The chat render: the question, an optional reject confirmation, and
//! the answer as one inert block — model-controlled text never renders
//! live markdown.

use super::{escape_inline, neutralize_mentions};

#[must_use]
pub fn render_answer(question: &str, answer: &str, confirmation: Option<&str>) -> String {
    let mut parts: Vec<String> = vec![
        "## difftrace reply".to_owned(),
        format!("question: {}", escape_inline(question)),
    ];
    if let Some(line) = confirmation {
        parts.push(escape_inline(line));
    }
    parts.push(escape_inline(answer));
    neutralize_mentions(&format!("{}\n", parts.join("\n\n")))
}
