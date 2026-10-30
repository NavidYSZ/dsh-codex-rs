//! Live opt-in provider probe. Read [{"provider","config","key"}] from stdin;
//! never print keys, prompts, reasoning or raw responses. Each selected model
//! calls one in-memory echo tool and reads its result in a second request.
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dshx_core::config::ProviderConfig;
use dshx_core::{ContentBlock, FinishReason, GenerateRequest, LlmClient, Message, ToolSchema};
use dshx_llm::{BlockAssembler, ProviderClient, ProviderSpec, StaticApiKey};
use futures::{StreamExt, stream};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Input {
    provider: String,
    config: ProviderConfig,
    key: String,
}

async fn probe(client: &ProviderClient, model: &str) -> anyhow::Result<serde_json::Value> {
    let mut request = GenerateRequest {
        model: model.into(),
        reasoning_effort: Some("low".into()),
        messages: vec![
            Message::system(
                "Call echo_probe exactly once with value DSH_PROVIDER_OK. After receiving its result, answer only DSH_PROVIDER_OK. This is a tool protocol test; do not explain it.",
            ),
            Message::user_text("Run the protocol test."),
        ],
        tools: vec![ToolSchema {
            name: "echo_probe".into(),
            description: "Echo the value unchanged; this test runs entirely in memory.".into(),
            parameters: json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
        }],
        max_tokens: Some(4096),
        temperature: None,
        session_id: None,
    };
    let mut usages = vec![];
    for turn in 0..2 {
        let mut chunks = client.stream(request.clone(), Default::default()).await;
        let mut assembled = BlockAssembler::new();
        while let Some(chunk) = chunks.next().await {
            assembled.push(&chunk);
        }
        anyhow::ensure!(
            !matches!(assembled.finish(), FinishReason::Error { .. }),
            "{:?}",
            assembled.finish()
        );
        usages.push(assembled.usage().cloned());
        let message = assembled.message(&client.spec().id, model);
        let calls: Vec<_> = message.tool_calls().collect();
        if turn == 0 {
            anyhow::ensure!(
                calls.len() == 1 && calls[0].1 == "echo_probe",
                "expected one echo_probe tool call; finish={:?}, calls={}",
                assembled.finish(),
                calls.len()
            );
            let arguments: serde_json::Value = serde_json::from_str(calls[0].2)?;
            anyhow::ensure!(
                arguments["value"] == "DSH_PROVIDER_OK",
                "wrong tool arguments"
            );
            let result = Message::tool_result(
                calls[0].0,
                vec![ContentBlock::text("DSH_PROVIDER_OK")],
                false,
            );
            request.messages.push(message);
            request.messages.push(result);
        } else {
            anyhow::ensure!(
                calls.is_empty() && message.plain_text().trim() == "DSH_PROVIDER_OK",
                "expected final marker; finish={:?}, calls={}",
                assembled.finish(),
                calls.len()
            );
        }
    }
    Ok(json!({"toolCalls":1,"requests":2,"usage":usages}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    let inputs: Vec<Input> = serde_json::from_str(&text)?;
    let failures: Vec<usize> = stream::iter(inputs).map(|input| async move {
        let spec = ProviderSpec::resolve(&input.provider, &input.config).unwrap();
        let models: Vec<String> = spec.models.iter().map(|m| m.id.clone()).collect();
        let client = ProviderClient::new(spec, Arc::new(StaticApiKey(input.key.clone())), None, None).unwrap();
        let mut failures = 0;
        for model in models {
            let start = Instant::now();
            let outcome = tokio::time::timeout(Duration::from_secs(90), probe(&client, &model)).await;
            let mut result = json!({"provider": input.provider, "model":model,"seconds":start.elapsed().as_secs_f64()});
            match outcome {
                Ok(Ok(details)) => { result["ok"] = true.into(); result["details"] = details; }
                error => {
                    failures += 1;
                    result["ok"] = false.into();
                    result["error"] = format!("{error:?}").replace(&input.key, "[redacted]").into();
                }
            }
            println!("{result}");
        }
        failures
    }).buffer_unordered(5).collect().await;
    anyhow::ensure!(
        failures.iter().sum::<usize>() == 0,
        "one or more provider probes failed"
    );
    Ok(())
}
