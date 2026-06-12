//! `CraneliftFrontend` — public API for CodeNode → Cranelift IR compilation.
//!
//! EX-3176 Phase 1 & 2.

use nusy_codegraph::schema::CodeNode;

use crate::error::CraneliftError;
use crate::translator::{make_jit_module, translate_item_fn};

// ── Output types ──────────────────────────────────────────────────────────────

/// The output of `node_to_ir`: textual CLIF IR for debugging + metadata.
#[derive(Debug, Clone)]
pub struct CompilationUnit {
    /// Node ID from the graph (propagated from `CodeNode::id`).
    pub node_id: String,
    /// Function name as declared in the source.
    pub function_name: String,
    /// Textual CLIF IR (Cranelift Intermediate Format), for debugging.
    pub cranelift_ir: String,
}

/// A successfully JIT-compiled function.
///
/// The raw pointer is valid for the lifetime of the `CraneliftFrontend` that
/// produced it. **Calling it with the wrong signature is undefined behaviour.**
pub struct CompiledFunction {
    pub node_id: String,
    pub function_name: String,
    /// Raw pointer to the JIT-compiled native code.
    /// Caller is responsible for transmuting to the correct `fn` type.
    pub code_ptr: *const u8,
}

// SAFETY: the raw code pointer points to JIT-allocated memory (mmaped executable
// pages). It is inherently not Send/Sync, but for testing we need to move it
// across test threads. In production this should be wrapped in a safe handle.
unsafe impl Send for CompiledFunction {}
unsafe impl Sync for CompiledFunction {}

// ── CraneliftFrontend ─────────────────────────────────────────────────────────

/// Graph-native Cranelift JIT frontend.
///
/// Translates a `CodeNode` (containing a Rust function body string) into
/// Cranelift IR, then optionally JIT-compiles it to native machine code.
///
/// **Supported Rust subset:**
/// - Primitive params: `i32`, `i64`, `f32`, `f64`, `bool`
/// - Arithmetic: `+`, `-`, `*`, `/`, `%`
/// - Comparison: `<`, `>`, `<=`, `>=`, `==`, `!=`
/// - Logical: `&&`, `||`, `!`
/// - Let bindings, if/else, return, literal integers/floats
///
/// **Unsupported → `Err(UnsupportedSyntax)`:**
/// Generics, async, closures, references (&T), method calls, pattern matching,
/// loops, std types (String, Vec, …), struct literals, field access.
pub struct CraneliftFrontend;

impl CraneliftFrontend {
    /// Create a new frontend (stateless — all state lives in per-call JIT modules).
    pub fn new() -> Result<Self, CraneliftError> {
        // Probe that the host ISA is available.
        cranelift_native::builder()
            .map_err(|e| CraneliftError::CompileError(format!("host ISA unavailable: {e}")))?;
        Ok(CraneliftFrontend)
    }

    /// Parse the node's body, build Cranelift IR, and return the textual CLIF.
    ///
    /// Does NOT JIT-compile — safe to call even in non-JIT contexts.
    pub fn node_to_ir(&self, node: &CodeNode) -> Result<CompilationUnit, CraneliftError> {
        let body = node.body.as_deref().ok_or(CraneliftError::MissingBody)?;
        let item_fn: syn::ItemFn =
            syn::parse_str(body).map_err(|e| CraneliftError::ParseError(e.to_string()))?;

        let mut module = make_jit_module()?;
        let (_, ir_text) = translate_item_fn(&item_fn, &mut module)?;
        // Finalize so we can safely drop the module.
        module
            .finalize_definitions()
            .map_err(|e| CraneliftError::CompileError(format!("finalize failed: {e}")))?;

        Ok(CompilationUnit {
            node_id: node.id.clone(),
            function_name: item_fn.sig.ident.to_string(),
            cranelift_ir: ir_text,
        })
    }

    /// Parse, build IR, JIT-compile, and return a raw function pointer.
    ///
    /// The returned `CompiledFunction::code_ptr` is valid until the frontend
    /// is dropped. **Transmute to the correct signature before calling.**
    ///
    /// Note: the JIT module is leaked here (intentionally for V13-1 scope).
    /// V13-2 will introduce a proper `CompiledModule` handle with Drop.
    pub fn compile_node(&self, node: &CodeNode) -> Result<CompiledFunction, CraneliftError> {
        let body = node.body.as_deref().ok_or(CraneliftError::MissingBody)?;
        let item_fn: syn::ItemFn =
            syn::parse_str(body).map_err(|e| CraneliftError::ParseError(e.to_string()))?;

        let fn_name = item_fn.sig.ident.to_string();
        let mut module = make_jit_module()?;
        let (func_id, _) = translate_item_fn(&item_fn, &mut module)?;
        module
            .finalize_definitions()
            .map_err(|e| CraneliftError::CompileError(format!("finalize failed: {e}")))?;

        let code_ptr = module.get_finalized_function(func_id);
        // Leak the module so the JIT memory stays live.
        // TODO(V13-2): replace with Arc<JitHandle> that owns the module.
        std::mem::forget(module);

        Ok(CompiledFunction {
            node_id: node.id.clone(),
            function_name: fn_name,
            code_ptr,
        })
    }
}
