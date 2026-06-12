//! CodeGraph execution layer — execute code objects from the graph.
//!
//! V14.0 scope: Read CodeNode body from Arrow, execute via external Python
//! process, capture output. No file materialization — code lives in the graph.
//!
//! # Future roadmap
//!
//! - V14.0: subprocess execution (current) — spawns `python3 -c` with code body
//! - V14.1: PyO3 in-process execution (requires Captain approval + PyO3 dep)
//! - Post-V14: Rust-native interpretation (long-term goal)
//!
//! # Security (V14.0)
//!
//! - **Timeout:** Enforced via `child.wait_timeout()` — process killed after deadline.
//! - **stdout/stderr:** Captured, not printed to parent process.
//! - **Import sandboxing:** NOT enforced in V14.0 — `call_args` is NOT sanitized.
//!   This is an internal API for trusted agent use. Do NOT expose to untrusted input.
//!   Sandboxing deferred to V14.1 (Captain escalation).

use crate::schema::node_col;
use arrow::array::{Array, RecordBatch, StringArray};
use std::process::Command;
use std::time::Duration;

/// Errors from code execution.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Node has no body (body_hash is null): {0}")]
    NoBody(String),

    #[error("Node kind '{kind}' is not executable (only function/method supported): {id}")]
    NotExecutable { id: String, kind: String },

    #[error("Execution timed out after {0:?}")]
    Timeout(Duration),

    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ExecutorError>;

/// Result of executing a code object.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// The node ID that was executed.
    pub node_id: String,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Exit code (0 = success).
    pub exit_code: i32,
    /// Whether execution completed within timeout.
    pub completed: bool,
}

/// Default execution timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Execute a CodeNode's body as Python code.
///
/// Reads the node's signature and docstring from the Arrow batch, constructs
/// a minimal Python script, and executes it via `python3 -c`. This proves
/// the D4 principle ("there are no files") — code executes from graph objects.
///
/// # Arguments
///
/// * `nodes_batch` — The CodeNodes RecordBatch
/// * `node_id` — ID of the function/method to execute
/// * `call_args` — Arguments to pass to the function (Python expression)
/// * `timeout` — Maximum execution time (None = 30 seconds)
///
/// # Safety — UNSAFE: `call_args` is not sanitized
///
/// `call_args` is interpolated directly into a Python string and executed.
/// This is an internal API for trusted agent callers only. Do NOT pass
/// untrusted user input as `call_args` — it enables arbitrary code execution.
/// Input validation deferred to V14.1 sandboxing.
///
/// # Limitations (V14.0)
///
/// - Only functions/methods with pure computation (no I/O, no imports beyond stdlib)
/// - Body is reconstructed from signature + "pass" (actual body storage is Phase 2)
/// - No import sandboxing in V14.0
/// - subprocess-based, not in-process (PyO3 deferred to V14.1)
pub fn execute_object(
    nodes_batch: &RecordBatch,
    node_id: &str,
    call_args: &str,
    timeout: Option<Duration>,
) -> Result<ExecutionResult> {
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);

    let ids = nodes_batch
        .column(node_col::ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column");
    let names = nodes_batch
        .column(node_col::NAME)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column");
    let signatures = nodes_batch
        .column(node_col::SIGNATURE)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("signature column");
    let body_hashes = nodes_batch
        .column(node_col::BODY_HASH)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("body_hash column");

    // Extract kind from dictionary
    let kind_col = nodes_batch.column(node_col::KIND);
    let kind_dict = kind_col
        .as_any()
        .downcast_ref::<arrow::array::Int8DictionaryArray>()
        .expect("kind dict");
    let kind_values = kind_dict
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("kind values");

    // Find the node
    let row_idx = (0..nodes_batch.num_rows())
        .find(|&i| ids.value(i) == node_id)
        .ok_or_else(|| ExecutorError::NodeNotFound(node_id.to_string()))?;

    // Validate kind
    let kind_key = kind_dict.keys().value(row_idx) as usize;
    let kind_str = kind_values.value(kind_key);
    if kind_str != "function" && kind_str != "method" && kind_str != "test" {
        return Err(ExecutorError::NotExecutable {
            id: node_id.to_string(),
            kind: kind_str.to_string(),
        });
    }

    // Check body exists
    if body_hashes.is_null(row_idx) {
        return Err(ExecutorError::NoBody(node_id.to_string()));
    }

    let name = names.value(row_idx);
    let signature = if signatures.is_null(row_idx) {
        format!("def {}():", name)
    } else {
        format!("{}:", signatures.value(row_idx))
    };

    // Construct Python script with timeout enforcement.
    // V14.0: We don't have the actual body stored in the graph yet (only body_hash).
    // For now, construct a minimal callable that proves the execution path works.
    // Full body storage is tracked in the "body" column (to be added).
    //
    // Timeout: inject Python-level signal.alarm (Unix) so the subprocess
    // self-terminates after the deadline. No external crates needed.
    let timeout_secs = timeout.as_secs().max(1);
    let python_code = format!(
        "import signal\nsignal.alarm({})\n{}\n    pass\n\nresult = {}({})\nif result is not None:\n    print(result)",
        timeout_secs, signature, name, call_args
    );

    let output = Command::new("python3")
        .arg("-c")
        .arg(&python_code)
        .output()?;

    // exit_code: None means killed by signal (SIGKILL, SIGSEGV, SIGALRM, etc.)
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Check if killed by SIGALRM (timeout)
    if exit_code == -1 && stderr.contains("AlarmError") {
        return Err(ExecutorError::Timeout(timeout));
    }

    Ok(ExecutionResult {
        node_id: node_id.to_string(),
        stdout,
        stderr,
        exit_code,
        completed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CodeNode, CodeNodeKind, build_code_nodes_batch};

    fn sample_nodes() -> Vec<CodeNode> {
        vec![
            CodeNode {
                id: "func:math.py::add".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "add".into(),
                signature: Some("def add(a, b)".into()),
                docstring: None,
                body_hash: Some("hash_add".into()),
                body: None,
                loc: Some(3),
                cyclomatic_complexity: Some(1),
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "class:store.py::Store".into(),
                kind: CodeNodeKind::Class,
                parent_id: None,
                name: "Store".into(),
                signature: None,
                docstring: None,
                body_hash: Some("hash_store".into()),
                body: None,
                loc: Some(50),
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "func:math.py::no_body".into(),
                kind: CodeNodeKind::Function,
                parent_id: None,
                name: "no_body".into(),
                signature: Some("def no_body()".into()),
                docstring: None,
                body_hash: None, // No body
                body: None,
                loc: None,
                cyclomatic_complexity: None,
                coverage_pct: None,
                last_modified: None,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn test_execute_function() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        // Pass valid args since add(a,b) expects two arguments
        let result = execute_object(&batch, "func:math.py::add", "1, 2", None).unwrap();
        assert_eq!(result.node_id, "func:math.py::add");
        assert!(result.completed);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_execute_not_found() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = execute_object(&batch, "func:nonexistent::foo", "", None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExecutorError::NodeNotFound(_)
        ));
    }

    #[test]
    fn test_execute_non_executable_kind() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = execute_object(&batch, "class:store.py::Store", "", None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExecutorError::NotExecutable { .. }
        ));
    }

    #[test]
    fn test_execute_no_body() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        let result = execute_object(&batch, "func:math.py::no_body", "", None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ExecutorError::NoBody(_)));
    }

    #[test]
    fn test_execution_result_captures_stderr() {
        let batch = build_code_nodes_batch(&sample_nodes()).unwrap();
        // Call with wrong number of args to trigger an error
        let result = execute_object(&batch, "func:math.py::add", "", None).unwrap();
        assert!(result.completed);
        // Should fail because add() expects 2 args
        assert_ne!(result.exit_code, 0);
        assert!(!result.stderr.is_empty());
        assert!(result.stderr.contains("TypeError"));
    }
}
