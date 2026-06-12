//! nusy-cranelift — Graph-native Cranelift JIT frontend + WASM sandbox.
//!
//! Takes a `CodeNode` from the NuSy code graph (containing a Rust function body
//! string) and produces either:
//! - **Cranelift IR + native JIT code** (V13-1, `CraneliftFrontend`)
//! - **WASM bytecode + sandboxed execution** (V13-2, `WasmCompiler`)
//!
//! ## Supported Rust DSL subset
//!
//! Primitive types (i32/i64/f32/f64/bool), arithmetic, comparisons,
//! let bindings, if/else, return, integer/float literals.
//!
//! ## Unsupported → `Err(UnsupportedSyntax)`
//!
//! Generics, async, closures, references, method calls, pattern matching,
//! loops, std heap types (String, Vec, …).
//!
//! ## Example (native JIT)
//!
//! ```rust,no_run
//! use nusy_cranelift::{CraneliftFrontend, CraneliftError};
//! use nusy_codegraph::schema::{CodeNode, CodeNodeKind};
//!
//! let node = CodeNode {
//!     id: "add-fn".into(),
//!     kind: CodeNodeKind::RustFn,
//!     name: "add".into(),
//!     body: Some("pub fn add(a: i64, b: i64) -> i64 { a + b }".into()),
//!     ..CodeNode::default()
//! };
//! let frontend = CraneliftFrontend::new().unwrap();
//! let unit = frontend.node_to_ir(&node).unwrap();
//! assert!(!unit.cranelift_ir.is_empty());
//! ```
//!
//! ## Example (WASM sandbox)
//!
//! ```rust,no_run
//! use nusy_cranelift::{WasmCompiler, WasmValue};
//! use nusy_codegraph::schema::{CodeNode, CodeNodeKind};
//!
//! let node = CodeNode {
//!     id: "add-fn".into(),
//!     kind: CodeNodeKind::RustFn,
//!     name: "add".into(),
//!     body: Some("pub fn add(a: i64, b: i64) -> i64 { a + b }".into()),
//!     ..CodeNode::default()
//! };
//! let compiler = WasmCompiler::new().unwrap();
//! let result = compiler.compile_and_run(&node, &[WasmValue::I64(3), WasmValue::I64(4)]).unwrap();
//! assert_eq!(result, WasmValue::I64(7));
//! ```

pub mod build_orchestrator;
pub mod cached_compiler;
pub mod error;
pub mod frontend;
pub mod test_runner;
pub(crate) mod translator;
pub mod wasm_compiler;
pub(crate) mod wasm_translator;

pub use build_orchestrator::{
    BuildConfig, BuildOrchestrator, CrateBuildReport, WorkspaceBuildReport,
};
pub use cached_compiler::{BuildReport, CachedStatsSnapshot, CachedWasmCompiler, build_workspace};
pub use error::CraneliftError;
pub use frontend::{CompilationUnit, CompiledFunction, CraneliftFrontend};
pub use test_runner::{
    TestResult, TestSuiteReport, run_all_tests, run_single_test, run_tests_for_crate,
};
pub use wasm_compiler::{WasmCompiler, WasmModule, WasmValue};
