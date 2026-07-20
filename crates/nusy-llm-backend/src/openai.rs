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

    /// The resolved base URL, e.g. `https://api.openai.com/v1` (EX-5134 — exposed
    /// so callers/logs can report which endpoint a config-selected backend points at).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The served model name the requests target.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether Qwen3 `<think>` reasoning is disabled for this backend (CH-5430).
    /// Exposed so callers can lock the default in a test (CH-5309/PROP-3110: the
    /// proposer MUST keep this `true`, else the `<think>` preamble eats the verdict
    /// budget → silent abstain).
    pub fn disable_thinking(&self) -> bool {
        self.disable_thinking
    }

    /// **Probe `GET {base_url}/models` and verify this backend's `model` is actually served**
    /// (CH-5341). Under an explicit "live" request a model the server doesn't publish must FAIL
    /// LOUD — naming the served set vs the requested model — never silently 404 → abstain, which
    /// makes a misconfigured run indistinguishable from a real one except by wall-time (the
    /// canned-output trap CH-5221 warns about). An unreachable endpoint is likewise a loud error.
    pub async fn verify_model_served(&self) -> Result<()> {
        let url = format!("{}/models", self.base_url);
        let mut req = self.client.get(&url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| {
            LlmError::Config(format!(
                "live backend: vLLM endpoint {} is unreachable while verifying the served model \
                 ({e}). Refusing to proceed (would otherwise silently fall back / abstain).",
                self.base_url
            ))
        })?;
        if !resp.status().is_success() {
            return Err(LlmError::Config(format!(
                "live backend: {url} returned {} — cannot verify the served model; refusing.",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp.json().await?;
        let served: Vec<String> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if served.iter().any(|m| m == &self.model) {
            Ok(())
        } else {
            Err(LlmError::Config(format!(
                "live backend: vLLM at {} serves {served:?} but the requested model '{}' is NOT \
                 among them — refusing (a 404 here would silently abstain, masking the misconfig \
                 as a real run). Set VLLM_MODEL to a served id.",
                self.base_url, self.model
            )))
        }
    }

    /// Whether a bearer key is configured — `true` for a hosted OpenAI-compatible
    /// provider, `false` for an unauthenticated local vLLM / Ollama server.
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
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

impl OpenAiBackend {
    /// Like [`LlmClient::complete`], but on a `max_tokens` truncation returns the PARTIAL text with a
    /// `true` truncation flag instead of erroring — so a caller that can salvage the prefix (e.g. the
    /// complete JSON objects generated before the cut) recovers them at ZERO extra generation.
    ///
    /// CH-6045: high-interaction DDI lists truncate, and re-generating at a higher cap is impractically
    /// slow (~9 tok/s on the GB10) and often just yields more repetition; salvaging the already-generated
    /// prefix is the cheap, correct recovery (after dedup, even a repetitive truncation surfaces the real
    /// partners). Returns `(text, was_truncated)`.
    pub async fn complete_lenient(
        &self,
        prompt: &str,
        params: &LlmParams,
    ) -> Result<(String, bool)> {
        let request = self.build_request(prompt, params);
        let response = self.send_request(&request).await?;
        let truncated = response
            .choices
            .first()
            .and_then(|c| c.finish_reason.as_deref())
            == Some("length");
        let text = Self::extract_text(&response)?;
        Ok((text, truncated))
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

/// A named OpenAI-compatible provider — a default-endpoint + auth *profile* for
/// the one [`OpenAiBackend`] adapter (EX-5134, "easy LLM plug-in").
///
/// Every variant speaks the same `/v1/chat/completions` protocol; only the
/// default base URL and whether an API key is required differ. Hosted
/// OpenAI-compatible services not named here (Together, Groq, LM Studio, …) are
/// reached through [`OpenAiProvider::OpenAi`] with `OPENAI_BASE_URL` pointed at
/// their endpoint — one adapter, many providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiProvider {
    /// Local vLLM server (the GB10 sidecar) — `http://localhost:8000/v1`, key optional.
    Vllm,
    /// OpenAI or any hosted OpenAI-compatible API — `https://api.openai.com/v1`,
    /// `OPENAI_API_KEY` required. Together/Groq/LM Studio via an `OPENAI_BASE_URL` override.
    OpenAi,
    /// Ollama's OpenAI-compatible mode — `http://localhost:11434/v1`, no key.
    Ollama,
}

impl OpenAiProvider {
    /// The provider's default base URL, used when no `OPENAI_BASE_URL` override is set.
    pub fn default_base_url(self) -> &'static str {
        match self {
            OpenAiProvider::Vllm => "http://localhost:8000/v1",
            OpenAiProvider::OpenAi => "https://api.openai.com/v1",
            OpenAiProvider::Ollama => "http://localhost:11434/v1",
        }
    }

    /// Whether this provider requires an API key. Hosted OpenAI does; a local
    /// vLLM / Ollama does not. A missing required key is surfaced as a config
    /// error in [`Backend::from_env`](crate::Backend::from_env) — never a silent
    /// unauthenticated call.
    pub fn requires_key(self) -> bool {
        matches!(self, OpenAiProvider::OpenAi)
    }
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

    #[test]
    fn provider_profiles_are_correct() {
        // Pure profile data — no env, no races (EX-5134).
        assert_eq!(
            OpenAiProvider::Vllm.default_base_url(),
            "http://localhost:8000/v1"
        );
        assert_eq!(
            OpenAiProvider::OpenAi.default_base_url(),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            OpenAiProvider::Ollama.default_base_url(),
            "http://localhost:11434/v1"
        );
        // Only the hosted OpenAI profile demands a key.
        assert!(OpenAiProvider::OpenAi.requires_key());
        assert!(!OpenAiProvider::Vllm.requires_key());
        assert!(!OpenAiProvider::Ollama.requires_key());
    }

    #[test]
    fn config_accessors_report_resolved_profile() {
        let b = OpenAiBackend::with_config(
            "gpt-4o-mini",
            "https://api.openai.com/v1",
            Some("sk-test".into()),
        );
        assert_eq!(b.base_url(), "https://api.openai.com/v1");
        assert_eq!(b.model(), "gpt-4o-mini");
        assert!(b.has_api_key());
        // An empty key is treated as "no key" (with_config filters it).
        let local = OpenAiBackend::with_config("llama3", "http://localhost:11434/v1", None);
        assert!(!local.has_api_key());
    }

    // ── CH-5341: served-model probe (verify_model_served) — wiremock, no GPU/vLLM ──────────────

    #[tokio::test]
    async fn verify_model_served_ok_when_model_listed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "Qwen/Qwen3-32B"}, {"id": "other"}]
            })))
            .mount(&server)
            .await;
        let b = OpenAiBackend::with_config("Qwen/Qwen3-32B", format!("{}/v1", server.uri()), None);
        b.verify_model_served()
            .await
            .expect("served model must verify");
    }

    #[tokio::test]
    async fn verify_model_served_errors_naming_served_vs_requested() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "Qwen/Qwen3-4B"}] // server serves 4B…
            })))
            .mount(&server)
            .await;
        // …but the run requests 32B (the CH-5341 mismatch trap).
        let b = OpenAiBackend::with_config("Qwen/Qwen3-32B", format!("{}/v1", server.uri()), None);
        let err = b
            .verify_model_served()
            .await
            .expect_err("mismatch must fail loud");
        let msg = err.to_string();
        assert!(
            msg.contains("Qwen/Qwen3-32B"),
            "names requested model: {msg}"
        );
        assert!(msg.contains("Qwen/Qwen3-4B"), "names served model: {msg}");
    }

    #[tokio::test]
    async fn verify_model_served_errors_when_unreachable() {
        // Nothing listening on this port → reachability failure must be a loud error, not silent.
        let b = OpenAiBackend::with_config("m", "http://127.0.0.1:1/v1", None);
        let err = b
            .verify_model_served()
            .await
            .expect_err("unreachable must fail loud");
        assert!(err.to_string().contains("unreachable"), "got: {err}");
    }
}
