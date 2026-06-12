//! Integration tests for nusy-llm-backend.
//!
//! Uses wiremock to simulate API responses from Claude backend,
//! verifying trait compliance and error handling without real API access.
//! EX-3435: Ollama tests removed — Candle is the only LLM path.

use nusy_llm_backend::{ClaudeBackend, LlmClient, LlmError, LlmParams, MockLlmBackend};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Claude integration tests ──────────────────────────────────────────────

#[tokio::test]
async fn claude_complete_success() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
        "content": [{"type": "text", "text": "Paris is the capital of France."}],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 15, "output_tokens": 8}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("claude-sonnet-4-20250514", "test-key", server.uri());
    let result = backend
        .complete("What is the capital of France?", &LlmParams::default())
        .await
        .unwrap();

    assert_eq!(result, "Paris is the capital of France.");
}

#[tokio::test]
async fn claude_complete_with_system_prompt() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
        "content": [{"type": "text", "text": "42"}],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 20, "output_tokens": 1}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("claude-sonnet-4-20250514", "test-key", server.uri());
    let params = LlmParams::default()
        .with_system_prompt("Answer with just a number.")
        .with_max_tokens(10);

    let result = backend.complete("What is 6 * 7?", &params).await.unwrap();
    assert_eq!(result, "42");
}

#[tokio::test]
async fn claude_rate_limited() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).append_header("retry-after", "30"))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let err = backend
        .complete("test", &LlmParams::default())
        .await
        .unwrap_err();

    match err {
        LlmError::RateLimited { retry_after_secs } => {
            assert_eq!(retry_after_secs, Some(30));
        }
        other => panic!("Expected RateLimited, got: {other:?}"),
    }
}

#[tokio::test]
async fn claude_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error": "invalid_request"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let err = backend
        .complete("test", &LlmParams::default())
        .await
        .unwrap_err();

    match err {
        LlmError::Api { status, message } => {
            assert_eq!(status, 400);
            assert!(message.contains("invalid_request"));
        }
        other => panic!("Expected Api error, got: {other:?}"),
    }
}

#[tokio::test]
async fn claude_empty_response() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
        "content": [],
        "model": "model",
        "stop_reason": null,
        "usage": {"input_tokens": 0, "output_tokens": 0}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let err = backend
        .complete("test", &LlmParams::default())
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::EmptyResponse));
}

#[tokio::test]
async fn claude_stream_returns_single_chunk() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
        "content": [{"type": "text", "text": "Streamed response content"}],
        "model": "model",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let chunks = backend.stream("test", &LlmParams::default()).await.unwrap();

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "Streamed response content");
}

#[tokio::test]
async fn claude_invalid_json_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let err = backend
        .complete("test", &LlmParams::default())
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::Parse(_)));
}

// ── Trait compliance tests ────────────────────────────────────────────────

#[tokio::test]
async fn all_backends_implement_llm_client() {
    fn assert_impl<T: LlmClient>() {}
    assert_impl::<MockLlmBackend>();
    assert_impl::<ClaudeBackend>();
}

#[tokio::test]
async fn mock_backend_trait_compliance() {
    let mock = MockLlmBackend::new();
    let params = LlmParams::default();

    // complete works
    let result = mock.complete("hello", &params).await.unwrap();
    assert!(!result.is_empty());

    // stream works
    let chunks = mock.stream("hello world", &params).await.unwrap();
    assert!(!chunks.is_empty());
}

#[tokio::test]
async fn claude_backend_trait_compliance() {
    let server = MockServer::start().await;

    let response_body = serde_json::json!({
        "content": [{"type": "text", "text": "response"}],
        "model": "model",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(2) // complete + stream both hit the endpoint
        .mount(&server)
        .await;

    let backend = ClaudeBackend::with_config("model", "key", server.uri());
    let params = LlmParams::default();

    // complete works
    let result = backend.complete("test", &params).await.unwrap();
    assert_eq!(result, "response");

    // stream works
    let chunks = backend.stream("test", &params).await.unwrap();
    assert_eq!(chunks, vec!["response"]);
}
