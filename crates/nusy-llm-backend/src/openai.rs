//! OpenAI-compatible backend — talks to any `/v1/chat/completions` endpoint.
//!
//! CH-4673 (VY-4679 / VOY-4): the schooling-extraction pipeline (school.rs,
//! reblueprint, the `nusy-y-layers` enricher) was hard-wired to [`ClaudeBackend`](crate::ClaudeBackend)
//! — the Claude cloud API — which needs an `ANTHROPIC_API_KEY` and contradicts
//! GPU-first (VY-3532: schooling should run on the local DGX). This backend lets
//! the same pipeline drive a **local vLLM server** (the GB10 sidecar,
//! `scripts/start_vllm_gb10.py`, default `http://localhost:8000/v1`) through the
//! OpenAI Chat Completions protocol, so the teacher model selected by EXPR-4674
//! (Qwen3-4B) runs on-node.
//!
//! It is generic over any OpenAI-compatible server (vLLM, llama.cpp, OpenAI
//! itself): pass a base URL, model, and optional bearer key. For vLLM-served Qwen3
//! teachers, [`OpenAiBackend::with_disable_thinking`] sends the vLLM
//! `chat_template_kwargs={"enable_thinking": false}` extension so extraction output
//! is clean JSON rather than `<think>` traces (the env recipe from EXPR-4674).

use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::{LlmClient, LlmParams};

/// Default OpenAI-compatible base URL — the local GB10 vLLM sidecar.
const DEFAULT_BASE_URL: &str = "http://localhost:8000/v1";

/// OpenAI-compatible chat-completions backend (vLLM, OpenAI, llama.cpp, …).
pub struct OpenAiBackend {
    client: reqwest::Client,
    /// Base URL including the API version segment, e.g. `http://localhost:8000/v1`.
    base_url: String,
    /// The served model name (must match vLLM's `--served-model-name`).
    model: String,
    /// Bearer token. `None` for an unauthenticated local vLLM; `Some` for OpenAI.
    api_key: Option<String>,
    /// When true, send `chat_template_kwargs={"enable_thinking": false}` (a vLLM
    /// extension Qwen3 honours). Off by default so requests stay strictly
    /// OpenAI-compatible unless explicitly enabled.
    disable_thinking: bool,
}

impl OpenAiBackend {
    /// Create a backend for a local vLLM (or other OpenAI-compatible) server.
    ///
    /// Reads, with sensible local-vLLM defaults:
    /// - `VLLM_BASE_URL` (else `OPENAI_BASE_URL`, else `http://localhost:8000/v1`)
    /// - `OPENAI_API_KEY` (optional — unset is fine for a local server)
    /// - `VLLM_DISABLE_THINKING` (truthy → disable Qwen3 thinking; default off)
    ///
    /// # Arguments
    /// * `model` — served model name (e.g. `"Qwen/Qwen3-4B"`).
    pub fn new(model: impl Into<String>) -> Result<Self> {
        let base_url = std::env::var("VLLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        let disable_thinking = std::env::var("VLLM_DISABLE_THINKING")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
            .unwrap_or(false);
        Ok(Self {
            client: reqwest::Client::new(),
            base_url: normalize_base_url(&base_url),
            model: model.into(),
            api_key,
            disable_thinking,
        })
    }

    /// Construct with an explicit base URL and optional key (used in tests and by
    /// callers that don't want env-driven config).
    pub fn with_config(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: normalize_base_url(&base_url.into()),
            model: model.into(),
            api_key: api_key.filter(|k| !k.is_empty()),
            disable_thinking: false,
        }
    }

    /// Enable/disable the vLLM Qwen3 thinking-suppression extension.
    pub fn with_disable_thinking(mut self, disable: bool) -> Self {
        self.disable_thinking = disable;
        self
    }

    /// Build the Chat Completions request body.
    fn build_request(&self, prompt: &str, params: &LlmParams) -> ChatRequest {
        let mut messages = Vec::new();
        if let Some(system) = params.effective_system_prompt() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: system,
            });
        }
        messages.push(ChatMessage {
            role: "user".into(),
            content: prompt.into(),
        });

        ChatRequest {
            model: self.model.clone(),
            messages,
            max_tokens: params.max_tokens,
            // OpenAI accepts temperature in [0, 2]; clamp defensively.
            temperature: params.temperature.map(|t| t.clamp(0.0, 2.0)),
            stop: if params.stop_sequences.is_empty() {
                None
            } else {
                Some(params.stop_sequences.clone())
            },
            stream: false,
            chat_template_kwargs: self.disable_thinking.then_some(ChatTemplateKwargs {
                enable_thinking: false,
            }),
        }
    }

    /// POST the request and parse the response, mapping HTTP/transport failures
    /// onto [`LlmError`].
    async fn send_request(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("content-type", "application/json")
            .json(request);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let response = req.send().await?;
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
        serde_json::from_str::<ChatResponse>(&body)
            .map_err(|e| LlmError::Parse(format!("Failed to parse response: {e}: {body}")))
    }

    /// Map `finish_reason == "length"` (the OpenAI/vLLM cap-hit signal) onto a
    /// structured [`LlmError::TruncatedOutput`] — mirrors `ClaudeBackend`'s
    /// `max_tokens` handling so downstream callers branch on "raise the cap"
    /// instead of a confusing JSON parse failure. Other finish reasons
    /// (`stop`, `tool_calls`, …) pass through.
    fn check_truncation(response: &ChatResponse, max_tokens: u32) -> Result<()> {
        let truncated = response
            .choices
            .first()
            .and_then(|c| c.finish_reason.as_deref())
            == Some("length");
        if truncated {
            return Err(LlmError::TruncatedOutput {
                output_tokens: response
                    .usage
                    .as_ref()
                    .map(|u| u.completion_tokens)
                    .unwrap_or(max_tokens),
                max_tokens,
            });
        }
        Ok(())
    }

    /// Extract assistant text from the first choice.
    fn extract_text(response: &ChatResponse) -> Result<String> {
        let text = response
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        Ok(text)
    }
}

impl LlmClient for OpenAiBackend {
    async fn complete(&self, prompt: &str, params: &LlmParams) -> Result<String> {
        let request = self.build_request(prompt, params);
        let response = self.send_request(&request).await?;
        Self::check_truncation(&response, request.max_tokens)?;
        Self::extract_text(&response)
    }

    async fn stream(&self, prompt: &str, params: &LlmParams) -> Result<Vec<String>> {
        // Non-streaming for now (parity with ClaudeBackend); SSE can be added later.
        let text = self.complete(prompt, params).await?;
        Ok(vec![text])
    }
}

/// Trim a trailing slash so `{base}/chat/completions` is always well-formed.
fn normalize_base_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

// ── OpenAI Chat Completions API types ──────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
    /// vLLM extension (Qwen3 thinking control); omitted unless explicitly enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
}

#[derive(Debug, Serialize)]
struct ChatTemplateKwargs {
    enable_thinking: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[allow(dead_code)]
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> OpenAiBackend {
        OpenAiBackend::with_config("Qwen/Qwen3-4B", "http://localhost:8000/v1", None)
    }

    #[test]
    fn build_request_defaults() {
        let req = backend().build_request("Hello", &LlmParams::default());
        assert_eq!(req.model, "Qwen/Qwen3-4B");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.messages.len(), 1); // user only, no system
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "Hello");
        assert!(req.temperature.is_none());
        assert!(req.stop.is_none());
        assert!(!req.stream);
        assert!(req.chat_template_kwargs.is_none());
    }

    #[test]
    fn build_request_prepends_system_and_graph_context() {
        let params = LlmParams::default()
            .with_system_prompt("Be terse.")
            .with_graph_context("GRAPH")
            .with_temperature(0.0)
            .with_stop_sequences(vec!["END".into()]);
        let req = backend().build_request("extract", &params);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        // graph context is prepended to the system prompt (effective_system_prompt).
        assert_eq!(req.messages[0].content, "GRAPH\n\nBe terse.");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.temperature, Some(0.0));
        assert_eq!(req.stop, Some(vec!["END".into()]));
    }

    #[test]
    fn temperature_is_clamped_to_openai_range() {
        let params = LlmParams::default().with_temperature(9.0);
        let req = backend().build_request("x", &params);
        assert_eq!(req.temperature, Some(2.0));
    }

    #[test]
    fn disable_thinking_emits_chat_template_kwargs() {
        let b = backend().with_disable_thinking(true);
        let req = b.build_request("x", &LlmParams::default());
        let kw = req.chat_template_kwargs.as_ref().expect("kwargs present");
        assert!(!kw.enable_thinking);
        // And it serializes into the body for vLLM to read.
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("chat_template_kwargs"));
        assert!(json.contains("enable_thinking"));
    }

    #[test]
    fn request_omits_none_fields_and_default_thinking() {
        let json =
            serde_json::to_string(&backend().build_request("x", &LlmParams::default())).unwrap();
        assert!(!json.contains("temperature"));
        assert!(!json.contains("stop"));
        assert!(!json.contains("stream")); // false → skipped
        assert!(!json.contains("chat_template_kwargs"));
        assert!(json.contains("\"model\":\"Qwen/Qwen3-4B\""));
    }

    #[test]
    fn extract_text_joins_first_choice() {
        let resp = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: "assistant".into(),
                    content: "  hello world  ".into(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        };
        assert_eq!(OpenAiBackend::extract_text(&resp).unwrap(), "hello world");
    }

    #[test]
    fn extract_text_empty_is_error() {
        let resp = ChatResponse {
            choices: vec![],
            usage: None,
        };
        assert!(matches!(
            OpenAiBackend::extract_text(&resp).unwrap_err(),
            LlmError::EmptyResponse
        ));
    }

    #[test]
    fn finish_reason_length_is_truncation_error() {
        let resp = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: "assistant".into(),
                    content: "{\"partial".into(),
                },
                finish_reason: Some("length".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 1024,
            }),
        };
        match OpenAiBackend::check_truncation(&resp, 1024).unwrap_err() {
            LlmError::TruncatedOutput {
                output_tokens,
                max_tokens,
            } => {
                assert_eq!(output_tokens, 1024);
                assert_eq!(max_tokens, 1024);
            }
            other => panic!("expected TruncatedOutput, got {other:?}"),
        }
    }

    #[test]
    fn finish_reason_stop_is_not_truncation() {
        let resp = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: "assistant".into(),
                    content: "done".into(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        };
        OpenAiBackend::check_truncation(&resp, 1024).expect("stop is not truncation");
    }

    #[test]
    fn base_url_trailing_slash_is_normalized() {
        let b = OpenAiBackend::with_config("m", "http://localhost:8000/v1/", None);
        assert_eq!(b.base_url, "http://localhost:8000/v1");
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OpenAiBackend>();
    }
}
