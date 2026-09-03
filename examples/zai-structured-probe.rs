//! Probe: run the exact summary-pass request shape against a real
//! Anthropic-protocol endpoint and print what actually comes back.
//!
//! Usage:
//!   `ZAI_API_KEY=… cargo run --all-features --example zai-structured-probe`

use loopctl::api::ApiClient;
use loopctl::api::StreamRequest;
use loopctl::message::Message;
use loopctl::structured::RequestOptions;
use loopctl::structured::ResponseFormat;
use loopctl::structured::StructuredOutput;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("ZAI_API_KEY")
        .or_else(|_| std::env::var("ZHIPUAI_API_KEY"))
        .map_err(|_| "set ZAI_API_KEY")?;
    let client = loopctl::provider::zai_builder().with_api_key(key).build()?;

    let mut response_format = ResponseFormat::from_type::<difftrace::findings::ReviewSummary>();
    response_format.strict = false;
    let options = RequestOptions::new().with_response_format(response_format);

    let system = "You are writing the summary of a completed code review. Given every finding \
the review recorded, write the summary body, the risk notes (one per line), and one sentence on \
test coverage.";
    let user = "The review recorded these findings:\n\
[{\"file\":\"src/lib.rs\",\"line\":3,\"severity\":\"warning\",\"title\":\"Lock dropped\",\
\"body\":\"The guard is dropped before the read completes.\"}]\n\n\
Write the review summary.";
    let request = StreamRequest {
        messages: vec![Message::user(user)],
        system: Some(system.to_owned()),
        tools: None,
    };

    let response = client
        .create_message_with_options(&request, options)
        .await?;
    println!("== raw message ==\n{:#?}", response.message);
    let extracted = client.extract_structured(&response.message);
    println!("\n== extracted value ==\n{extracted:#?}");
    match difftrace::findings::ReviewSummary::from_value(extracted) {
        Ok(summary) => println!("\n== parsed ==\n{summary:#?}"),
        Err(err) => println!("\n== parse failed ==\n{err}"),
    }
    Ok(())
}
