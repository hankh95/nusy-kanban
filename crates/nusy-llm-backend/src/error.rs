//! Error types for the LLM backend.

/// Errors that can occur during LLM inference.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// HTTP request failed (network error, DNS, connection refused).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// API returned a non-success status code.
    #[error("API error (status {status}): {message}")]
    Api { status: u16, message: String },

    /// Rate limited by the API. Includes retry-after duration if provided.
    #[error("Rate limited: retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },

    /// Failed to parse the API response.
    #[error("Response parse error: {0}")]
    Parse(String),

    /// Missing required configuration (e.g., API key not set).
    #[error("Configuration error: {0}")]
    Config(String),

    /// The model returned an empty or invalid response.
    #[error("Empty response from model")]
    EmptyResponse,

    /// CH-4406 — model output was truncated because it hit the
    /// `max_tokens` cap mid-response. The text up to the cap is
    /// well-formed model output, but downstream parsers (e.g.
    /// `serde_json::from_str`) will fail on the truncated tail.
    /// Callers should raise `LlmParams::max_tokens` and retry, or
    /// surface the failure to the user with the actionable hint.
    #[error(
        "Model output truncated at max_tokens cap (output_tokens={output_tokens}, max_tokens={max_tokens}). \
         Raise `--max-tokens` and retry."
    )]
    TruncatedOutput {
        /// Number of tokens the model actually generated before being cut off.
        output_tokens: u32,
        /// The `max_tokens` cap configured on the request that was hit.
        max_tokens: u32,
    },
}

/// Convenience alias for `std::result::Result<T, LlmError>`.
pub type Result<T> = std::result::Result<T, LlmError>;
