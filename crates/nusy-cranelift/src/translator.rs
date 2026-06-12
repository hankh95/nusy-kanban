//! syn AST → Cranelift IR translator (restricted Rust DSL subset).
//!
//! ## Supported subset
//!
//! | Category           | Example                              |
//! |--------------------|--------------------------------------|
//! | Primitive params   | `a: i32`, `b: i64`, `x: f32`, `y: f64` |
//! | Arithmetic         | `a + b`, `a - b`, `a * b`, `a / b`  |
//! | Comparison         | `a > b`, `a < b`, `a == b`, etc.    |
//! | Let binding        | `let x: i64 = a + b;`               |
//! | If / else          | `if cond { e1 } else { e2 }`        |
//! | Return             | `return expr` or trailing expr      |
//! | Literals           | `42`, `0.7`, `-1`                   |
//!
//! ## Unsupported → `Err(UnsupportedSyntax(...))`
//!
//! Generics, async, closures, trait objects, references (&T), method calls,
//! pattern matching, loop/while/for, std types (String, Vec, …).

use std::collections::HashMap;

use cranelift::prelude::*;
use cranelift_codegen::ir::BlockArg;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use syn::{BinOp, Expr, Lit, ReturnType, Stmt, Type, UnOp};

use crate::error::CraneliftError;

// ── Type mapping ──────────────────────────────────────────────────────────────

/// Map a `syn::Type` to a Cranelift scalar type.
///
/// Only primitive numeric types are supported. Everything else returns
/// `Err(UnsupportedSyntax(...))`.
pub(crate) fn map_type(ty: &Type) -> Result<types::Type, CraneliftError> {
    let Type::Path(tp) = ty else {
        return Err(CraneliftError::UnsupportedSyntax(
            "non-path type (reference, slice, tuple, etc.) is not supported".into(),
        ));
    };
    let seg = tp
        .path
        .segments
        .last()
        .ok_or_else(|| CraneliftError::UnsupportedSyntax("empty type path".into()))?;
    match seg.ident.to_string().as_str() {
        "i32" => Ok(types::I32),
        "i64" | "isize" => Ok(types::I64),
        "u32" => Ok(types::I32),
        "u64" | "usize" => Ok(types::I64),
        "f32" => Ok(types::F32),
        "f64" => Ok(types::F64),
        "bool" => Ok(types::I8),
        other => Err(CraneliftError::UnsupportedSyntax(format!(
            "unsupported type: {other}"
        ))),
    }
}

/// Check a `syn::Type` for references (&T, &mut T) and return Err immediately.
fn reject_reference(ty: &Type) -> Result<(), CraneliftError> {
    if matches!(ty, Type::Reference(_)) {
        return Err(CraneliftError::UnsupportedSyntax(
            "reference types (&T) are not supported".into(),
        ));
    }
    Ok(())
}

// ── Variable context ──────────────────────────────────────────────────────────

/// Per-function translation context: variable → (Cranelift Value, its type).
type VarMap = HashMap<String, (Value, types::Type)>;

// ── Expression translator ─────────────────────────────────────────────────────

/// Translate a `syn::Expr` into a Cranelift (Value, type) pair.
///
/// `builder` must be positioned in an active block.
pub(crate) fn translate_expr(
    expr: &Expr,
    builder: &mut FunctionBuilder<'_>,
    vars: &VarMap,
) -> Result<(Value, types::Type), CraneliftError> {
    match expr {
        // ── Literals ──────────────────────────────────────────────────────
        Expr::Lit(e) => match &e.lit {
            Lit::Int(i) => {
                let v: i64 = i
                    .base10_parse()
                    .map_err(|e| CraneliftError::ParseError(format!("bad int literal: {e}")))?;
                Ok((builder.ins().iconst(types::I64, v), types::I64))
            }
            Lit::Float(f) => {
                let v: f64 = f
                    .base10_parse()
                    .map_err(|e| CraneliftError::ParseError(format!("bad float literal: {e}")))?;
                Ok((builder.ins().f64const(v), types::F64))
            }
            Lit::Bool(b) => {
                let v = if b.value { 1i64 } else { 0i64 };
                Ok((builder.ins().iconst(types::I8, v), types::I8))
            }
            _ => Err(CraneliftError::UnsupportedSyntax(
                "unsupported literal (only int, float, bool)".into(),
            )),
        },

        // ── Identifier / path reference ───────────────────────────────────
        Expr::Path(e) => {
            if e.path.segments.len() != 1 {
                return Err(CraneliftError::UnsupportedSyntax(
                    "multi-segment paths not supported".into(),
                ));
            }
            let name = e.path.segments[0].ident.to_string();
            vars.get(&name)
                .copied()
                .ok_or_else(|| CraneliftError::IrError(format!("undefined variable: {name}")))
        }

        // ── Unary operators ───────────────────────────────────────────────
        Expr::Unary(e) => {
            let (val, ty) = translate_expr(&e.expr, builder, vars)?;
            match &e.op {
                UnOp::Neg(_) => {
                    if ty == types::F32 || ty == types::F64 {
                        Ok((builder.ins().fneg(val), ty))
                    } else {
                        Ok((builder.ins().ineg(val), ty))
                    }
                }
                UnOp::Not(_) => Ok((builder.ins().bnot(val), ty)),
                _ => Err(CraneliftError::UnsupportedSyntax(
                    "unsupported unary operator (only - and !)".into(),
                )),
            }
        }

        // ── Binary operators ──────────────────────────────────────────────
        Expr::Binary(e) => {
            let (lhs, lty) = translate_expr(&e.left, builder, vars)?;
            let (rhs, rty) = translate_expr(&e.right, builder, vars)?;

            // Coerce I64 literal to match the other side's float type.
            let (lhs, lty, rhs, _rty) = coerce_types(lhs, lty, rhs, rty, builder)?;

            translate_binop(&e.op, lhs, lty, rhs, builder)
        }

        // ── If / else ─────────────────────────────────────────────────────
        Expr::If(e) => {
            // Must have an else branch to produce a value.
            let else_branch = e.else_branch.as_ref().ok_or_else(|| {
                CraneliftError::UnsupportedSyntax("if without else cannot produce a value".into())
            })?;

            // Translate condition.
            let (cond, _) = translate_expr(&e.cond, builder, vars)?;

            let then_block = builder.create_block();
            let else_block = builder.create_block();
            let merge_block = builder.create_block();

            builder.ins().brif(cond, then_block, &[], else_block, &[]);

            // Then branch.
            builder.switch_to_block(then_block);
            builder.seal_block(then_block);
            let (then_val, then_ty) = translate_block(&e.then_branch, builder, vars)?
                .ok_or_else(|| CraneliftError::IrError("then branch produced no value".into()))?;
            builder.append_block_param(merge_block, then_ty);
            builder
                .ins()
                .jump(merge_block, &[BlockArg::Value(then_val)]);

            // Else branch.
            builder.switch_to_block(else_block);
            builder.seal_block(else_block);
            let (else_val, else_ty) = match else_branch.1.as_ref() {
                Expr::Block(eb) => translate_block(&eb.block, builder, vars)?.ok_or_else(|| {
                    CraneliftError::IrError("else branch produced no value".into())
                })?,
                other => translate_expr(other, builder, vars)?,
            };
            // Guard: both branches must produce the same type.
            if else_ty != then_ty {
                return Err(CraneliftError::UnsupportedSyntax(format!(
                    "if/else branch type mismatch: then={then_ty}, else={else_ty}"
                )));
            }
            builder
                .ins()
                .jump(merge_block, &[BlockArg::Value(else_val)]);

            // Merge.
            builder.switch_to_block(merge_block);
            builder.seal_block(merge_block);
            let result = builder.block_params(merge_block)[0];
            Ok((result, then_ty))
        }

        // ── Block expression ──────────────────────────────────────────────
        Expr::Block(e) => translate_block(&e.block, builder, vars)?
            .ok_or_else(|| CraneliftError::UnsupportedSyntax("empty block expression".into())),

        // ── Explicit return ────────────────────────────────────────────────
        Expr::Return(e) => {
            let val = if let Some(expr) = &e.expr {
                let (v, _) = translate_expr(expr, builder, vars)?;
                v
            } else {
                builder.ins().iconst(types::I64, 0)
            };
            builder.ins().return_(&[val]);
            // Return a dummy — the block is terminated so this won't be used.
            Ok((val, types::I64))
        }

        // ── Parenthesised expression ───────────────────────────────────────
        Expr::Paren(e) => translate_expr(&e.expr, builder, vars),

        // ── Catch-all ─────────────────────────────────────────────────────
        other => Err(CraneliftError::UnsupportedSyntax(format!(
            "unsupported expression kind: {}",
            expr_kind_name(other)
        ))),
    }
}

/// Translate a `syn::Block` and return the value of the last expression, if any.
pub(crate) fn translate_block(
    block: &syn::Block,
    builder: &mut FunctionBuilder<'_>,
    vars: &VarMap,
) -> Result<Option<(Value, types::Type)>, CraneliftError> {
    let mut local_vars = vars.clone();
    let mut last: Option<(Value, types::Type)> = None;

    for stmt in &block.stmts {
        last = translate_stmt(stmt, builder, &mut local_vars)?;
    }

    Ok(last)
}

/// Translate a single statement.
fn translate_stmt(
    stmt: &Stmt,
    builder: &mut FunctionBuilder<'_>,
    vars: &mut VarMap,
) -> Result<Option<(Value, types::Type)>, CraneliftError> {
    match stmt {
        // `let x: T = expr;`
        Stmt::Local(local) => {
            let name = match &local.pat {
                syn::Pat::Ident(pi) => pi.ident.to_string(),
                syn::Pat::Type(pt) => match pt.pat.as_ref() {
                    syn::Pat::Ident(pi) => pi.ident.to_string(),
                    _ => {
                        return Err(CraneliftError::UnsupportedSyntax(
                            "complex let pattern".into(),
                        ));
                    }
                },
                _ => {
                    return Err(CraneliftError::UnsupportedSyntax(
                        "complex let pattern".into(),
                    ));
                }
            };

            // Explicit type annotation (optional).
            let hint_ty: Option<types::Type> = match &local.pat {
                syn::Pat::Type(pt) => {
                    reject_reference(&pt.ty)?;
                    Some(map_type(&pt.ty)?)
                }
                _ => None,
            };

            let init = local.init.as_ref().ok_or_else(|| {
                CraneliftError::UnsupportedSyntax("let without initializer".into())
            })?;
            let (val, mut ty) = translate_expr(&init.expr, builder, vars)?;

            // If a type hint was given and types differ, attempt a cast.
            if let Some(ht) = hint_ty {
                let (cast_val, cast_ty) = cast_value(val, ty, ht, builder)?;
                ty = cast_ty;
                vars.insert(name, (cast_val, ty));
            } else {
                vars.insert(name, (val, ty));
            }
            Ok(None)
        }

        // Bare expression statement.
        Stmt::Expr(expr, semi) => {
            let result = translate_expr(expr, builder, vars)?;
            if semi.is_some() {
                Ok(None)
            } else {
                // Trailing expression — this is the block's return value.
                Ok(Some(result))
            }
        }

        // Item definitions inside blocks — unsupported.
        Stmt::Item(_) => Err(CraneliftError::UnsupportedSyntax(
            "item definitions inside blocks are not supported".into(),
        )),

        Stmt::Macro(_) => Err(CraneliftError::UnsupportedSyntax(
            "macro invocations are not supported".into(),
        )),
    }
}

// ── Binary op dispatch ────────────────────────────────────────────────────────

fn translate_binop(
    op: &BinOp,
    lhs: Value,
    lty: types::Type,
    rhs: Value,
    builder: &mut FunctionBuilder<'_>,
) -> Result<(Value, types::Type), CraneliftError> {
    let is_float = lty == types::F32 || lty == types::F64;

    let result = match op {
        BinOp::Add(_) => {
            if is_float {
                builder.ins().fadd(lhs, rhs)
            } else {
                builder.ins().iadd(lhs, rhs)
            }
        }
        BinOp::Sub(_) => {
            if is_float {
                builder.ins().fsub(lhs, rhs)
            } else {
                builder.ins().isub(lhs, rhs)
            }
        }
        BinOp::Mul(_) => {
            if is_float {
                builder.ins().fmul(lhs, rhs)
            } else {
                builder.ins().imul(lhs, rhs)
            }
        }
        BinOp::Div(_) => {
            if is_float {
                builder.ins().fdiv(lhs, rhs)
            } else {
                builder.ins().sdiv(lhs, rhs)
            }
        }
        BinOp::Rem(_) => {
            if is_float {
                return Err(CraneliftError::UnsupportedSyntax(
                    "float remainder not supported".into(),
                ));
            }
            builder.ins().srem(lhs, rhs)
        }
        // Comparison ops → I8 (0 or 1)
        BinOp::Lt(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::LessThan, lhs, rhs)
            } else {
                builder.ins().icmp(IntCC::SignedLessThan, lhs, rhs)
            }
        }
        BinOp::Gt(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::GreaterThan, lhs, rhs)
            } else {
                builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs)
            }
        }
        BinOp::Le(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::LessThanOrEqual, lhs, rhs)
            } else {
                builder.ins().icmp(IntCC::SignedLessThanOrEqual, lhs, rhs)
            }
        }
        BinOp::Ge(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs)
            } else {
                builder
                    .ins()
                    .icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs)
            }
        }
        BinOp::Eq(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::Equal, lhs, rhs)
            } else {
                builder.ins().icmp(IntCC::Equal, lhs, rhs)
            }
        }
        BinOp::Ne(_) => {
            if is_float {
                builder.ins().fcmp(FloatCC::NotEqual, lhs, rhs)
            } else {
                builder.ins().icmp(IntCC::NotEqual, lhs, rhs)
            }
        }
        // Boolean ops — computed eagerly (no short-circuit in DSL context)
        BinOp::And(_) => builder.ins().band(lhs, rhs),
        BinOp::Or(_) => builder.ins().bor(lhs, rhs),
        _ => {
            return Err(CraneliftError::UnsupportedSyntax(
                "unsupported binary operator".into(),
            ));
        }
    };

    // Comparison ops produce I8; arithmetic preserves lty.
    let out_ty = match op {
        BinOp::Lt(_) | BinOp::Gt(_) | BinOp::Le(_) | BinOp::Ge(_) | BinOp::Eq(_) | BinOp::Ne(_) => {
            types::I8
        }
        _ => lty,
    };

    Ok((result, out_ty))
}

// ── Type coercion helpers ─────────────────────────────────────────────────────

/// Coerce (lhs, lty) and (rhs, rty) to a common type for a binary op.
///
/// Strategy:
/// - If types match, return as-is.
/// - I64 literal + F64 var → promote literal to F64.
/// - I64 literal + F32 var → promote literal to F32.
/// - Anything else → error.
fn coerce_types(
    lhs: Value,
    lty: types::Type,
    rhs: Value,
    rty: types::Type,
    builder: &mut FunctionBuilder<'_>,
) -> Result<(Value, types::Type, Value, types::Type), CraneliftError> {
    if lty == rty {
        return Ok((lhs, lty, rhs, rty));
    }

    // Promote I64 to float if the other side is float.
    match (lty, rty) {
        (types::I64, types::F64) => {
            let promoted = builder.ins().fcvt_from_sint(types::F64, lhs);
            Ok((promoted, types::F64, rhs, types::F64))
        }
        (types::F64, types::I64) => {
            let promoted = builder.ins().fcvt_from_sint(types::F64, rhs);
            Ok((lhs, types::F64, promoted, types::F64))
        }
        (types::I64, types::F32) => {
            let promoted = builder.ins().fcvt_from_sint(types::F32, lhs);
            Ok((promoted, types::F32, rhs, types::F32))
        }
        (types::F32, types::I64) => {
            let promoted = builder.ins().fcvt_from_sint(types::F32, rhs);
            Ok((lhs, types::F32, promoted, types::F32))
        }
        (types::I32, types::I64) => {
            let extended = builder.ins().sextend(types::I64, lhs);
            Ok((extended, types::I64, rhs, types::I64))
        }
        (types::I64, types::I32) => {
            let extended = builder.ins().sextend(types::I64, rhs);
            Ok((lhs, types::I64, extended, types::I64))
        }
        _ => Err(CraneliftError::UnsupportedSyntax(format!(
            "incompatible types in binary expression: {lty} and {rty}"
        ))),
    }
}

/// Cast a value from `src_ty` to `dst_ty`.
fn cast_value(
    val: Value,
    src_ty: types::Type,
    dst_ty: types::Type,
    builder: &mut FunctionBuilder<'_>,
) -> Result<(Value, types::Type), CraneliftError> {
    if src_ty == dst_ty {
        return Ok((val, dst_ty));
    }
    match (src_ty, dst_ty) {
        (types::I64, types::F64) => Ok((builder.ins().fcvt_from_sint(types::F64, val), types::F64)),
        (types::I64, types::F32) => Ok((builder.ins().fcvt_from_sint(types::F32, val), types::F32)),
        (types::I64, types::I32) => Ok((builder.ins().ireduce(types::I32, val), types::I32)),
        (types::I32, types::I64) => Ok((builder.ins().sextend(types::I64, val), types::I64)),
        (types::F64, types::F32) => Ok((builder.ins().fdemote(types::F32, val), types::F32)),
        (types::F32, types::F64) => Ok((builder.ins().fpromote(types::F64, val), types::F64)),
        _ => Err(CraneliftError::UnsupportedSyntax(format!(
            "cannot cast {src_ty} to {dst_ty}"
        ))),
    }
}

// ── Full-function translation ─────────────────────────────────────────────────

/// Translate a `syn::ItemFn` into a JIT-compiled function.
///
/// Returns `(FuncId, ir_text)` where `ir_text` is the CLIF textual IR for debugging.
pub(crate) fn translate_item_fn(
    item_fn: &syn::ItemFn,
    module: &mut JITModule,
) -> Result<(cranelift_module::FuncId, String), CraneliftError> {
    // ── Reject unsupported function features ─────────────────────────────

    if item_fn.sig.asyncness.is_some() {
        return Err(CraneliftError::UnsupportedSyntax(
            "async functions are not supported".into(),
        ));
    }
    if !item_fn.sig.generics.params.is_empty() {
        return Err(CraneliftError::UnsupportedSyntax(format!(
            "generic functions are not supported (found {} type param(s))",
            item_fn.sig.generics.params.len()
        )));
    }

    // ── Build Cranelift signature ─────────────────────────────────────────

    let mut param_types: Vec<(String, types::Type)> = Vec::new();
    for input in &item_fn.sig.inputs {
        match input {
            syn::FnArg::Receiver(_) => {
                return Err(CraneliftError::UnsupportedSyntax(
                    "methods with self are not supported".into(),
                ));
            }
            syn::FnArg::Typed(pt) => {
                reject_reference(&pt.ty)?;
                let cty = map_type(&pt.ty)?;
                let name = match pt.pat.as_ref() {
                    syn::Pat::Ident(pi) => pi.ident.to_string(),
                    _ => {
                        return Err(CraneliftError::UnsupportedSyntax(
                            "complex parameter pattern".into(),
                        ));
                    }
                };
                param_types.push((name, cty));
            }
        }
    }

    let ret_ty: Option<types::Type> = match &item_fn.sig.output {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => {
            reject_reference(ty)?;
            Some(map_type(ty)?)
        }
    };

    // Build Cranelift ABI signature.
    let mut sig = module.make_signature();
    for (_, cty) in &param_types {
        sig.params.push(AbiParam::new(*cty));
    }
    if let Some(rty) = ret_ty {
        sig.returns.push(AbiParam::new(rty));
    }

    let fn_name = item_fn.sig.ident.to_string();
    let func_id = module
        .declare_function(&fn_name, Linkage::Local, &sig)
        .map_err(|e| CraneliftError::CompileError(e.to_string()))?;

    // ── Build function body ───────────────────────────────────────────────

    let mut ctx = module.make_context();
    ctx.func.signature = sig;

    let mut func_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut func_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        // Populate vars with function parameters.
        let mut vars: VarMap = HashMap::new();
        for (i, (name, cty)) in param_types.iter().enumerate() {
            let val = builder.block_params(entry)[i];
            vars.insert(name.clone(), (val, *cty));
        }

        // Translate function body.
        let block_result = translate_block(&item_fn.block, &mut builder, &vars)?;

        // Emit return if the block produced a value.
        // (If the last stmt was an explicit `return`, the block is already terminated
        // but finalize() handles that gracefully.)
        match block_result {
            Some((val, _)) if ret_ty.is_some() => {
                builder.ins().return_(&[val]);
            }
            None if ret_ty.is_none() => {
                builder.ins().return_(&[]);
            }
            _ => {
                builder.ins().return_(&[]);
            }
        }

        builder.finalize();
    }

    // ── Capture textual IR ────────────────────────────────────────────────

    let ir_text = ctx.func.display().to_string();

    // ── JIT compile ───────────────────────────────────────────────────────

    module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CraneliftError::CompileError(e.to_string()))?;
    module.clear_context(&mut ctx);

    Ok((func_id, ir_text))
}

// ── JIT module factory ────────────────────────────────────────────────────────

/// Create a fresh `JITModule` targeting the host ISA.
pub(crate) fn make_jit_module() -> Result<JITModule, CraneliftError> {
    let mut flag_builder = settings::builder();
    flag_builder
        .set("use_colocated_libcalls", "false")
        .map_err(|e| CraneliftError::CompileError(format!("flag error: {e}")))?;
    flag_builder
        .set("is_pic", "false")
        .map_err(|e| CraneliftError::CompileError(format!("flag error: {e}")))?;
    let isa = cranelift_native::builder()
        .map_err(|e| CraneliftError::CompileError(e.to_string()))?
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| CraneliftError::CompileError(e.to_string()))?;
    let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    Ok(JITModule::new(builder))
}

// ── Helper: expression kind name ─────────────────────────────────────────────

fn expr_kind_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::Array(_) => "Array",
        Expr::Assign(_) => "Assign",
        Expr::Async(_) => "Async",
        Expr::Await(_) => "Await",
        Expr::Break(_) => "Break",
        Expr::Call(_) => "Call (function calls require callee declaration)",
        Expr::Cast(_) => "Cast (as)",
        Expr::Closure(_) => "Closure",
        Expr::Const(_) => "Const",
        Expr::Continue(_) => "Continue",
        Expr::Field(_) => "Field (struct field access)",
        Expr::ForLoop(_) => "ForLoop",
        Expr::Group(_) => "Group",
        Expr::Index(_) => "Index",
        Expr::Infer(_) => "Infer",
        Expr::Let(_) => "Let-guard",
        Expr::Loop(_) => "Loop",
        Expr::Macro(_) => "Macro",
        Expr::Match(_) => "Match",
        Expr::MethodCall(_) => "MethodCall",
        Expr::Range(_) => "Range",
        Expr::RawAddr(_) => "RawAddr",
        Expr::Reference(_) => "Reference (&expr)",
        Expr::Repeat(_) => "Repeat",
        Expr::Struct(_) => "Struct literal",
        Expr::Try(_) => "Try (?)",
        Expr::TryBlock(_) => "TryBlock",
        Expr::Tuple(_) => "Tuple",
        Expr::Unsafe(_) => "Unsafe",
        Expr::Verbatim(_) => "Verbatim",
        Expr::While(_) => "While",
        Expr::Yield(_) => "Yield",
        _ => "Unknown",
    }
}
