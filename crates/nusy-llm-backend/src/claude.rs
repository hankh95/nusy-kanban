//! Claude backend — Anthropic Messages API client.
//!
//! EX-3127 Phase 2: Implements `LlmClient` for Claude models via the
//! Anthropic Messages API (https://docs.anthropic.com/en/api/messages).

use crate::error::{LlmError, Result};
use crate::{LlmClient, LlmParams};
use serde::{Deserialize, Serialize};

/// Default Anthropic API base URL.
const DEFAULT_API_URL: &str = "https://api.anthropic.com";

/// Current Anthropic API version header value.
const API_VERSION: &str = "2023-06-01";

/// Claude backend using the Anthropic Messages API.
pub struct ClaudeBackend {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
    model: String,
}

impl ClaudeBackend {
    /// Create a new Claude backend.
    ///
    /// Reads `ANTHROPIC_API_KEY` from the environment. Returns `LlmError::Config`
    /// if the key is not set.
    ///
    /// # Arguments
    /// * `model` - Model ID (e.g., "claude-sonnet-4-20250514")
    pub fn new(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| LlmError::Config("ANTHROPIC_API_KEY not set".into()))?;

        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            api_url: DEFAULT_API_URL.into(),
            model: model.into(),
        })
    }

    /// Create with an explicit API key and optional custom base URL.
    ///
    /// Useful for testing with mock servers.
    pub fn with_config(
        model: impl Into<String>,
        api_key: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            api_url: api_url.into(),
            model: model.into(),
        }
    }

    /// Build the Messages API request body.
    fn build_request(&self, prompt: &str, params: &LlmParams) -> MessagesRequest {
        let mut request = MessagesRequest {
            model: self.model.clone(),
            max_tokens: params.max_tokens,
            messages: vec![Message {
                role: "user".into(),
                content: prompt.into(),
            }],
            system: params.effective_system_prompt(),
            temperature: params.temperature,
            stop_sequences: if params.stop_sequences.is_empty() {
                None
            } else {
                Some(params.stop_sequences.clone())
            },
            stream: false,
        };

        // Clamp temperature to Anthropic's valid range [0.0, 1.0].
        if let Some(t) = request.temperature {
            request.temperature = Some(t.clamp(0.0, 1.0));
        }

        request
    }

    /// Send a request to the Messages API and parse the response.
    async fn send_request(&self, request: &MessagesRequest) -> Result<MessagesResponse> {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.api_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(request)
            .send()
            .await?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            return Err(LlmError::RateLimited {
                retry_after_secs: retry_after,
            });
        }

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let body = response.text().await?;
        serde_json::from_str::<MessagesResponse>(&body)
            .map_err(|e| LlmError::Parse(format!("Failed to parse response: {e}: {body}")))
    }

    /// CH-4406 — convert `stop_reason == "max_tokens"` into a structured
    /// [`LlmError::TruncatedOutput`] error so callers can branch on the
    /// "raise the cap" signal instead of seeing the truncation as a
    /// downstream JSON parse failure.
    ///
    /// Other Anthropic stop reasons (`end_turn`, `stop_sequence`, `tool_use`,
    /// `pause_turn`, `refusal`) are passed through — they're not error
    /// conditions for the `complete` contract.
    fn check_truncation(response: &MessagesResponse, max_tokens: u32) -> Result<()> {
        if matches!(response.stop_reason.as_deref(), Some("max_tokens")) {
            return Err(LlmError::TruncatedOutput {
                output_tokens: response.usage.output_tokens,
                max_tokens,
            });
        }
        Ok(())
    }

    /// Extract the text content from a Messages API response.
    fn extract_text(response: &MessagesResponse) -> Result<String> {
        let text: String = response
            .content
            .iter()
            .filter_map(|block| {
                if block.content_type == "text" {
                    Some(block.text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        Ok(text)
    }
}

impl LlmClient for ClaudeBackend {
    async fn complete(&self, prompt: &str, params: &LlmParams) -> Result<String> {
        let request = self.build_request(prompt, params);
        let response = self.send_request(&request).await?;
        Self::check_truncation(&response, request.max_tokens)?;
        Self::extract_text(&response)
    }

    async fn stream(&self, prompt: &str, params: &LlmParams) -> Result<Vec<String>> {
        // For now, use non-streaming and return as a single chunk.
        // Full SSE streaming can be added when needed.
        let text = self.complete(prompt, params).await?;
        Ok(vec![text])
    }
}

// ── Anthropic Messages API types ──────────────────────────────────────────

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    #[allow(dead_code)]
    model: String,
    /// CH-4406 — read by [`ClaudeBackend::check_truncation`] to detect
    /// `max_tokens` cap hits and surface them as
    /// [`LlmError::TruncatedOutput`].
    stop_reason: Option<String>,
    /// CH-4406 — `usage.output_tokens` is read by
    /// [`ClaudeBackend::check_truncation`] for the truncation-error
    /// payload. `input_tokens` is currently unused.
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[allow(dead_code)]
    input_tokens: u32,
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_defaults() {
        let backend =
            ClaudeBackend::with_config("claude-sonnet-4-20250514", "test-key", "http://localhost");
        let params = LlmParams::default();
        let req = backend.build_request("Hello", &params);

        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "Hello");
        assert!(req.system.is_none());
        assert!(req.temperature.is_none());
        assert!(req.stop_sequences.is_none());
        assert!(!req.stream);
    }

    #[test]
    fn test_build_request_with_params() {
        let backend =
            ClaudeBackend::with_config("claude-opus-4-20250514", "key", "http://localhost");
        let params = LlmParams::default()
            .with_temperature(0.5)
            .with_max_tokens(2048)
            .with_system_prompt("Be concise.")
            .with_stop_sequences(vec!["END".into()]);
        let req = backend.build_request("Test prompt", &params);

        assert_eq!(req.max_tokens, 2048);
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.system, Some("Be concise.".into()));
        assert_eq!(req.stop_sequences, Some(vec!["END".into()]));
    }

    #[test]
    fn test_temperature_clamped() {
        let backend = ClaudeBackend::with_config("model", "key", "http://localhost");
        let params = LlmParams::default().with_temperature(2.5);
        let req = backend.build_request("test", &params);
        assert_eq!(req.temperature, Some(1.0));
    }

    #[test]
    fn test_extract_text_success() {
        let response = MessagesResponse {
            content: vec![ContentBlock {
                content_type: "text".into(),
                text: "Hello world".into(),
            }],
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: Some("end_turn".into()),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        let text = ClaudeBackend::extract_text(&response).unwrap();
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_extract_text_multiple_blocks() {
        let response = MessagesResponse {
            content: vec![
                ContentBlock {
                    content_type: "text".into(),
                    text: "Hello ".into(),
                },
                ContentBlock {
                    content_type: "text".into(),
                    text: "world".into(),
                },
            ],
            model: "model".into(),
            stop_reason: None,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        let text = ClaudeBackend::extract_text(&response).unwrap();
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_extract_text_empty() {
        let response = MessagesResponse {
            content: vec![],
            model: "model".into(),
            stop_reason: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
            },
        };
        let err = ClaudeBackend::extract_text(&response).unwrap_err();
        assert!(matches!(err, LlmError::EmptyResponse));
    }

    /// CH-4406 — `stop_reason == "max_tokens"` must surface as
    /// [`LlmError::TruncatedOutput`] carrying the token counts. Without
    /// this, downstream callers see the truncation as a confusing JSON
    /// parse error rather than as an actionable "raise the cap" signal.
    #[test]
    fn check_truncation_max_tokens_returns_truncated_error() {
        let response = MessagesResponse {
            content: vec![ContentBlock {
                content_type: "text".into(),
                text: "{\"partial\": \"json".into(),
            }],
            model: "claude-opus-4-7".into(),
            stop_reason: Some("max_tokens".into()),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 32_000,
            },
        };
        let err = ClaudeBackend::check_truncation(&response, 32_000)
            .expect_err("must error on truncation");
        match err {
            LlmError::TruncatedOutput {
                output_tokens,
                max_tokens,
            } => {
                assert_eq!(output_tokens, 32_000);
                assert_eq!(max_tokens, 32_000);
            }
            other => panic!("expected TruncatedOutput, got {other:?}"),
        }
    }

    /// CH-4406 — `stop_reason == "end_turn"` (the normal completion
    /// path) must NOT raise the truncation error. Anthropic also emits
    /// `stop_sequence`, `tool_use`, `pause_turn`, `refusal`; none should
    /// trigger the cap-hit signal.
    #[test]
    fn check_truncation_normal_stop_reasons_are_ok() {
        let make = |reason: Option<&'static str>| MessagesResponse {
            content: vec![ContentBlock {
                content_type: "text".into(),
                text: "complete response".into(),
            }],
            model: "m".into(),
            stop_reason: reason.map(String::from),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        for reason in [
            Some("end_turn"),
            Some("stop_sequence"),
            Some("tool_use"),
            Some("pause_turn"),
            Some("refusal"),
            None,
        ] {
            ClaudeBackend::check_truncation(&make(reason), 1024).unwrap_or_else(|e| {
                panic!("stop_reason {reason:?} must not raise truncation: {e}")
            });
        }
    }

    /// CH-4406 — the `output_tokens` field of `LlmError::TruncatedOutput`
    /// reports what the model actually produced, NOT the cap. Useful so
    /// callers can size up the next-attempt cap (e.g. cap × 1.5).
    #[test]
    fn truncated_output_carries_actual_output_token_count() {
        let response = MessagesResponse {
            content: vec![ContentBlock {
                content_type: "text".into(),
                text: "x".into(),
            }],
            model: "m".into(),
            stop_reason: Some("max_tokens".into()),
            usage: Usage {
                input_tokens: 0,
                output_tokens: 8_500,
            },
        };
        let err = ClaudeBackend::check_truncation(&response, 8_192).unwrap_err();
        if let LlmError::TruncatedOutput {
            output_tokens,
            max_tokens,
        } = err
        {
            assert_eq!(output_tokens, 8_500);
            assert_eq!(max_tokens, 8_192);
            // Sanity: the error's Display string includes both numbers.
            let msg = LlmError::TruncatedOutput {
                output_tokens,
                max_tokens,
            }
            .to_string();
            assert!(msg.contains("8500"));
            assert!(msg.contains("8192"));
            assert!(msg.contains("--max-tokens"), "msg must give a hint: {msg}");
        } else {
            panic!("expected TruncatedOutput");
        }
    }

    #[test]
    fn test_new_without_env_key() {
        // Temporarily remove the key if set
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        // SAFETY: test runs single-threaded; no concurrent env access.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        let result = ClaudeBackend::new("model");
        assert!(result.is_err());
        if let Err(LlmError::Config(msg)) = result {
            assert!(msg.contains("ANTHROPIC_API_KEY"));
        }

        // Restore
        if let Some(key) = saved {
            // SAFETY: restoring previous env state.
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", key) };
        }
    }

    #[test]
    fn test_request_serialization() {
        let backend = ClaudeBackend::with_config("model", "key", "http://localhost");
        let params = LlmParams::default();
        let req = backend.build_request("test", &params);
        let json = serde_json::to_string(&req).unwrap();

        // Verify None fields are omitted
        assert!(!json.contains("system"));
        assert!(!json.contains("temperature"));
        assert!(!json.contains("stop_sequences"));
        assert!(!json.contains("stream")); // false → skipped
        assert!(json.contains("\"model\":\"model\""));
    }
}
