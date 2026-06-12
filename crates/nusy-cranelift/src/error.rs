//! Error types for the nusy-cranelift frontend.

/// Errors produced by the Cranelift frontend.
#[derive(Debug, thiserror::Error)]
pub enum CraneliftError {
    /// Rust syntax not supported in the restricted DSL subset.
    #[error("unsupported syntax: {0}")]
    UnsupportedSyntax(String),

    /// `syn` failed to parse the function body string.
    #[error("parse error: {0}")]
    ParseError(String),

    /// Cranelift IR construction failed.
    #[error("IR error: {0}")]
    IrError(String),

    /// JIT compilation failed.
    #[error("compile error: {0}")]
    CompileError(String),

    /// The CodeNode has no body string to compile.
    #[error("node has no body")]
    MissingBody,

    /// WASM module construction failed.
    #[error("WASM error: {0}")]
    WasmError(String),

    /// Sandboxed execution exceeded the configured timeout.
    #[error("execution timeout ({0:?})")]
    ExecutionTimeout(std::time::Duration),
}
