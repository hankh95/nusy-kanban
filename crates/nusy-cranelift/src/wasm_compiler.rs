//! WASM compiler — produces sandboxed WASM modules from CodeNodes (V13-2, EX-3177).
//!
//! ## Architecture
//!
//! ```text
//! CodeNode.body (Rust DSL string)
//!     → syn::ItemFn (parse)
//!     → wasm-encoder (emit WASM bytecode)
//!     → wasmtime Engine (instantiate + execute in sandbox)
//! ```
//!
//! The WASM sandbox provides:
//! - **No filesystem access** — no WASI imports
//! - **No network access** — pure computation only
//! - **Timeout enforcement** — epoch-based interruption prevents infinite loops
//!
//! ## Example
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

use std::time::Duration;

use nusy_codegraph::schema::CodeNode;
use wasmtime::{Engine, Instance, Module, Store, Trap, Val};

use crate::error::CraneliftError;
use crate::wasm_translator;

/// Default execution timeout (5 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

// ── Public types ──────────────────────────────────────────────────────────────

/// WASM compiler — compiles CodeNode Rust DSL to WASM and executes in a sandbox.
///
/// The sandbox has:
/// - No WASI imports (no filesystem, no network, no env vars)
/// - Epoch-based timeout interruption
pub struct WasmCompiler {
    engine: Engine,
    timeout: Duration,
}

/// A compiled WASM module ready for sandboxed execution.
#[derive(Debug)]
pub struct WasmModule {
    /// Node ID from the code graph.
    pub node_id: String,
    /// Function name as declared in the source.
    pub function_name: String,
    /// Raw WASM bytecode.
    pub wasm_bytes: Vec<u8>,
}

/// Value types for WASM function arguments and returns.
#[derive(Debug, Clone, PartialEq)]
pub enum WasmValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl WasmValue {
    /// Convert to a wasmtime `Val`.
    fn to_val(&self) -> Val {
        match self {
            WasmValue::I32(v) => Val::I32(*v),
            WasmValue::I64(v) => Val::I64(*v),
            WasmValue::F32(v) => Val::F32(v.to_bits()),
            WasmValue::F64(v) => Val::F64(v.to_bits()),
        }
    }

    /// Convert from a wasmtime `Val`.
    fn from_val(val: &Val) -> Result<Self, CraneliftError> {
        match val {
            Val::I32(v) => Ok(WasmValue::I32(*v)),
            Val::I64(v) => Ok(WasmValue::I64(*v)),
            Val::F32(v) => Ok(WasmValue::F32(f32::from_bits(*v))),
            Val::F64(v) => Ok(WasmValue::F64(f64::from_bits(*v))),
            other => Err(CraneliftError::WasmError(format!(
                "unsupported return value type: {other:?}"
            ))),
        }
    }
}

// ── WasmCompiler implementation ──────────────────────────────────────────────

impl WasmCompiler {
    /// Create a new WASM compiler with the default timeout (5 seconds).
    pub fn new() -> Result<Self, CraneliftError> {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Create a new WASM compiler with a custom timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self, CraneliftError> {
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config)
            .map_err(|e| CraneliftError::WasmError(format!("engine creation failed: {e}")))?;
        Ok(WasmCompiler { engine, timeout })
    }

    /// Compile a CodeNode to WASM bytes.
    ///
    /// Parses the node's body as a Rust function, translates the syn AST to
    /// WASM instructions via `wasm-encoder`, and returns the raw WASM module bytes.
    pub fn compile(&self, node: &CodeNode) -> Result<WasmModule, CraneliftError> {
        let body = node.body.as_deref().ok_or(CraneliftError::MissingBody)?;
        let item_fn: syn::ItemFn =
            syn::parse_str(body).map_err(|e| CraneliftError::ParseError(e.to_string()))?;

        let wasm_bytes = wasm_translator::emit_wasm(&item_fn)?;

        Ok(WasmModule {
            node_id: node.id.clone(),
            function_name: item_fn.sig.ident.to_string(),
            wasm_bytes,
        })
    }

    /// Execute a compiled WASM function with the given arguments.
    ///
    /// The execution is fully sandboxed:
    /// - No WASI imports (no filesystem, no network)
    /// - Epoch-based timeout interruption
    pub fn execute(
        &self,
        wasm_module: &WasmModule,
        args: &[WasmValue],
    ) -> Result<WasmValue, CraneliftError> {
        // Validate the WASM module against the engine.
        let module = Module::new(&self.engine, &wasm_module.wasm_bytes)
            .map_err(|e| CraneliftError::WasmError(format!("module validation failed: {e}")))?;

        let mut store = Store::new(&self.engine, ());

        // Configure epoch deadline for timeout.
        store.set_epoch_deadline(1);

        // Spawn a background thread to increment the epoch after the timeout.
        let engine = self.engine.clone();
        let timeout = self.timeout;
        let epoch_thread = std::thread::spawn(move || {
            std::thread::sleep(timeout);
            engine.increment_epoch();
        });

        // Instantiate — no imports (no WASI, no filesystem, no network).
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|e| CraneliftError::WasmError(format!("instantiation failed: {e}")))?;

        // Get the exported function.
        let func = instance
            .get_func(&mut store, &wasm_module.function_name)
            .ok_or_else(|| {
                CraneliftError::WasmError(format!(
                    "function '{}' not found in WASM module",
                    wasm_module.function_name
                ))
            })?;

        // Convert args.
        let wasm_args: Vec<Val> = args.iter().map(|a| a.to_val()).collect();
        let mut results = vec![Val::I64(0)]; // placeholder

        // Determine expected result count from the function type.
        let func_ty = func.ty(&store);
        let result_count = func_ty.results().len();
        results.resize(result_count.max(1), Val::I64(0));

        // Call the function.
        let call_result = func.call(&mut store, &wasm_args, &mut results);

        // Clean up the epoch thread (non-blocking — it will terminate on its own).
        drop(epoch_thread);

        match call_result {
            Ok(()) => {
                if result_count == 0 {
                    // Void function — return I64(0) as sentinel.
                    Ok(WasmValue::I64(0))
                } else {
                    WasmValue::from_val(&results[0])
                }
            }
            Err(e) => {
                // Check if this was an epoch interruption (timeout).
                // wasmtime uses Trap::Interrupt for epoch-based interruption.
                if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
                    Err(CraneliftError::ExecutionTimeout(timeout))
                } else {
                    Err(CraneliftError::WasmError(format!("execution failed: {e}")))
                }
            }
        }
    }

    /// Compile and execute in one step — convenience method.
    pub fn compile_and_run(
        &self,
        node: &CodeNode,
        args: &[WasmValue],
    ) -> Result<WasmValue, CraneliftError> {
        let module = self.compile(node)?;
        self.execute(&module, args)
    }

    /// Return a reference to the underlying wasmtime Engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}
