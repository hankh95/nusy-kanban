//! Integration tests for the OpenAI/vLLM backend (CH-4673).
//!
//! Two layers:
//! - `wiremock`-mocked tests that run everywhere (CI included): they assert the
//!   backend speaks the OpenAI `/v1/chat/completions` protocol correctly — request
//!   shape, response parsing, error mapping — without a GPU or a live server.
//! - A **gated live test** (`live_vllm_round_trip`) that hits a real vLLM only when
//!   `VLLM_BASE_URL` is set, so the on-DGX end-to-end path can be exercised on demand
//!   (`VLLM_BASE_URL=http://localhost:8000/v1 VLLM_MODEL=Qwen/Qwen3-4B cargo test -p
//!   nusy-llm-backend --test vllm_integration -- --ignored live`).

use nusy_llm_backend::{LlmClient, LlmError, LlmParams, OpenAiBackend};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_response(content: &str, finish_reason: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "Qwen/Qwen3-4B",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason
        }],
        "usage": { "prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19 }
    })
}

#[tokio::test]
async fn complete_round_trips_against_a_mock_vllm() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        // Verifies the backend sends the served model name and the user prompt.
        // (`stream: false` is omitted from the body by design — skip_serializing_if —
        // so we don't match on it.)
        .and(body_partial_json(json!({
            "model": "Qwen/Qwen3-4B",
            "messages": [{ "role": "user", "content": "extract rules" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "[{\"label\":\"r1\",\"if\":\"age>=60\",\"then\":\"target 150/90\"}]",
            "stop",
        )))
        .mount(&server)
        .await;

    let backend = OpenAiBackend::with_config("Qwen/Qwen3-4B", format!("{}/v1", server.uri()), None);
    let out = backend
        .complete("extract rules", &LlmParams::default())
        .await
        .expect("mock vLLM completes");
    assert!(out.contains("\"label\":\"r1\""));
}

#[tokio::test]
async fn system_prompt_becomes_a_system_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "messages": [
                { "role": "system", "content": "You extract if/then rules." },
                { "role": "user", "content": "go" }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("ok", "stop")))
        .mount(&server)
        .await;

    let backend = OpenAiBackend::with_config("Qwen/Qwen3-4B", format!("{}/v1", server.uri()), None);
    let params = LlmParams::default().with_system_prompt("You extract if/then rules.");
    let out = backend.complete("go", &params).await.expect("completes");
    assert_eq!(out, "ok");
}

#[tokio::test]
async fn bearer_key_is_sent_when_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("authed", "stop")))
        .mount(&server)
        .await;

    let backend = OpenAiBackend::with_config(
        "Qwen/Qwen3-4B",
        format!("{}/v1", server.uri()),
        Some("sk-test".into()),
    );
    let out = backend
        .complete("x", &LlmParams::default())
        .await
        .expect("authed");
    assert_eq!(out, "authed");
}

#[tokio::test]
async fn finish_reason_length_surfaces_truncation_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_response("{\"partial", "length")),
        )
        .mount(&server)
        .await;

    let backend = OpenAiBackend::with_config("Qwen/Qwen3-4B", format!("{}/v1", server.uri()), None);
    let err = backend
        .complete("x", &LlmParams::default().with_max_tokens(8))
        .await
        .expect_err("truncation must surface");
    assert!(matches!(
        err,
        LlmError::TruncatedOutput { max_tokens: 8, .. }
    ));
}

#[tokio::test]
async fn http_500_maps_to_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("engine died"))
        .mount(&server)
        .await;

    let backend = OpenAiBackend::with_config("Qwen/Qwen3-4B", format!("{}/v1", server.uri()), None);
    let err = backend
        .complete("x", &LlmParams::default())
        .await
        .expect_err("5xx must error");
    assert!(matches!(err, LlmError::Api { status: 500, .. }));
}

/// Live end-to-end against a real vLLM. Skipped unless `VLLM_BASE_URL` is set, so
/// CI and non-GPU machines stay green; run on DGX with a started sidecar.
#[tokio::test]
#[ignore = "requires a running vLLM server (set VLLM_BASE_URL)"]
async fn live_vllm_round_trip() {
    let Ok(base_url) = std::env::var("VLLM_BASE_URL") else {
        eprintln!("VLLM_BASE_URL unset — skipping live vLLM test");
        return;
    };
    let model = std::env::var("VLLM_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-4B".into());
    let backend =
        OpenAiBackend::with_config(&model, base_url, std::env::var("OPENAI_API_KEY").ok())
            .with_disable_thinking(true);
    let params = LlmParams::default()
        .with_max_tokens(64)
        .with_temperature(0.0)
        .with_system_prompt("Answer with a single word.");
    let out = backend
        .complete("What is the capital of France?", &params)
        .await
        .expect("live vLLM completes");
    assert!(!out.is_empty(), "live vLLM returned empty");
    eprintln!("live vLLM ({model}) -> {out:?}");
}
