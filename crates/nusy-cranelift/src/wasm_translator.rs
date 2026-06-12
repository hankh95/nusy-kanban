//! syn AST -> WASM bytecode translator (restricted Rust DSL subset).
//!
//! Mirrors the supported subset of `translator.rs` (Cranelift path) but emits
//! WASM instructions via `wasm_encoder` instead of Cranelift IR.
//!
//! ## Supported subset
//!
//! | Category         | Example                                 |
//! |------------------|-----------------------------------------|
//! | Primitive params | `a: i32`, `b: i64`, `x: f32`, `y: f64` |
//! | Arithmetic       | `a + b`, `a - b`, `a * b`, `a / b`      |
//! | Comparison       | `a > b`, `a < b`, `a == b`, etc.        |
//! | Let binding      | `let x: i64 = a + b;`                   |
//! | If / else        | `if cond { e1 } else { e2 }`            |
//! | Return           | `return expr` or trailing expr          |
//! | Literals         | `42`, `0.7`, `-1`                       |

use std::collections::HashMap;

use syn::{BinOp, Expr, Lit, ReturnType, Stmt, Type, UnOp};
use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    Module, TypeSection, ValType,
};

use crate::error::CraneliftError;

// ── Type mapping ──────────────────────────────────────────────────────────────

/// Map a `syn::Type` path segment to a WASM ValType.
fn map_type(ty: &Type) -> Result<ValType, CraneliftError> {
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
        "i32" | "u32" | "bool" => Ok(ValType::I32),
        "i64" | "u64" | "isize" | "usize" => Ok(ValType::I64),
        "f32" => Ok(ValType::F32),
        "f64" => Ok(ValType::F64),
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

// ── Translation context ───────────────────────────────────────────────────────

/// Tracks local variables: name -> (local_index, ValType).
struct WasmCtx {
    /// All locals (params first, then let-bindings).
    locals: HashMap<String, (u32, ValType)>,
    /// Next available local index.
    next_local: u32,
    /// Extra (non-param) locals to declare — collected during translation.
    extra_locals: Vec<ValType>,
    /// The function return type (for type inference on stack).
    return_type: Option<ValType>,
}

impl WasmCtx {
    fn new(params: &[(String, ValType)], return_type: Option<ValType>) -> Self {
        let mut locals = HashMap::new();
        for (i, (name, vt)) in params.iter().enumerate() {
            locals.insert(name.clone(), (i as u32, *vt));
        }
        WasmCtx {
            locals,
            next_local: params.len() as u32,
            extra_locals: Vec::new(),
            return_type,
        }
    }

    /// Allocate a new local variable (for let bindings).
    fn alloc_local(&mut self, name: String, vt: ValType) -> u32 {
        let idx = self.next_local;
        self.next_local += 1;
        self.extra_locals.push(vt);
        self.locals.insert(name, (idx, vt));
        idx
    }

    fn get(&self, name: &str) -> Result<(u32, ValType), CraneliftError> {
        self.locals
            .get(name)
            .copied()
            .ok_or_else(|| CraneliftError::IrError(format!("undefined variable: {name}")))
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Emit a complete WASM module from a `syn::ItemFn`.
///
/// The resulting bytes are a valid WASM binary with a single exported function.
pub fn emit_wasm(item_fn: &syn::ItemFn) -> Result<Vec<u8>, CraneliftError> {
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

    // ── Extract parameter types ──────────────────────────────────────────

    let mut params: Vec<(String, ValType)> = Vec::new();
    for input in &item_fn.sig.inputs {
        match input {
            syn::FnArg::Receiver(_) => {
                return Err(CraneliftError::UnsupportedSyntax(
                    "methods with self are not supported".into(),
                ));
            }
            syn::FnArg::Typed(pt) => {
                reject_reference(&pt.ty)?;
                let vt = map_type(&pt.ty)?;
                let name = match pt.pat.as_ref() {
                    syn::Pat::Ident(pi) => pi.ident.to_string(),
                    _ => {
                        return Err(CraneliftError::UnsupportedSyntax(
                            "complex parameter pattern".into(),
                        ));
                    }
                };
                params.push((name, vt));
            }
        }
    }

    let ret_ty: Option<ValType> = match &item_fn.sig.output {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => {
            reject_reference(ty)?;
            Some(map_type(ty)?)
        }
    };

    // ── Build WASM function body ─────────────────────────────────────────

    let mut ctx = WasmCtx::new(&params, ret_ty);
    let mut instructions: Vec<Instruction<'static>> = Vec::new();

    translate_block(&item_fn.block, &mut ctx, &mut instructions)?;

    instructions.push(Instruction::End);

    // ── Assemble WASM module ─────────────────────────────────────────────

    let param_types: Vec<ValType> = params.iter().map(|(_, vt)| *vt).collect();
    let result_types: Vec<ValType> = ret_ty.into_iter().collect();

    let mut type_section = TypeSection::new();
    type_section.ty().function(param_types, result_types);

    let mut function_section = FunctionSection::new();
    function_section.function(0); // type index 0

    let mut export_section = ExportSection::new();
    let fn_name = item_fn.sig.ident.to_string();
    export_section.export(&fn_name, ExportKind::Func, 0);

    let mut code_section = CodeSection::new();
    let mut func = Function::new(
        ctx.extra_locals
            .iter()
            .map(|vt| (1, *vt))
            .collect::<Vec<_>>(),
    );
    for instr in &instructions {
        func.instruction(instr);
    }
    code_section.function(&func);

    let mut module = Module::new();
    module.section(&type_section);
    module.section(&function_section);
    module.section(&export_section);
    module.section(&code_section);

    Ok(module.finish())
}

// ── Block translation ────────────────────────────────────────────────────────

fn translate_block(
    block: &syn::Block,
    ctx: &mut WasmCtx,
    out: &mut Vec<Instruction<'static>>,
) -> Result<Option<ValType>, CraneliftError> {
    let mut last_type: Option<ValType> = None;
    let stmts = &block.stmts;

    for (i, stmt) in stmts.iter().enumerate() {
        let is_last = i == stmts.len() - 1;
        last_type = translate_stmt(stmt, ctx, out, is_last)?;
    }

    Ok(last_type)
}

// ── Statement translation ─────────────────────────────────────────────────────

fn translate_stmt(
    stmt: &Stmt,
    ctx: &mut WasmCtx,
    out: &mut Vec<Instruction<'static>>,
    is_last: bool,
) -> Result<Option<ValType>, CraneliftError> {
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
            let hint_ty: Option<ValType> = match &local.pat {
                syn::Pat::Type(pt) => {
                    reject_reference(&pt.ty)?;
                    Some(map_type(&pt.ty)?)
                }
                _ => None,
            };

            let init = local.init.as_ref().ok_or_else(|| {
                CraneliftError::UnsupportedSyntax("let without initializer".into())
            })?;

            let expr_ty = translate_expr(&init.expr, ctx, out)?;

            // Determine the local's type: explicit hint wins, otherwise inferred from expr.
            let local_ty = hint_ty.unwrap_or(expr_ty);

            // Insert a conversion if needed (e.g., i64 literal assigned to f64 local).
            if local_ty != expr_ty {
                emit_cast(expr_ty, local_ty, out)?;
            }

            let idx = ctx.alloc_local(name, local_ty);
            out.push(Instruction::LocalSet(idx));
            Ok(None)
        }

        // Bare expression statement.
        Stmt::Expr(expr, semi) => {
            let ty = translate_expr(expr, ctx, out)?;
            if semi.is_some() {
                // Statement with semicolon — drop the value from the stack.
                out.push(Instruction::Drop);
                Ok(None)
            } else if is_last {
                // Trailing expression — this is the block's return value.
                Ok(Some(ty))
            } else {
                // Not the last statement but no semicolon — still drop.
                out.push(Instruction::Drop);
                Ok(None)
            }
        }

        Stmt::Item(_) => Err(CraneliftError::UnsupportedSyntax(
            "item definitions inside blocks are not supported".into(),
        )),
        Stmt::Macro(_) => Err(CraneliftError::UnsupportedSyntax(
            "macro invocations are not supported".into(),
        )),
    }
}

// ── Expression translation ──────────────────────────────────────────────────

/// Translate a `syn::Expr` and push the resulting value onto the WASM stack.
/// Returns the ValType of the value pushed.
fn translate_expr(
    expr: &Expr,
    ctx: &mut WasmCtx,
    out: &mut Vec<Instruction<'static>>,
) -> Result<ValType, CraneliftError> {
    match expr {
        // ── Literals ──────────────────────────────────────────────────────
        Expr::Lit(e) => match &e.lit {
            Lit::Int(i) => {
                let v: i64 = i
                    .base10_parse()
                    .map_err(|e| CraneliftError::ParseError(format!("bad int literal: {e}")))?;
                // Use the suffix if present, otherwise default to i64.
                let suffix = i.suffix();
                match suffix {
                    "i32" | "u32" => {
                        out.push(Instruction::I32Const(v as i32));
                        Ok(ValType::I32)
                    }
                    "f32" => {
                        out.push(Instruction::F32Const(v as f32));
                        Ok(ValType::F32)
                    }
                    "f64" => {
                        out.push(Instruction::F64Const(v as f64));
                        Ok(ValType::F64)
                    }
                    _ => {
                        // Default: i64
                        out.push(Instruction::I64Const(v));
                        Ok(ValType::I64)
                    }
                }
            }
            Lit::Float(f) => {
                let v: f64 = f
                    .base10_parse()
                    .map_err(|e| CraneliftError::ParseError(format!("bad float literal: {e}")))?;
                let suffix = f.suffix();
                if suffix == "f32" {
                    out.push(Instruction::F32Const(v as f32));
                    Ok(ValType::F32)
                } else {
                    out.push(Instruction::F64Const(v));
                    Ok(ValType::F64)
                }
            }
            Lit::Bool(b) => {
                let v = if b.value { 1i32 } else { 0i32 };
                out.push(Instruction::I32Const(v));
                Ok(ValType::I32)
            }
            _ => Err(CraneliftError::UnsupportedSyntax(
                "unsupported literal (only int, float, bool)".into(),
            )),
        },

        // ── Identifier / path reference ────────────────────────────────
        Expr::Path(e) => {
            if e.path.segments.len() != 1 {
                return Err(CraneliftError::UnsupportedSyntax(
                    "multi-segment paths not supported".into(),
                ));
            }
            let name = e.path.segments[0].ident.to_string();
            let (idx, vt) = ctx.get(&name)?;
            out.push(Instruction::LocalGet(idx));
            Ok(vt)
        }

        // ── Unary operators ────────────────────────────────────────────
        Expr::Unary(e) => {
            let ty = translate_expr(&e.expr, ctx, out)?;
            match &e.op {
                UnOp::Neg(_) => match ty {
                    ValType::I32 => {
                        // 0 - x
                        // We need to push 0 first, then the value, then sub.
                        // But the value is already on the stack. Insert 0 before it:
                        // Actually, we need a different approach. The value is on the stack.
                        // We can do: i32.const 0, swap... WASM doesn't have swap.
                        // Alternative: store to temp, push 0, load temp, sub.
                        // Simpler: use (0 - x) pattern by using a temp local.
                        let temp = ctx.alloc_local(format!("__neg_tmp_{}", ctx.next_local), ty);
                        out.push(Instruction::LocalSet(temp));
                        out.push(Instruction::I32Const(0));
                        out.push(Instruction::LocalGet(temp));
                        out.push(Instruction::I32Sub);
                        Ok(ValType::I32)
                    }
                    ValType::I64 => {
                        let temp = ctx.alloc_local(format!("__neg_tmp_{}", ctx.next_local), ty);
                        out.push(Instruction::LocalSet(temp));
                        out.push(Instruction::I64Const(0));
                        out.push(Instruction::LocalGet(temp));
                        out.push(Instruction::I64Sub);
                        Ok(ValType::I64)
                    }
                    ValType::F32 => {
                        out.push(Instruction::F32Neg);
                        Ok(ValType::F32)
                    }
                    ValType::F64 => {
                        out.push(Instruction::F64Neg);
                        Ok(ValType::F64)
                    }
                    _ => Err(CraneliftError::UnsupportedSyntax(format!(
                        "cannot negate type {ty:?}"
                    ))),
                },
                UnOp::Not(_) => {
                    // Boolean not: xor with 1
                    match ty {
                        ValType::I32 => {
                            out.push(Instruction::I32Const(1));
                            out.push(Instruction::I32Xor);
                            Ok(ValType::I32)
                        }
                        ValType::I64 => {
                            out.push(Instruction::I64Const(1));
                            out.push(Instruction::I64Xor);
                            Ok(ValType::I64)
                        }
                        _ => Err(CraneliftError::UnsupportedSyntax(
                            "logical not on non-integer type".into(),
                        )),
                    }
                }
                _ => Err(CraneliftError::UnsupportedSyntax(
                    "unsupported unary operator (only - and !)".into(),
                )),
            }
        }

        // ── Binary operators ───────────────────────────────────────────
        Expr::Binary(e) => {
            let lty = translate_expr(&e.left, ctx, out)?;
            let rty = translate_expr(&e.right, ctx, out)?;

            // Coerce types if needed — for WASM, we need both operands to be the same type.
            let common_ty = coerce_binary_types(lty, rty, ctx, out)?;

            emit_binop(&e.op, common_ty, out)
        }

        // ── If / else ──────────────────────────────────────────────────
        Expr::If(e) => {
            let else_branch = e.else_branch.as_ref().ok_or_else(|| {
                CraneliftError::UnsupportedSyntax("if without else cannot produce a value".into())
            })?;

            // Evaluate condition.
            let cond_ty = translate_expr(&e.cond, ctx, out)?;

            // WASM `if` expects i32 on the stack. If condition is i64, wrap != 0.
            if cond_ty == ValType::I64 {
                out.push(Instruction::I64Const(0));
                out.push(Instruction::I64Ne);
            }

            // Determine the result type from the return type hint.
            let result_type = ctx.return_type.unwrap_or(ValType::I64);

            out.push(Instruction::If(BlockType::Result(result_type)));

            // Then branch.
            let then_ty = translate_block_inner(&e.then_branch, ctx, out)?;
            if let Some(then_vt) = then_ty
                && then_vt != result_type
            {
                emit_cast(then_vt, result_type, out)?;
            }

            out.push(Instruction::Else);

            // Else branch.
            let else_ty = match else_branch.1.as_ref() {
                Expr::Block(eb) => translate_block_inner(&eb.block, ctx, out)?,
                other => Some(translate_expr(other, ctx, out)?),
            };
            if let Some(else_vt) = else_ty
                && else_vt != result_type
            {
                emit_cast(else_vt, result_type, out)?;
            }

            out.push(Instruction::End);
            Ok(result_type)
        }

        // ── Block expression ───────────────────────────────────────────
        Expr::Block(e) => {
            let ty = translate_block_inner(&e.block, ctx, out)?;
            Ok(ty.unwrap_or(ValType::I64))
        }

        // ── Explicit return ────────────────────────────────────────────
        Expr::Return(e) => {
            if let Some(expr) = &e.expr {
                let ty = translate_expr(expr, ctx, out)?;
                // Cast to return type if needed.
                if let Some(ret_ty) = ctx.return_type
                    && ty != ret_ty
                {
                    emit_cast(ty, ret_ty, out)?;
                }
            }
            out.push(Instruction::Return);
            // Return a dummy type — the block is terminated.
            Ok(ctx.return_type.unwrap_or(ValType::I64))
        }

        // ── Parenthesised expression ───────────────────────────────────
        Expr::Paren(e) => translate_expr(&e.expr, ctx, out),

        // ── Catch-all ──────────────────────────────────────────────────
        other => Err(CraneliftError::UnsupportedSyntax(format!(
            "unsupported expression kind: {}",
            expr_kind_name(other)
        ))),
    }
}

// ── Inner block translation (without implicit return logic) ──────────────────

/// Translate a block's statements — returns the ValType of the trailing expression, if any.
fn translate_block_inner(
    block: &syn::Block,
    ctx: &mut WasmCtx,
    out: &mut Vec<Instruction<'static>>,
) -> Result<Option<ValType>, CraneliftError> {
    let mut last_type: Option<ValType> = None;
    let stmts = &block.stmts;

    for (i, stmt) in stmts.iter().enumerate() {
        let is_last = i == stmts.len() - 1;
        last_type = translate_stmt(stmt, ctx, out, is_last)?;
    }

    Ok(last_type)
}

// ── Binary operator emission ────────────────────────────────────────────────

fn emit_binop(
    op: &BinOp,
    ty: ValType,
    out: &mut Vec<Instruction<'static>>,
) -> Result<ValType, CraneliftError> {
    let is_float = ty == ValType::F32 || ty == ValType::F64;

    match op {
        BinOp::Add(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Add),
            ValType::I64 => out.push(Instruction::I64Add),
            ValType::F32 => out.push(Instruction::F32Add),
            ValType::F64 => out.push(Instruction::F64Add),
            _ => return Err(unsupported_type_for_op("add", ty)),
        },
        BinOp::Sub(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Sub),
            ValType::I64 => out.push(Instruction::I64Sub),
            ValType::F32 => out.push(Instruction::F32Sub),
            ValType::F64 => out.push(Instruction::F64Sub),
            _ => return Err(unsupported_type_for_op("sub", ty)),
        },
        BinOp::Mul(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Mul),
            ValType::I64 => out.push(Instruction::I64Mul),
            ValType::F32 => out.push(Instruction::F32Mul),
            ValType::F64 => out.push(Instruction::F64Mul),
            _ => return Err(unsupported_type_for_op("mul", ty)),
        },
        BinOp::Div(_) => match ty {
            ValType::I32 => out.push(Instruction::I32DivS),
            ValType::I64 => out.push(Instruction::I64DivS),
            ValType::F32 => out.push(Instruction::F32Div),
            ValType::F64 => out.push(Instruction::F64Div),
            _ => return Err(unsupported_type_for_op("div", ty)),
        },
        BinOp::Rem(_) => {
            if is_float {
                return Err(CraneliftError::UnsupportedSyntax(
                    "float remainder not supported".into(),
                ));
            }
            match ty {
                ValType::I32 => out.push(Instruction::I32RemS),
                ValType::I64 => out.push(Instruction::I64RemS),
                _ => return Err(unsupported_type_for_op("rem", ty)),
            }
        }
        // Comparison operators → I32 (0 or 1)
        BinOp::Lt(_) => match ty {
            ValType::I32 => out.push(Instruction::I32LtS),
            ValType::I64 => out.push(Instruction::I64LtS),
            ValType::F32 => out.push(Instruction::F32Lt),
            ValType::F64 => out.push(Instruction::F64Lt),
            _ => return Err(unsupported_type_for_op("lt", ty)),
        },
        BinOp::Gt(_) => match ty {
            ValType::I32 => out.push(Instruction::I32GtS),
            ValType::I64 => out.push(Instruction::I64GtS),
            ValType::F32 => out.push(Instruction::F32Gt),
            ValType::F64 => out.push(Instruction::F64Gt),
            _ => return Err(unsupported_type_for_op("gt", ty)),
        },
        BinOp::Le(_) => match ty {
            ValType::I32 => out.push(Instruction::I32LeS),
            ValType::I64 => out.push(Instruction::I64LeS),
            ValType::F32 => out.push(Instruction::F32Le),
            ValType::F64 => out.push(Instruction::F64Le),
            _ => return Err(unsupported_type_for_op("le", ty)),
        },
        BinOp::Ge(_) => match ty {
            ValType::I32 => out.push(Instruction::I32GeS),
            ValType::I64 => out.push(Instruction::I64GeS),
            ValType::F32 => out.push(Instruction::F32Ge),
            ValType::F64 => out.push(Instruction::F64Ge),
            _ => return Err(unsupported_type_for_op("ge", ty)),
        },
        BinOp::Eq(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Eq),
            ValType::I64 => out.push(Instruction::I64Eq),
            ValType::F32 => out.push(Instruction::F32Eq),
            ValType::F64 => out.push(Instruction::F64Eq),
            _ => return Err(unsupported_type_for_op("eq", ty)),
        },
        BinOp::Ne(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Ne),
            ValType::I64 => out.push(Instruction::I64Ne),
            ValType::F32 => out.push(Instruction::F32Ne),
            ValType::F64 => out.push(Instruction::F64Ne),
            _ => return Err(unsupported_type_for_op("ne", ty)),
        },
        // Boolean ops (eager, no short-circuit).
        BinOp::And(_) => match ty {
            ValType::I32 => out.push(Instruction::I32And),
            ValType::I64 => out.push(Instruction::I64And),
            _ => return Err(unsupported_type_for_op("and", ty)),
        },
        BinOp::Or(_) => match ty {
            ValType::I32 => out.push(Instruction::I32Or),
            ValType::I64 => out.push(Instruction::I64Or),
            _ => return Err(unsupported_type_for_op("or", ty)),
        },
        _ => {
            return Err(CraneliftError::UnsupportedSyntax(
                "unsupported binary operator".into(),
            ));
        }
    }

    // Comparison ops produce I32; arithmetic preserves the input type.
    let out_ty = match op {
        BinOp::Lt(_) | BinOp::Gt(_) | BinOp::Le(_) | BinOp::Ge(_) | BinOp::Eq(_) | BinOp::Ne(_) => {
            ValType::I32
        }
        _ => ty,
    };

    Ok(out_ty)
}

// ── Type coercion ───────────────────────────────────────────────────────────

/// Coerce the two top-of-stack values to a common type for a binary op.
///
/// If types match, returns as-is. Otherwise attempts promotion (I64 → F64, etc.)
/// by inserting conversion instructions and a temp local to swap stack order.
fn coerce_binary_types(
    lty: ValType,
    rty: ValType,
    ctx: &mut WasmCtx,
    out: &mut Vec<Instruction<'static>>,
) -> Result<ValType, CraneliftError> {
    if lty == rty {
        return Ok(lty);
    }

    // Stack layout: [..., lhs, rhs]
    // We need to convert one of them. The rhs is on top.

    match (lty, rty) {
        // rhs is I64, lhs is F64 → convert rhs (top of stack) to F64
        (ValType::F64, ValType::I64) => {
            out.push(Instruction::F64ConvertI64S);
            Ok(ValType::F64)
        }
        // lhs is I64, rhs is F64 → need to convert lhs (under rhs on stack)
        (ValType::I64, ValType::F64) => {
            // Save rhs to temp, convert lhs, restore rhs.
            let temp = ctx.alloc_local(format!("__coerce_tmp_{}", ctx.next_local), rty);
            out.push(Instruction::LocalSet(temp)); // save rhs
            out.push(Instruction::F64ConvertI64S); // convert lhs
            out.push(Instruction::LocalGet(temp)); // restore rhs
            Ok(ValType::F64)
        }
        (ValType::F32, ValType::I64) => {
            out.push(Instruction::F32ConvertI64S);
            Ok(ValType::F32)
        }
        (ValType::I64, ValType::F32) => {
            let temp = ctx.alloc_local(format!("__coerce_tmp_{}", ctx.next_local), rty);
            out.push(Instruction::LocalSet(temp));
            out.push(Instruction::F32ConvertI64S);
            out.push(Instruction::LocalGet(temp));
            Ok(ValType::F32)
        }
        // I32 ↔ I64 promotion
        (ValType::I32, ValType::I64) => {
            // rhs is i64, lhs is i32 — convert lhs
            let temp = ctx.alloc_local(format!("__coerce_tmp_{}", ctx.next_local), rty);
            out.push(Instruction::LocalSet(temp));
            out.push(Instruction::I64ExtendI32S);
            out.push(Instruction::LocalGet(temp));
            Ok(ValType::I64)
        }
        (ValType::I64, ValType::I32) => {
            // rhs is i32, convert rhs to i64
            out.push(Instruction::I64ExtendI32S);
            Ok(ValType::I64)
        }
        _ => Err(CraneliftError::UnsupportedSyntax(format!(
            "incompatible types in binary expression: {lty:?} and {rty:?}"
        ))),
    }
}

// ── Type casting ────────────────────────────────────────────────────────────

/// Emit conversion instructions from `src` to `dst`.
fn emit_cast(
    src: ValType,
    dst: ValType,
    out: &mut Vec<Instruction<'static>>,
) -> Result<(), CraneliftError> {
    if src == dst {
        return Ok(());
    }
    match (src, dst) {
        (ValType::I64, ValType::F64) => out.push(Instruction::F64ConvertI64S),
        (ValType::I64, ValType::F32) => out.push(Instruction::F32ConvertI64S),
        (ValType::I64, ValType::I32) => out.push(Instruction::I32WrapI64),
        (ValType::I32, ValType::I64) => out.push(Instruction::I64ExtendI32S),
        (ValType::F64, ValType::F32) => out.push(Instruction::F32DemoteF64),
        (ValType::F32, ValType::F64) => out.push(Instruction::F64PromoteF32),
        (ValType::I32, ValType::F64) => out.push(Instruction::F64ConvertI32S),
        (ValType::I32, ValType::F32) => out.push(Instruction::F32ConvertI32S),
        _ => {
            return Err(CraneliftError::UnsupportedSyntax(format!(
                "cannot cast {src:?} to {dst:?}"
            )));
        }
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn unsupported_type_for_op(op: &str, ty: ValType) -> CraneliftError {
    CraneliftError::UnsupportedSyntax(format!("unsupported type for {op}: {ty:?}"))
}

fn expr_kind_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::Array(_) => "Array",
        Expr::Assign(_) => "Assign",
        Expr::Async(_) => "Async",
        Expr::Await(_) => "Await",
        Expr::Break(_) => "Break",
        Expr::Call(_) => "Call",
        Expr::Cast(_) => "Cast",
        Expr::Closure(_) => "Closure",
        Expr::Const(_) => "Const",
        Expr::Continue(_) => "Continue",
        Expr::Field(_) => "Field",
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
        Expr::Reference(_) => "Reference",
        Expr::Repeat(_) => "Repeat",
        Expr::Struct(_) => "Struct literal",
        Expr::Try(_) => "Try",
        Expr::TryBlock(_) => "TryBlock",
        Expr::Tuple(_) => "Tuple",
        Expr::Unsafe(_) => "Unsafe",
        Expr::Verbatim(_) => "Verbatim",
        Expr::While(_) => "While",
        Expr::Yield(_) => "Yield",
        _ => "Unknown",
    }
}
