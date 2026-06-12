//! V13-2 integration tests: WASM sandbox (EX-3177).
//!
//! 15+ tests covering compilation, execution, type support, error handling,
//! sandbox isolation, and parity with the native JIT path.

use std::time::Duration;

use nusy_codegraph::schema::{CodeNode, CodeNodeKind};
use nusy_cranelift::{CraneliftError, CraneliftFrontend, WasmCompiler, WasmValue};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_node(id: &str, body: &str) -> CodeNode {
    CodeNode {
        id: id.into(),
        kind: CodeNodeKind::RustFn,
        name: id.into(),
        body: Some(body.into()),
        ..CodeNode::default()
    }
}

fn compiler() -> WasmCompiler {
    WasmCompiler::new().expect("WasmCompiler::new")
}

// ── 1. Compile simple add ─────────────────────────────────────────────────────

#[test]
fn test_wasm_compile_simple_add() {
    let node = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");
    let module = compiler().compile(&node).expect("compile");
    assert_eq!(module.function_name, "add");
    assert!(
        !module.wasm_bytes.is_empty(),
        "WASM bytes must not be empty"
    );
    // Minimal WASM module header: \0asm
    assert_eq!(
        &module.wasm_bytes[..4],
        b"\0asm",
        "must be valid WASM magic"
    );
}

// ── 2. Execute add ────────────────────────────────────────────────────────────

#[test]
fn test_wasm_execute_add() {
    let node = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");
    let c = compiler();
    let result = c
        .compile_and_run(&node, &[WasmValue::I64(3), WasmValue::I64(4)])
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I64(7));
}

// ── 3. Execute multiply ───────────────────────────────────────────────────────

#[test]
fn test_wasm_execute_multiply() {
    let node = make_node("mul", "pub fn mul(a: i64, b: i64) -> i64 { a * b }");
    let c = compiler();
    let result = c
        .compile_and_run(&node, &[WasmValue::I64(6), WasmValue::I64(7)])
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I64(42));
}

// ── 4. Execute if/else ────────────────────────────────────────────────────────

#[test]
fn test_wasm_execute_if_else() {
    let node = make_node(
        "max",
        "pub fn max(a: i64, b: i64) -> i64 { if a > b { a } else { b } }",
    );
    let c = compiler();

    let r1 = c
        .compile_and_run(&node, &[WasmValue::I64(10), WasmValue::I64(5)])
        .expect("10 > 5");
    assert_eq!(r1, WasmValue::I64(10));

    let r2 = c
        .compile_and_run(&node, &[WasmValue::I64(3), WasmValue::I64(8)])
        .expect("3 < 8");
    assert_eq!(r2, WasmValue::I64(8));

    let r3 = c
        .compile_and_run(&node, &[WasmValue::I64(7), WasmValue::I64(7)])
        .expect("7 == 7");
    assert_eq!(r3, WasmValue::I64(7));
}

// ── 5. Execute let binding ────────────────────────────────────────────────────

#[test]
fn test_wasm_execute_let_binding() {
    let node = make_node(
        "triple",
        "pub fn triple(x: i64) -> i64 { let t = x * 3; t }",
    );
    let c = compiler();
    let result = c
        .compile_and_run(&node, &[WasmValue::I64(5)])
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I64(15));
}

// ── 6. Execute float arithmetic ───────────────────────────────────────────────

#[test]
fn test_wasm_execute_float() {
    let node = make_node(
        "weighted_sum",
        "pub fn weighted_sum(a: f64, b: f64) -> f64 { 0.7 * a + 0.3 * b }",
    );
    let c = compiler();
    let result = c
        .compile_and_run(&node, &[WasmValue::F64(10.0), WasmValue::F64(20.0)])
        .expect("compile_and_run");
    match result {
        WasmValue::F64(v) => {
            let expected = 0.7 * 10.0 + 0.3 * 20.0; // 13.0
            assert!(
                (v - expected).abs() < 1e-10,
                "expected ~{expected}, got {v}"
            );
        }
        other => panic!("expected F64, got {other:?}"),
    }
}

// ── 7. Execute bool comparison ────────────────────────────────────────────────

#[test]
fn test_wasm_execute_bool_comparison() {
    // Comparison operators return i32 (0 or 1) in WASM.
    let node = make_node(
        "is_positive",
        "pub fn is_positive(v: i64) -> i32 { if v > 0 { 1 } else { 0 } }",
    );
    let c = compiler();

    let r1 = c
        .compile_and_run(&node, &[WasmValue::I64(5)])
        .expect("positive");
    assert_eq!(r1, WasmValue::I32(1));

    let r2 = c
        .compile_and_run(&node, &[WasmValue::I64(-3)])
        .expect("negative");
    assert_eq!(r2, WasmValue::I32(0));
}

// ── 8. Unsupported syntax errors ──────────────────────────────────────────────

#[test]
fn test_wasm_unsupported_syntax_errors() {
    let c = compiler();

    // Generics
    let node = make_node(
        "generic",
        "pub fn generic<T: Ord>(a: T, b: T) -> T { if a > b { a } else { b } }",
    );
    let err = c.compile(&node).expect_err("generics must fail");
    assert!(matches!(err, CraneliftError::UnsupportedSyntax(_)));

    // Closures
    let node = make_node(
        "closure",
        "pub fn closure(x: i64) -> i64 { let f = |v: i64| v + 1; f(x) }",
    );
    let err = c.compile(&node).expect_err("closures must fail");
    assert!(matches!(err, CraneliftError::UnsupportedSyntax(_)));

    // References
    let node = make_node("refs", "pub fn refs(s: &str) -> i64 { 0 }");
    let err = c.compile(&node).expect_err("references must fail");
    assert!(matches!(err, CraneliftError::UnsupportedSyntax(_)));
}

// ── 9. Missing body errors ────────────────────────────────────────────────────

#[test]
fn test_wasm_missing_body_errors() {
    let node = CodeNode {
        id: "no-body".into(),
        kind: CodeNodeKind::RustFn,
        name: "no_body".into(),
        body: None,
        ..CodeNode::default()
    };
    let err = compiler()
        .compile(&node)
        .expect_err("missing body must fail");
    assert!(
        matches!(err, CraneliftError::MissingBody),
        "expected MissingBody, got: {err}"
    );
}

// ── 10. Timeout enforcement ───────────────────────────────────────────────────

#[test]
fn test_wasm_timeout_enforcement() {
    // Create a function with an infinite loop.
    // We can't write `loop {}` because loops are UnsupportedSyntax in our DSL.
    // Instead, use the wasmtime engine directly with a hand-crafted WASM module
    // that has an infinite loop.
    use wasm_encoder::{
        BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
        Module, TypeSection, ValType,
    };

    let mut type_section = TypeSection::new();
    type_section.ty().function(vec![], vec![ValType::I64]);

    let mut function_section = FunctionSection::new();
    function_section.function(0);

    let mut export_section = ExportSection::new();
    export_section.export("infinite", ExportKind::Func, 0);

    let mut code_section = CodeSection::new();
    let mut func = Function::new(vec![]);
    // loop { br 0 } — infinite loop in WASM
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::Br(0)); // branch to loop start
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::I64Const(0)); // unreachable, but needed for type
    func.instruction(&Instruction::End); // end function
    code_section.function(&func);

    let mut module = Module::new();
    module.section(&type_section);
    module.section(&function_section);
    module.section(&export_section);
    module.section(&code_section);
    let wasm_bytes = module.finish();

    // Use a short timeout.
    let c = WasmCompiler::with_timeout(Duration::from_millis(100)).expect("compiler");
    let wasm_module = nusy_cranelift::WasmModule {
        node_id: "infinite".into(),
        function_name: "infinite".into(),
        wasm_bytes,
    };

    let err = c.execute(&wasm_module, &[]).expect_err("must timeout");
    assert!(
        matches!(err, CraneliftError::ExecutionTimeout(_)),
        "expected ExecutionTimeout, got: {err}"
    );
}

// ── 11. No filesystem access ──────────────────────────────────────────────────

#[test]
fn test_wasm_no_filesystem_access() {
    // Our WASM modules are instantiated with NO imports at all (no WASI).
    // This test verifies that a compiled module has zero imports.
    let node = make_node("add", "pub fn add(a: i64, b: i64) -> i64 { a + b }");
    let module = compiler().compile(&node).expect("compile");

    // Parse the WASM binary and verify zero imports.
    let parser = wasmparser::Parser::new(0);
    for payload in parser.parse_all(&module.wasm_bytes) {
        let payload = payload.expect("valid WASM payload");
        if let wasmparser::Payload::ImportSection(reader) = payload {
            let import_count = reader.into_iter().count();
            assert_eq!(
                import_count, 0,
                "WASM module must have zero imports (no WASI)"
            );
        }
    }
    // If no import section at all, that's also a pass — means zero imports.
}

// ── 12. Results match native JIT ──────────────────────────────────────────────

#[test]
fn test_wasm_results_match_native() {
    let body =
        "pub fn compute(a: i64, b: i64) -> i64 { let sum = a + b; let diff = a - b; sum * diff }";
    let node = make_node("compute", body);

    // Native JIT path (V13-1).
    let fe = CraneliftFrontend::new().expect("CraneliftFrontend");
    let compiled = fe.compile_node(&node).expect("compile_node");
    let native_fn: fn(i64, i64) -> i64 = unsafe { std::mem::transmute(compiled.code_ptr) };
    let native_result = native_fn(10, 3);

    // WASM sandbox path (V13-2).
    let c = compiler();
    let wasm_result = c
        .compile_and_run(&node, &[WasmValue::I64(10), WasmValue::I64(3)])
        .expect("wasm compile_and_run");

    assert_eq!(
        wasm_result,
        WasmValue::I64(native_result),
        "WASM and native JIT must produce the same result"
    );
    // (10+3)*(10-3) = 13*7 = 91
    assert_eq!(native_result, 91);
}

// ── 13. Multiple params ───────────────────────────────────────────────────────

#[test]
fn test_wasm_multiple_params() {
    let node = make_node(
        "quad_sum",
        "pub fn quad_sum(a: i64, b: i64, c: i64, d: i64) -> i64 { a + b + c + d }",
    );
    let c = compiler();
    let result = c
        .compile_and_run(
            &node,
            &[
                WasmValue::I64(1),
                WasmValue::I64(2),
                WasmValue::I64(3),
                WasmValue::I64(4),
            ],
        )
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I64(10));
}

// ── 14. Nested if/else ────────────────────────────────────────────────────────

#[test]
fn test_wasm_nested_if_else() {
    let node = make_node(
        "classify",
        r#"pub fn classify(x: i64) -> i64 {
            if x > 0 {
                if x > 100 { 2 } else { 1 }
            } else {
                0
            }
        }"#,
    );
    let c = compiler();

    let r1 = c
        .compile_and_run(&node, &[WasmValue::I64(200)])
        .expect("x=200");
    assert_eq!(r1, WasmValue::I64(2));

    let r2 = c
        .compile_and_run(&node, &[WasmValue::I64(50)])
        .expect("x=50");
    assert_eq!(r2, WasmValue::I64(1));

    let r3 = c
        .compile_and_run(&node, &[WasmValue::I64(-5)])
        .expect("x=-5");
    assert_eq!(r3, WasmValue::I64(0));
}

// ── 15. compile_and_run convenience ───────────────────────────────────────────

#[test]
fn test_compile_and_run_convenience() {
    let node = make_node("sub", "pub fn sub(a: i64, b: i64) -> i64 { a - b }");
    let c = compiler();

    // compile_and_run is a single call.
    let result = c
        .compile_and_run(&node, &[WasmValue::I64(100), WasmValue::I64(42)])
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I64(58));

    // Verify it also works by splitting compile + execute.
    let module = c.compile(&node).expect("compile");
    let result2 = c
        .execute(&module, &[WasmValue::I64(100), WasmValue::I64(42)])
        .expect("execute");
    assert_eq!(result2, WasmValue::I64(58));
}

// ── 16. i32 arithmetic ───────────────────────────────────────────────────────

#[test]
fn test_wasm_i32_arithmetic() {
    let node = make_node("add32", "pub fn add32(a: i32, b: i32) -> i32 { a + b }");
    let c = compiler();
    let result = c
        .compile_and_run(&node, &[WasmValue::I32(10), WasmValue::I32(20)])
        .expect("compile_and_run");
    assert_eq!(result, WasmValue::I32(30));
}

// ── 17. Division and remainder ────────────────────────────────────────────────

#[test]
fn test_wasm_division_and_remainder() {
    let node_div = make_node("div", "pub fn div(a: i64, b: i64) -> i64 { a / b }");
    let node_rem = make_node("rem", "pub fn rem(a: i64, b: i64) -> i64 { a % b }");
    let c = compiler();

    let r_div = c
        .compile_and_run(&node_div, &[WasmValue::I64(17), WasmValue::I64(5)])
        .expect("div");
    assert_eq!(r_div, WasmValue::I64(3));

    let r_rem = c
        .compile_and_run(&node_rem, &[WasmValue::I64(17), WasmValue::I64(5)])
        .expect("rem");
    assert_eq!(r_rem, WasmValue::I64(2));
}

// ── 18. Clamp zero (let + if/else combined) ──────────────────────────────────

#[test]
fn test_wasm_clamp_zero() {
    let node = make_node(
        "clamp_zero",
        "pub fn clamp_zero(v: i64) -> i64 { if v < 0 { 0 } else { v } }",
    );
    let c = compiler();

    let r1 = c
        .compile_and_run(&node, &[WasmValue::I64(-10)])
        .expect("negative");
    assert_eq!(r1, WasmValue::I64(0));

    let r2 = c
        .compile_and_run(&node, &[WasmValue::I64(42)])
        .expect("positive");
    assert_eq!(r2, WasmValue::I64(42));
}
