//! Shared LLM inference backend for NuSy.
//!
//! EX-3127: Provides a unified `LlmClient` trait with pluggable backends.
//! EX-3435: Ollama backend removed. Candle is the production LLM path.
//! This crate provides Claude (API) and Mock backends for non-Candle use cases.

pub mod claude;
pub mod error;
pub mod openai;

pub use claude::ClaudeBackend;
pub use error::{LlmError, Result};
pub use openai::OpenAiBackend;

/// Parameters for LLM inference requests.
#[derive(Debug, Clone)]
pub struct LlmParams {
    /// Sampling temperature (0.0 = deterministic, 1.0 = creative).
    pub temperature: Option<f64>,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// System prompt prepended to the conversation.
    pub system_prompt: Option<String>,
    /// Stop sequences that terminate generation.
    pub stop_sequences: Vec<String>,
    /// Graph context from `GraphAdapterPipeline` (EX-3243 Path B).
    ///
    /// When `Some`, backends prepend this text to the effective system prompt
    /// before sending to the model. Delivers session-local graph state
    /// (serialized k-hop neighborhoods) without explicit retrieval calls.
    pub graph_context: Option<String>,
}

impl Default for LlmParams {
    fn default() -> Self {
        Self {
            temperature: None,
            max_tokens: 1024,
            system_prompt: None,
            stop_sequences: Vec::new(),
            graph_context: None,
        }
    }
}

impl LlmParams {
    pub fn with_temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn with_system_prompt(mut self, s: impl Into<String>) -> Self {
        self.system_prompt = Some(s.into());
        self
    }

    pub fn with_stop_sequences(mut self, seqs: Vec<String>) -> Self {
        self.stop_sequences = seqs;
        self
    }

    pub fn with_graph_context(mut self, ctx: impl Into<String>) -> Self {
        self.graph_context = Some(ctx.into());
        self
    }

    /// Build the effective system prompt: graph context (if any) prepended
    /// to `system_prompt`, separated by a blank line.
    pub fn effective_system_prompt(&self) -> Option<String> {
        match (&self.graph_context, &self.system_prompt) {
            (Some(ctx), Some(sys)) => Some(format!("{ctx}\n\n{sys}")),
            (Some(ctx), None) => Some(ctx.clone()),
            (None, Some(sys)) => Some(sys.clone()),
            (None, None) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// LlmCallCounter (EX-3243 Phase 3b)
// ---------------------------------------------------------------------------

/// Thread-safe counter for LLM backend calls.
///
/// Used by EXPR-3216 to measure retrieval call reduction when the
/// `GraphAdapterPipeline` is active.
///
/// # Example
///
/// ```
/// use nusy_llm_backend::LlmCallCounter;
///
/// let counter = LlmCallCounter::new();
/// counter.increment();
/// counter.increment();
/// assert_eq!(counter.get(), 2);
/// counter.reset();
/// assert_eq!(counter.get(), 0);
/// ```
#[derive(Debug, Default)]
pub struct LlmCallCounter {
    count: std::sync::atomic::AtomicUsize,
}

impl LlmCallCounter {
    /// Create a new counter initialized to zero.
    pub fn new() -> Self {
        Self {
            count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Increment the counter by one.
    pub fn increment(&self) {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the current count.
    pub fn get(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        self.count.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Trait for LLM inference backends.
///
/// Implementations must be `Send + Sync` for use across async tasks.
/// Both `complete` and `stream` are async — callers use `.await`.
pub trait LlmClient: Send + Sync {
    /// Complete a prompt and return the full response text.
    fn complete(
        &self,
        prompt: &str,
        params: &LlmParams,
    ) -> impl std::future::Future<Output = Result<String>> + Send;

    /// Stream a prompt response, returning chunks as they arrive.
    ///
    /// Returns a `Vec<String>` of streamed chunks. For backends that don't
    /// support streaming natively, this falls back to a single-chunk response.
    fn stream(
        &self,
        prompt: &str,
        params: &LlmParams,
    ) -> impl std::future::Future<Output = Result<Vec<String>>> + Send;
}

/// Mock LLM backend for testing.
///
/// Returns deterministic responses based on prompt content. Useful for
/// unit tests that exercise downstream logic without real LLM calls.
pub struct MockLlmBackend {
    /// Canned responses to return, consumed in order. If empty, falls back
    /// to a default echo response.
    responses: std::sync::Mutex<Vec<String>>,
}

impl MockLlmBackend {
    /// Create a mock with no canned responses (uses default echo behavior).
    pub fn new() -> Self {
        Self {
            responses: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Create a mock that returns the given responses in order.
    ///
    /// Each call to `complete` or `stream` pops the first response.
    /// When exhausted, falls back to echo behavior.
    pub fn with_responses(responses: Vec<String>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }

    fn next_response(&self, prompt: &str) -> String {
        let mut queue = self.responses.lock().expect("mock lock poisoned");
        if queue.is_empty() {
            format!("mock-response(len={})", prompt.len())
        } else {
            queue.remove(0)
        }
    }
}

impl Default for MockLlmBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmClient for MockLlmBackend {
    async fn complete(&self, prompt: &str, _params: &LlmParams) -> Result<String> {
        Ok(self.next_response(prompt))
    }

    async fn stream(&self, prompt: &str, _params: &LlmParams) -> Result<Vec<String>> {
        let response = self.next_response(prompt);
        // Simulate streaming by splitting into word-sized chunks.
        let chunks: Vec<String> = response.split_whitespace().map(|w| w.to_string()).collect();
        if chunks.is_empty() {
            Ok(vec![response])
        } else {
            Ok(chunks)
        }
    }
}

// ---------------------------------------------------------------------------
// Backend selection (CH-4673)
// ---------------------------------------------------------------------------

/// A runtime-selectable [`LlmClient`]: the local vLLM teacher (GPU-first), the
/// Claude cloud API (fallback), or the deterministic mock.
///
/// The [`LlmClient`] trait is not dyn-compatible (it returns `impl Future`), so
/// callers that need to pick a backend at runtime hold this enum — it satisfies
/// `L: LlmClient` and dispatches to the chosen variant.
pub enum Backend {
    /// Local vLLM / OpenAI-compatible server (GPU-first, the schooling default on DGX).
    Vllm(OpenAiBackend),
    /// Anthropic Messages API (fallback; needs `ANTHROPIC_API_KEY`).
    Claude(ClaudeBackend),
    /// Deterministic mock for tests.
    Mock(MockLlmBackend),
}

impl Backend {
    /// Select a backend from the `LLM_BACKEND` env var, building it for `model`:
    ///
    /// | `LLM_BACKEND`        | backend                              |
    /// |----------------------|--------------------------------------|
    /// | `local-vllm`/`vllm`  | [`OpenAiBackend`] (local GB10 vLLM)  |
    /// | `mock`               | [`MockLlmBackend`]                   |
    /// | `claude`/unset       | [`ClaudeBackend`] (default, fallback)|
    ///
    /// Unset defaults to `claude` to preserve existing behavior; DGX schooling
    /// sets `LLM_BACKEND=local-vllm` (GPU-first, VY-3532). An unrecognized value
    /// is a [`LlmError::Config`] error rather than a silent default, so typos surface.
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let kind = std::env::var("LLM_BACKEND").unwrap_or_else(|_| "claude".to_string());
        match kind.as_str() {
            "local-vllm" | "vllm" => {
                // The vLLM-served model name is independent of the caller's
                // (Claude-oriented) `model`: default to the EXPR-4674 teacher
                // (Qwen3-4B), overridable via `VLLM_MODEL`.
                let vllm_model =
                    std::env::var("VLLM_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-4B".to_string());
                Ok(Backend::Vllm(OpenAiBackend::new(vllm_model)?))
            }
            "mock" => Ok(Backend::Mock(MockLlmBackend::new())),
            "claude" => Ok(Backend::Claude(ClaudeBackend::new(model)?)),
            other => Err(LlmError::Config(format!(
                "unknown LLM_BACKEND `{other}` (expected local-vllm | claude | mock)"
            ))),
        }
    }
}

impl LlmClient for Backend {
    async fn complete(&self, prompt: &str, params: &LlmParams) -> Result<String> {
        match self {
            Backend::Vllm(b) => b.complete(prompt, params).await,
            Backend::Claude(b) => b.complete(prompt, params).await,
            Backend::Mock(b) => b.complete(prompt, params).await,
        }
    }

    async fn stream(&self, prompt: &str, params: &LlmParams) -> Result<Vec<String>> {
        match self {
            Backend::Vllm(b) => b.stream(prompt, params).await,
            Backend::Claude(b) => b.stream(prompt, params).await,
            Backend::Mock(b) => b.stream(prompt, params).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_default_params() {
        let params = LlmParams::default();
        assert_eq!(params.max_tokens, 1024);
        assert!(params.temperature.is_none());
        assert!(params.system_prompt.is_none());
        assert!(params.stop_sequences.is_empty());
    }

    #[tokio::test]
    async fn test_params_builder() {
        let params = LlmParams::default()
            .with_temperature(0.7)
            .with_max_tokens(2048)
            .with_system_prompt("You are helpful.")
            .with_stop_sequences(vec!["STOP".into()]);

        assert_eq!(params.temperature, Some(0.7));
        assert_eq!(params.max_tokens, 2048);
        assert_eq!(params.system_prompt.as_deref(), Some("You are helpful."));
        assert_eq!(params.stop_sequences, vec!["STOP"]);
    }

    #[tokio::test]
    async fn test_mock_default_response() {
        let mock = MockLlmBackend::new();
        let result = mock.complete("hello", &LlmParams::default()).await.unwrap();
        assert_eq!(result, "mock-response(len=5)");
    }

    #[tokio::test]
    async fn test_mock_canned_responses() {
        let mock = MockLlmBackend::with_responses(vec!["first".into(), "second".into()]);
        let r1 = mock.complete("a", &LlmParams::default()).await.unwrap();
        let r2 = mock.complete("b", &LlmParams::default()).await.unwrap();
        let r3 = mock.complete("c", &LlmParams::default()).await.unwrap();

        assert_eq!(r1, "first");
        assert_eq!(r2, "second");
        // Exhausted → falls back to echo
        assert_eq!(r3, "mock-response(len=1)");
    }

    #[tokio::test]
    async fn test_mock_stream() {
        let mock = MockLlmBackend::with_responses(vec!["hello world foo".into()]);
        let chunks = mock.stream("prompt", &LlmParams::default()).await.unwrap();
        assert_eq!(chunks, vec!["hello", "world", "foo"]);
    }

    #[tokio::test]
    async fn test_mock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockLlmBackend>();
        // The selectable Backend enum must also be usable across async tasks.
        assert_send_sync::<Backend>();
    }

    #[tokio::test]
    async fn backend_enum_dispatches_to_mock() {
        let backend = Backend::Mock(MockLlmBackend::with_responses(vec!["routed".into()]));
        let out = backend.complete("x", &LlmParams::default()).await.unwrap();
        assert_eq!(out, "routed");
        let chunks = backend.stream("y", &LlmParams::default()).await.unwrap();
        assert_eq!(chunks, vec!["mock-response(len=1)"]);
    }

    #[test]
    fn backend_from_env_rejects_unknown_value() {
        // SAFETY: single-threaded test; no other test reads LLM_BACKEND.
        let saved = std::env::var("LLM_BACKEND").ok();
        unsafe { std::env::set_var("LLM_BACKEND", "gpt-9000") };
        // Note: matches! (not expect_err) so the test doesn't require Backend: Debug.
        let result = Backend::from_env("m");
        assert!(matches!(&result, Err(LlmError::Config(msg)) if msg.contains("gpt-9000")));
        match saved {
            Some(v) => unsafe { std::env::set_var("LLM_BACKEND", v) },
            None => unsafe { std::env::remove_var("LLM_BACKEND") },
        }
    }

    #[test]
    fn backend_from_env_mock_builds() {
        // SAFETY: single-threaded test; restores the prior value.
        let saved = std::env::var("LLM_BACKEND").ok();
        unsafe { std::env::set_var("LLM_BACKEND", "mock") };
        assert!(matches!(Backend::from_env("m"), Ok(Backend::Mock(_))));
        match saved {
            Some(v) => unsafe { std::env::set_var("LLM_BACKEND", v) },
            None => unsafe { std::env::remove_var("LLM_BACKEND") },
        }
    }
}
