//! Shared LLM inference backend for NuSy.
//!
//! EX-3127: Provides a unified `LlmClient` trait with pluggable backends.
//! EX-3435: Ollama backend removed. Candle is the production LLM path.
//! This crate provides Claude (API) and Mock backends for non-Candle use cases.

pub mod claude;
// EX-4985: the LLM-egress PHI gate (allowlist + TLS + de-id precondition + fail-closed).
pub mod egress;
pub mod error;
pub mod openai;

pub use claude::ClaudeBackend;
pub use egress::{
    DeIdAttestation, EgressAudit, EgressError, EgressGate, EgressPolicy, GatedPrompt,
    resolve_openai_base_url_from_env,
};
pub use error::{LlmError, Result};
pub use openai::{OpenAiBackend, OpenAiProvider};

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

/// Run `client.complete` over many prompts with **bounded concurrency**, preserving input order.
///
/// EX-6065: the extraction/confirm/gap-source paths were serial `for … { client.complete().await }`
/// loops — one in-flight request at a time, so a continuous-batching server (vLLM) never fills its
/// batch and throughput is ~1 stream. This drives up to `concurrency` requests in flight at once
/// (`buffer_unordered`), which feeds the batcher and buys ~Nx (the expedition's 20-40x lever), while
/// still returning results in the SAME order as `prompts` (each future carries its index; the stream
/// completes out of order but is re-sorted). Each element is an independent `Result` — one failed
/// call does not sink the batch (fail-closed per-item, mirroring the serial loop's per-item handling).
///
/// `concurrency` is clamped to `>= 1`. For a local vLLM, 32–64 is a good default; for an external API,
/// match the provider's rate limit.
pub async fn complete_batch<C: LlmClient>(
    client: &C,
    prompts: &[String],
    params: &LlmParams,
    concurrency: usize,
) -> Vec<Result<String>> {
    use futures::stream::{self, StreamExt};
    let n = concurrency.max(1);
    let mut indexed: Vec<(usize, Result<String>)> = stream::iter(prompts.iter().enumerate())
        .map(|(i, prompt)| async move { (i, client.complete(prompt, params).await) })
        .buffer_unordered(n)
        .collect()
        .await;
    // buffer_unordered yields out of order — restore the caller's order by index.
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
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

/// A runtime-selectable [`LlmClient`]: any OpenAI-compatible endpoint (local vLLM,
/// hosted OpenAI/Together/Groq/LM Studio, or Ollama's OpenAI mode), the Anthropic
/// Messages API, or the deterministic mock.
///
/// The [`LlmClient`] trait is not dyn-compatible (it returns `impl Future`), so
/// callers that need to pick a backend at runtime hold this enum — it satisfies
/// `L: LlmClient` and dispatches to the chosen variant. Selection is config-driven
/// via [`Backend::from_env`] (EX-5134, the "easy LLM plug-in").
pub enum Backend {
    /// Any OpenAI-compatible server (vLLM, hosted OpenAI/Together/Groq/LM Studio,
    /// Ollama). One adapter, many providers — see [`OpenAiProvider`].
    OpenAiCompat(OpenAiBackend),
    /// Anthropic Messages API (needs `ANTHROPIC_API_KEY`).
    Claude(ClaudeBackend),
    /// Deterministic mock for tests / the CPU-offline path.
    Mock(MockLlmBackend),
}

impl Backend {
    /// Select a backend from the `LLM_BACKEND` env var, building it for `model`
    /// (EX-5134 — "plug in your LLM with one env var"):
    ///
    /// | `LLM_BACKEND`           | backend                                            | endpoint / key |
    /// |-------------------------|----------------------------------------------------|----------------|
    /// | `vllm` / `local-vllm`   | [`OpenAiBackend`] via [`OpenAiProvider::Vllm`]      | `localhost:8000/v1`, key optional |
    /// | `openai`                | [`OpenAiBackend`] via [`OpenAiProvider::OpenAi`]    | `api.openai.com/v1`, `OPENAI_API_KEY` required |
    /// | `ollama`                | [`OpenAiBackend`] via [`OpenAiProvider::Ollama`]    | `localhost:11434/v1`, no key |
    /// | `claude` / `anthropic`  | [`ClaudeBackend`]                                  | `ANTHROPIC_API_KEY` required |
    /// | `mock`                  | [`MockLlmBackend`]                                 | CPU / offline |
    /// | unset                   | [`ClaudeBackend`] (preserves existing behavior)    | |
    ///
    /// Per-provider env overrides for the OpenAI-compatible adapter: `OPENAI_BASE_URL`
    /// (point `openai` at Together/Groq/LM Studio, or override the local default),
    /// `OPENAI_API_KEY`, and `OPENAI_MODEL` (else the caller's `model`). vLLM keeps
    /// its established `VLLM_BASE_URL` / `VLLM_MODEL` recipe (EXPR-4674).
    ///
    /// An unrecognized value is a [`LlmError::Config`] error rather than a silent
    /// default, so typos surface; a hosted provider missing its required key is
    /// likewise a config error, never a silent unauthenticated call.
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let kind = std::env::var("LLM_BACKEND").unwrap_or_else(|_| "claude".to_string());
        match kind.as_str() {
            "local-vllm" | "vllm" => {
                // The vLLM-served model name is independent of the caller's
                // (Claude-oriented) `model`: default to the EXPR-4674 teacher
                // (Qwen3-4B), overridable via `VLLM_MODEL`. `OpenAiBackend::new`
                // already reads VLLM_BASE_URL/OPENAI_BASE_URL/OPENAI_API_KEY.
                let vllm_model =
                    std::env::var("VLLM_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-4B".to_string());
                Ok(Backend::OpenAiCompat(OpenAiBackend::new(vllm_model)?))
            }
            "openai" | "ollama" => {
                let provider = if kind == "openai" {
                    OpenAiProvider::OpenAi
                } else {
                    OpenAiProvider::Ollama
                };
                // Read env here (the impure edge); the resolution logic is the
                // pure `build_openai_compatible` so it is testable without races.
                Ok(Backend::OpenAiCompat(build_openai_compatible(
                    provider,
                    std::env::var("OPENAI_BASE_URL").ok(),
                    std::env::var("OPENAI_API_KEY").ok(),
                    std::env::var("OPENAI_MODEL").ok(),
                    model,
                )?))
            }
            "mock" => Ok(Backend::Mock(MockLlmBackend::new())),
            "claude" | "anthropic" => Ok(Backend::Claude(ClaudeBackend::new(model)?)),
            other => Err(LlmError::Config(format!(
                "unknown LLM_BACKEND `{other}` (expected vllm | openai | ollama | claude | anthropic | mock)"
            ))),
        }
    }

    /// **Liveness/served-model check before an explicit live run** (CH-5341). For an
    /// OpenAI-compatible (vLLM) backend, probes `/v1/models` and fails loud if the endpoint is
    /// unreachable or the configured model isn't served (rather than letting the call 404 → a
    /// silent abstention that masquerades as a real run). A no-op for `Mock`/`Claude`.
    pub async fn verify_ready(&self) -> Result<()> {
        match self {
            Backend::OpenAiCompat(b) => b.verify_model_served().await,
            Backend::Mock(_) | Backend::Claude(_) => Ok(()),
        }
    }

    /// CH-5430: force thinking-disabled (or -enabled) on the OpenAI-compatible
    /// (vLLM) variant, so structured-output tasks (blueprint/school/extraction)
    /// don't burn the `max_tokens` budget on Qwen3 `<think>` tokens (the
    /// truncation-panic footgun). A no-op for `Claude`/`Mock`, which have no
    /// reasoning-mode toggle.
    pub fn with_disable_thinking(self, disable: bool) -> Self {
        match self {
            Backend::OpenAiCompat(b) => Backend::OpenAiCompat(b.with_disable_thinking(disable)),
            other => other,
        }
    }

    /// Whether Qwen3 `<think>` reasoning is disabled (CH-5430). `false` for
    /// `Claude`/`Mock` (no reasoning toggle). Exposed so callers can lock the
    /// default in a test (CH-5309/PROP-3110 never-launder guard).
    pub fn disable_thinking(&self) -> bool {
        match self {
            Backend::OpenAiCompat(b) => b.disable_thinking(),
            _ => false,
        }
    }
}

/// Resolve an OpenAI-compatible backend from a provider profile + already-read
/// env values (EX-5134). **Pure** — env reads happen in [`Backend::from_env`];
/// passing the values in keeps this unit-testable with no env-var races
/// (the lesson from the EX-4985 env-test flake).
///
/// - base URL: `env_base_url` if set, else the provider's [`default_base_url`](OpenAiProvider::default_base_url).
/// - key: `env_api_key` (empty treated as absent); a missing **required** key
///   (per [`requires_key`](OpenAiProvider::requires_key)) is a [`LlmError::Config`].
/// - model: `env_model` if set+non-empty, else `fallback_model`.
fn build_openai_compatible(
    provider: OpenAiProvider,
    env_base_url: Option<String>,
    env_api_key: Option<String>,
    env_model: Option<String>,
    fallback_model: String,
) -> Result<OpenAiBackend> {
    let base_url = env_base_url
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| provider.default_base_url().to_string());
    let api_key = env_api_key.filter(|k| !k.is_empty());
    if provider.requires_key() && api_key.is_none() {
        return Err(LlmError::Config(format!(
            "LLM_BACKEND selects a hosted OpenAI-compatible provider ({}) but OPENAI_API_KEY is unset \
             — set OPENAI_API_KEY (no silent unauthenticated call)",
            provider.default_base_url()
        )));
    }
    let model = env_model
        .filter(|m| !m.is_empty())
        .unwrap_or(fallback_model);
    Ok(OpenAiBackend::with_config(model, base_url, api_key))
}

impl LlmClient for Backend {
    async fn complete(&self, prompt: &str, params: &LlmParams) -> Result<String> {
        match self {
            Backend::OpenAiCompat(b) => b.complete(prompt, params).await,
            Backend::Claude(b) => b.complete(prompt, params).await,
            Backend::Mock(b) => b.complete(prompt, params).await,
        }
    }

    async fn stream(&self, prompt: &str, params: &LlmParams) -> Result<Vec<String>> {
        match self {
            Backend::OpenAiCompat(b) => b.stream(prompt, params).await,
            Backend::Claude(b) => b.stream(prompt, params).await,
            Backend::Mock(b) => b.stream(prompt, params).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn complete_batch_preserves_order() {
        // MockLlmBackend echoes prompt length; give distinct-length prompts and confirm the results
        // come back in input order despite buffer_unordered's out-of-order completion.
        let mock = MockLlmBackend::new();
        let prompts: Vec<String> = vec!["a".into(), "bb".into(), "ccc".into(), "dddd".into()];
        let out = complete_batch(&mock, &prompts, &LlmParams::default(), 3).await;
        assert_eq!(out.len(), 4);
        let texts: Vec<String> = out.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(texts[0], "mock-response(len=1)");
        assert_eq!(texts[1], "mock-response(len=2)");
        assert_eq!(texts[2], "mock-response(len=3)");
        assert_eq!(texts[3], "mock-response(len=4)");
    }

    #[tokio::test]
    async fn complete_batch_isolates_per_item_errors() {
        // A backend that errors on a specific prompt — the batch returns Err for that item and Ok
        // for the rest (one failure must not sink the whole batch, mirroring the serial loop).
        struct SelectiveErr;
        impl LlmClient for SelectiveErr {
            async fn complete(&self, prompt: &str, _p: &LlmParams) -> Result<String> {
                if prompt == "boom" {
                    Err(LlmError::Config("boom".into()))
                } else {
                    Ok(format!("ok:{prompt}"))
                }
            }
            async fn stream(&self, _p: &str, _q: &LlmParams) -> Result<Vec<String>> {
                Ok(vec![])
            }
        }
        let prompts: Vec<String> = vec!["x".into(), "boom".into(), "y".into()];
        let out = complete_batch(&SelectiveErr, &prompts, &LlmParams::default(), 8).await;
        assert!(out[0].is_ok() && out[0].as_ref().unwrap() == "ok:x");
        assert!(
            out[1].is_err(),
            "the erroring item is Err, in its input position"
        );
        assert!(out[2].is_ok() && out[2].as_ref().unwrap() == "ok:y");
    }

    #[tokio::test]
    async fn complete_batch_concurrency_zero_is_clamped_to_one() {
        // concurrency=0 must not deadlock/panic — clamped to 1 (serial, still correct).
        let mock = MockLlmBackend::new();
        let out = complete_batch(&mock, &["a".into(), "bb".into()], &LlmParams::default(), 0).await;
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn test_default_params() {
        let params = LlmParams::default();
        assert_eq!(params.max_tokens, 1024);
        assert!(params.temperature.is_none());
        assert!(params.system_prompt.is_none());
        assert!(params.stop_sequences.is_empty());
    }

    #[test]
    fn ch5430_with_disable_thinking_is_noop_for_non_vllm() {
        // Passthrough: a no-op for Mock/Claude (no reasoning toggle). The
        // OpenAiCompat arm's effect is covered by openai.rs
        // `disable_thinking_emits_chat_template_kwargs`.
        let b = Backend::Mock(MockLlmBackend::new()).with_disable_thinking(true);
        assert!(matches!(b, Backend::Mock(_)));
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

    /// Serializes every test that mutates process env (`LLM_BACKEND`,
    /// `OPENAI_*`). cargo runs tests on multiple threads, so two env-mutating
    /// tests can otherwise race and clobber each other's save/restore — the
    /// flake class behind the EX-4985 env-test removal. Holding this lock for the
    /// whole set-read-restore window makes them deterministic without a new dep.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `LLM_BACKEND` (+ optional `OPENAI_*` overrides), run `f`, then restore
    /// every touched var — all under [`ENV_LOCK`]. SAFETY: the lock guarantees no
    /// other env-mutating test runs concurrently.
    fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var(k).ok();
                match v {
                    Some(val) => unsafe { std::env::set_var(k, val) },
                    None => unsafe { std::env::remove_var(k) },
                }
                (k.to_string(), prev)
            })
            .collect();
        f();
        for (k, prev) in saved {
            match prev {
                Some(val) => unsafe { std::env::set_var(&k, val) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
    }

    #[test]
    fn backend_from_env_rejects_unknown_value() {
        with_env(&[("LLM_BACKEND", Some("gpt-9000"))], || {
            // matches! (not expect_err) so the test doesn't require Backend: Debug.
            let result = Backend::from_env("m");
            assert!(matches!(&result, Err(LlmError::Config(msg)) if msg.contains("gpt-9000")));
        });
    }

    #[test]
    fn backend_from_env_mock_builds() {
        with_env(&[("LLM_BACKEND", Some("mock"))], || {
            assert!(matches!(Backend::from_env("m"), Ok(Backend::Mock(_))));
        });
    }

    #[test]
    fn backend_from_env_ollama_alias_selects_openai_compat() {
        // EX-5134: `ollama` is an OpenAI-compatible provider needing no key.
        with_env(
            &[
                ("LLM_BACKEND", Some("ollama")),
                ("OPENAI_BASE_URL", None),
                ("OPENAI_API_KEY", None),
                ("OPENAI_MODEL", Some("llama3.1")),
            ],
            || match Backend::from_env("ignored-fallback") {
                Ok(Backend::OpenAiCompat(b)) => {
                    assert_eq!(b.base_url(), "http://localhost:11434/v1");
                    assert_eq!(b.model(), "llama3.1");
                    assert!(!b.has_api_key());
                }
                _ => panic!("ollama should select OpenAiCompat"),
            },
        );
    }

    #[test]
    fn backend_from_env_openai_without_key_is_config_error() {
        // EX-5134: a hosted provider missing its key fails loudly, never a silent
        // unauthenticated call.
        with_env(
            &[
                ("LLM_BACKEND", Some("openai")),
                ("OPENAI_API_KEY", None),
                ("OPENAI_BASE_URL", None),
            ],
            || {
                assert!(matches!(
                    Backend::from_env("gpt-4o-mini"),
                    Err(LlmError::Config(_))
                ));
            },
        );
    }

    // ── Pure provider-profile resolution (no env, no races) ──────────────────

    #[test]
    fn build_openai_compat_openai_needs_key() {
        let r = build_openai_compatible(OpenAiProvider::OpenAi, None, None, None, "m".into());
        assert!(matches!(r, Err(LlmError::Config(_))));
    }

    #[test]
    fn build_openai_compat_openai_with_key_resolves_defaults() {
        let b = build_openai_compatible(
            OpenAiProvider::OpenAi,
            None,
            Some("sk-live".into()),
            None,
            "gpt-4o-mini".into(),
        )
        .expect("key present → ok");
        assert_eq!(b.base_url(), "https://api.openai.com/v1");
        assert_eq!(b.model(), "gpt-4o-mini"); // fallback model used
        assert!(b.has_api_key());
    }

    #[test]
    fn build_openai_compat_base_url_override_reaches_other_providers() {
        // One adapter, many providers: point `openai` at Groq via OPENAI_BASE_URL.
        let b = build_openai_compatible(
            OpenAiProvider::OpenAi,
            Some("https://api.groq.com/openai/v1".into()),
            Some("gsk-x".into()),
            Some("llama-3.3-70b-versatile".into()),
            "fallback".into(),
        )
        .expect("ok");
        assert_eq!(b.base_url(), "https://api.groq.com/openai/v1");
        assert_eq!(b.model(), "llama-3.3-70b-versatile"); // env model wins over fallback
    }

    #[test]
    fn build_openai_compat_ollama_no_key_ok() {
        let b = build_openai_compatible(OpenAiProvider::Ollama, None, None, None, "llama3".into())
            .expect("ollama needs no key");
        assert_eq!(b.base_url(), "http://localhost:11434/v1");
        assert!(!b.has_api_key());
        assert_eq!(b.model(), "llama3");
    }

    #[test]
    fn build_openai_compat_empty_key_treated_as_absent() {
        // An exported-but-empty OPENAI_API_KEY must not pass the required-key gate.
        let r = build_openai_compatible(
            OpenAiProvider::OpenAi,
            None,
            Some(String::new()),
            None,
            "m".into(),
        );
        assert!(matches!(r, Err(LlmError::Config(_))));
    }

    // ── End-to-end: the selectable router dispatches to a live OpenAI-compatible
    //    endpoint (wiremock, no network, no env). Phase 4 of EX-5134. ──────────

    #[tokio::test]
    async fn router_dispatches_to_openai_compatible_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "pong"},
                "finish_reason": "stop"
            }]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        // Select the OpenAI-compatible variant of the runtime router and drive it.
        let backend = Backend::OpenAiCompat(OpenAiBackend::with_config(
            "test-model",
            format!("{}/v1", server.uri()),
            None,
        ));
        let out = backend
            .complete("ping", &LlmParams::default())
            .await
            .expect("round-trip");
        assert_eq!(out, "pong");
    }
}
