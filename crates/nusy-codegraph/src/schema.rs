//! Arrow schemas for code graph objects.
//!
//! Two tables:
//! - **CodeNodes**: functions, classes, modules, files — the objects in the code graph
//! - **CodeEdges**: calls, imports, inheritance, containment — relationships between objects
//!
//! ## V12a-1 additions (EX-3168)
//!
//! **V12/V13 parity audit:** V12/V13 Python had no position metadata and no Rust-specific
//! node kinds. All position columns and Rust-specific `CodeNodeKind` variants are new work,
//! not ports from V12.
//!
//! Position columns (13–18) are nullable — backward-compatible with existing Parquet files.
//! New Parquet files produced by the Rust tree-sitter parser (EX-3120) will populate them.

use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

/// Default embedding dimension for code objects.
pub const CODE_EMBEDDING_DIM: i32 = 768;

// ─── CodeNode column indices ────────────────────────────────────────────────

/// Named column indices for the CodeNodes schema.
pub mod node_col {
    pub const ID: usize = 0;
    pub const KIND: usize = 1;
    pub const PARENT_ID: usize = 2;
    pub const NAME: usize = 3;
    pub const SIGNATURE: usize = 4;
    pub const DOCSTRING: usize = 5;
    pub const BODY_HASH: usize = 6;
    pub const BODY: usize = 7;
    pub const EMBEDDING: usize = 8;
    pub const LOC: usize = 9;
    pub const CYCLOMATIC_COMPLEXITY: usize = 10;
    pub const COVERAGE_PCT: usize = 11;
    pub const LAST_MODIFIED: usize = 12;
    // Position metadata (EX-3168 / V12a-1) — nullable, populated by tree-sitter parser
    pub const START_LINE: usize = 13;
    pub const END_LINE: usize = 14;
    pub const START_COL: usize = 15;
    pub const END_COL: usize = 16;
    pub const FILE_PATH: usize = 17;
    pub const BYTE_OFFSET: usize = 18;
}

// ─── CodeEdge column indices ────────────────────────────────────────────────

/// Named column indices for the CodeEdges schema.
pub mod edge_col {
    pub const SOURCE_ID: usize = 0;
    pub const TARGET_ID: usize = 1;
    pub const PREDICATE: usize = 2;
    pub const WEIGHT: usize = 3;
    pub const COMMIT_ID: usize = 4;
}

// ─── Enums ──────────────────────────────────────────────────────────────────

/// Kind of code object.
///
/// Variants are stored as Arrow `Dictionary<Int8, Utf8>` for efficient memory use.
///
/// ## Variant groups
///
/// **Language-agnostic** (original set — used by the Python parser):
/// `File`, `Module`, `Class`, `Function`, `Method`, `Parameter`, `Variable`, `Test`
///
/// **Generic** (EX-3168 / V12a-1 — language-independent additions):
/// `Type`, `Constant`, `Import`
///
/// **Rust-specific** (EX-3168 / V12a-1 — populated by tree-sitter parser in EX-3120):
/// `RustFn`, `RustMethod`, `RustImpl`, `RustTrait`, `RustStruct`, `RustEnum`,
/// `RustMod`, `RustMacro`, `RustUse`, `RustConst`, `RustStatic`, `RustTypeAlias`,
/// `RustAttribute`, `RustLifetime`, `RustTest`
///
/// **Python-specific** (EX-3172 / V12b-1 — populated by PythonParser with position metadata):
/// `PythonFunction`, `PythonMethod`, `PythonClass`, `PythonDecorator`, `PythonImport`,
/// `PythonModule`, `PythonLambda`, `PythonAsync`, `PythonProperty`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeNodeKind {
    // ── Language-agnostic (original set, used by Python parser) ───────────
    File,
    Module,
    Class,
    Function,
    Method,
    Parameter,
    Variable,
    Test,
    // ── Generic additions (EX-3168) ────────────────────────────────────────
    /// Named type (struct, class, enum — language-agnostic abstraction)
    Type,
    /// Compile-time or declaration-time constant value
    Constant,
    /// Module import / use declaration
    Import,
    // ── Rust-specific (EX-3168, populated by EX-3120 tree-sitter parser) ──
    /// `fn foo()` — free function
    RustFn,
    /// `fn method(&self)` inside an impl block
    RustMethod,
    /// `impl Foo { ... }` or `impl Trait for Foo { ... }`
    RustImpl,
    /// `trait Foo { ... }`
    RustTrait,
    /// `struct Foo { ... }`
    RustStruct,
    /// `enum Foo { ... }`
    RustEnum,
    /// `mod foo { ... }` or `mod foo;`
    RustMod,
    /// `macro_rules! name { ... }`
    RustMacro,
    /// `use path::to::Item;`
    RustUse,
    /// `const NAME: Type = value;`
    RustConst,
    /// `static NAME: Type = value;`
    RustStatic,
    /// `type Foo = Bar;`
    RustTypeAlias,
    /// `#[derive(...)]` or other attributes (stored separately from node)
    RustAttribute,
    /// Lifetime parameter `'a` in fn / struct / impl
    RustLifetime,
    /// `#[test] fn test_foo() { ... }`
    RustTest,
    // ── Python-specific (EX-3172 / V12b-1, populated by PythonParser) ──────
    /// `def foo(...):` — top-level function
    PythonFunction,
    /// `def method(self, ...):` inside a class body
    PythonMethod,
    /// `class Foo:` — class definition
    PythonClass,
    /// `@dataclass`, `@property`, `@classmethod` etc. — decorator node
    PythonDecorator,
    /// `import x` or `from x import y` — import statement
    PythonImport,
    /// Top-level file module (file as a Python module)
    PythonModule,
    /// `lambda x: ...` — anonymous function expression
    PythonLambda,
    /// `async def foo():` — async function or method
    PythonAsync,
    /// Method decorated with `@property`
    PythonProperty,
}

impl CodeNodeKind {
    /// All 35 variants in stable order — used to build the Arrow Dictionary<Int8, Utf8>.
    /// Int8 supports up to 127 unique values; 35 variants is well within range.
    pub const ALL: [CodeNodeKind; 35] = [
        // Language-agnostic (original set)
        Self::File,
        Self::Module,
        Self::Class,
        Self::Function,
        Self::Method,
        Self::Parameter,
        Self::Variable,
        Self::Test,
        // Generic additions (EX-3168)
        Self::Type,
        Self::Constant,
        Self::Import,
        // Rust-specific (EX-3168)
        Self::RustFn,
        Self::RustMethod,
        Self::RustImpl,
        Self::RustTrait,
        Self::RustStruct,
        Self::RustEnum,
        Self::RustMod,
        Self::RustMacro,
        Self::RustUse,
        Self::RustConst,
        Self::RustStatic,
        Self::RustTypeAlias,
        Self::RustAttribute,
        Self::RustLifetime,
        Self::RustTest,
        // Python-specific (EX-3172)
        Self::PythonFunction,
        Self::PythonMethod,
        Self::PythonClass,
        Self::PythonDecorator,
        Self::PythonImport,
        Self::PythonModule,
        Self::PythonLambda,
        Self::PythonAsync,
        Self::PythonProperty,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Parameter => "parameter",
            Self::Variable => "variable",
            Self::Test => "test",
            Self::Type => "type",
            Self::Constant => "constant",
            Self::Import => "import",
            Self::RustFn => "rust_fn",
            Self::RustMethod => "rust_method",
            Self::RustImpl => "rust_impl",
            Self::RustTrait => "rust_trait",
            Self::RustStruct => "rust_struct",
            Self::RustEnum => "rust_enum",
            Self::RustMod => "rust_mod",
            Self::RustMacro => "rust_macro",
            Self::RustUse => "rust_use",
            Self::RustConst => "rust_const",
            Self::RustStatic => "rust_static",
            Self::RustTypeAlias => "rust_type_alias",
            Self::RustAttribute => "rust_attribute",
            Self::RustLifetime => "rust_lifetime",
            Self::RustTest => "rust_test",
            Self::PythonFunction => "python_function",
            Self::PythonMethod => "python_method",
            Self::PythonClass => "python_class",
            Self::PythonDecorator => "python_decorator",
            Self::PythonImport => "python_import",
            Self::PythonModule => "python_module",
            Self::PythonLambda => "python_lambda",
            Self::PythonAsync => "python_async",
            Self::PythonProperty => "python_property",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "class" => Some(Self::Class),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "parameter" => Some(Self::Parameter),
            "variable" => Some(Self::Variable),
            "test" => Some(Self::Test),
            "type" => Some(Self::Type),
            "constant" => Some(Self::Constant),
            "import" => Some(Self::Import),
            "rust_fn" => Some(Self::RustFn),
            "rust_method" => Some(Self::RustMethod),
            "rust_impl" => Some(Self::RustImpl),
            "rust_trait" => Some(Self::RustTrait),
            "rust_struct" => Some(Self::RustStruct),
            "rust_enum" => Some(Self::RustEnum),
            "rust_mod" => Some(Self::RustMod),
            "rust_macro" => Some(Self::RustMacro),
            "rust_use" => Some(Self::RustUse),
            "rust_const" => Some(Self::RustConst),
            "rust_static" => Some(Self::RustStatic),
            "rust_type_alias" => Some(Self::RustTypeAlias),
            "rust_attribute" => Some(Self::RustAttribute),
            "rust_lifetime" => Some(Self::RustLifetime),
            "rust_test" => Some(Self::RustTest),
            "python_function" => Some(Self::PythonFunction),
            "python_method" => Some(Self::PythonMethod),
            "python_class" => Some(Self::PythonClass),
            "python_decorator" => Some(Self::PythonDecorator),
            "python_import" => Some(Self::PythonImport),
            "python_module" => Some(Self::PythonModule),
            "python_lambda" => Some(Self::PythonLambda),
            "python_async" => Some(Self::PythonAsync),
            "python_property" => Some(Self::PythonProperty),
            _ => None,
        }
    }

    /// Whether this kind is Rust-specific (introduced by EX-3168 / V12a-1).
    pub fn is_rust_specific(self) -> bool {
        matches!(
            self,
            Self::RustFn
                | Self::RustMethod
                | Self::RustImpl
                | Self::RustTrait
                | Self::RustStruct
                | Self::RustEnum
                | Self::RustMod
                | Self::RustMacro
                | Self::RustUse
                | Self::RustConst
                | Self::RustStatic
                | Self::RustTypeAlias
                | Self::RustAttribute
                | Self::RustLifetime
                | Self::RustTest
        )
    }

    /// Whether this kind is Python-specific (introduced by EX-3172 / V12b-1).
    pub fn is_python_specific(self) -> bool {
        matches!(
            self,
            Self::PythonFunction
                | Self::PythonMethod
                | Self::PythonClass
                | Self::PythonDecorator
                | Self::PythonImport
                | Self::PythonModule
                | Self::PythonLambda
                | Self::PythonAsync
                | Self::PythonProperty
        )
    }
}

impl std::fmt::Display for CodeNodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kind of relationship between code objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeEdgePredicate {
    Calls,
    InheritsFrom,
    Imports,
    Defines,
    Uses,
    Tests,
    Contains,
    References,
    /// impl Trait for Struct → Trait node
    ImplementsTrait,
    /// pub use re-exports an item from another module
    ReExports,
    /// Macro invocation → macro definition
    MacroExpands,
    /// #[test] fn → function it tests (naming convention)
    TestTargets,
}

impl CodeEdgePredicate {
    pub const ALL: [CodeEdgePredicate; 12] = [
        Self::Calls,
        Self::InheritsFrom,
        Self::Imports,
        Self::Defines,
        Self::Uses,
        Self::Tests,
        Self::Contains,
        Self::References,
        Self::ImplementsTrait,
        Self::ReExports,
        Self::MacroExpands,
        Self::TestTargets,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::InheritsFrom => "inherits_from",
            Self::Imports => "imports",
            Self::Defines => "defines",
            Self::Uses => "uses",
            Self::Tests => "tests",
            Self::Contains => "contains",
            Self::References => "references",
            Self::ImplementsTrait => "implements_trait",
            Self::ReExports => "re_exports",
            Self::MacroExpands => "macro_expands",
            Self::TestTargets => "test_targets",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "calls" => Some(Self::Calls),
            "inherits_from" => Some(Self::InheritsFrom),
            "imports" => Some(Self::Imports),
            "defines" => Some(Self::Defines),
            "uses" => Some(Self::Uses),
            "tests" => Some(Self::Tests),
            "contains" => Some(Self::Contains),
            "references" => Some(Self::References),
            "implements_trait" => Some(Self::ImplementsTrait),
            "re_exports" => Some(Self::ReExports),
            "macro_expands" => Some(Self::MacroExpands),
            "test_targets" => Some(Self::TestTargets),
            _ => None,
        }
    }
}

impl std::fmt::Display for CodeEdgePredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Utilities ──────────────────────────────────────────────────────────────

/// Extract file path from a CodeNode ID.
///
/// IDs follow the pattern `prefix:path/to/file.py::Name`.
/// This extracts `path/to/file.py`.
///
/// Examples:
/// - `"func:brain/signal.py::fuse"` → `Some("brain/signal.py")`
/// - `"mod:brain/signal.py"` → `Some("brain/signal.py")`
/// - `"method:brain/store.py::Dual::get"` → `Some("brain/store.py")`
pub fn extract_file_path(node_id: &str) -> Option<String> {
    let rest = node_id.split_once(':')?.1;
    let file_part = rest.split("::").next()?;
    if file_part.is_empty() {
        None
    } else {
        Some(file_part.to_string())
    }
}

// ─── Schemas ────────────────────────────────────────────────────────────────

/// Schema for the CodeNodes table.
///
/// Columns 0–12 (original):
/// - `id`: fully-qualified identifier (e.g. `func:brain/perception/signal_fusion.py::fuse`)
/// - `kind`: object type (dictionary-encoded string, `Dictionary<Int8, Utf8>`)
/// - `parent_id`: containment pointer (function→class→module→file)
/// - `name`: object name
/// - `signature`: function/method signature
/// - `docstring`: documentation text
/// - `body_hash`: SHA-256 of the object body for equality checks
/// - `body`: full source text of the node
/// - `embedding`: semantic vector (`FixedSizeList<f32, 768>`)
/// - `loc`: lines of code
/// - `cyclomatic_complexity`: complexity metric
/// - `coverage_pct`: test coverage percentage
/// - `last_modified`: timestamp from git history
///
/// Columns 13–18 (EX-3168 / V12a-1 — position metadata, all nullable):
/// - `start_line`: 1-indexed source line where the node begins
/// - `end_line`: 1-indexed source line where the node ends
/// - `start_col`: 0-indexed column within `start_line`
/// - `end_col`: 0-indexed column within `end_line`
/// - `file_path`: relative path from crate root (e.g. `"src/lib.rs"`)
/// - `byte_offset`: byte position from file start (for fast editor seek)
///
/// Position columns are nullable for backward compatibility with existing Parquet files.
/// The tree-sitter Rust parser (EX-3120) populates them for new nodes.
pub fn code_nodes_schema() -> Schema {
    Schema::new(vec![
        // ── Core identity ──────────────────────────────────────────────────
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "kind",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("parent_id", DataType::Utf8, true),
        Field::new("name", DataType::Utf8, false),
        Field::new("signature", DataType::Utf8, true),
        Field::new("docstring", DataType::Utf8, true),
        Field::new("body_hash", DataType::Utf8, true),
        Field::new("body", DataType::LargeUtf8, true),
        // ── Embeddings ─────────────────────────────────────────────────────
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, false)),
                CODE_EMBEDDING_DIM,
            ),
            true,
        ),
        // ── Metrics ────────────────────────────────────────────────────────
        Field::new("loc", DataType::Int32, true),
        Field::new("cyclomatic_complexity", DataType::Int32, true),
        Field::new("coverage_pct", DataType::Float64, true),
        Field::new(
            "last_modified",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        // ── Position metadata (EX-3168 / V12a-1) — nullable ───────────────
        Field::new("start_line", DataType::UInt32, true),
        Field::new("end_line", DataType::UInt32, true),
        Field::new("start_col", DataType::UInt32, true),
        Field::new("end_col", DataType::UInt32, true),
        Field::new("file_path", DataType::Utf8, true),
        Field::new("byte_offset", DataType::UInt64, true),
    ])
}

/// Schema for the CodeEdges table.
///
/// Columns:
/// - `source_id`: source CodeNode ID
/// - `target_id`: target CodeNode ID
/// - `predicate`: relationship type (dictionary-encoded string)
/// - `weight`: optional edge weight (call frequency, inheritance depth, etc.)
/// - `commit_id`: which commit introduced this edge
pub fn code_edges_schema() -> Schema {
    Schema::new(vec![
        Field::new("source_id", DataType::Utf8, false),
        Field::new("target_id", DataType::Utf8, false),
        Field::new(
            "predicate",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("weight", DataType::Float32, true),
        Field::new("commit_id", DataType::Utf8, true),
    ])
}

// ─── RecordBatch builders ───────────────────────────────────────────────────

use arrow::array::{
    Float32Array, Float64Array, Int8Array, Int32Array, RecordBatch, StringArray, UInt32Array,
    UInt64Array,
};

/// A code node record for batch building.
///
/// Position fields (`start_line` through `byte_offset`) were added in EX-3168 / V12a-1.
/// Existing call sites that don't set them should use `..Default::default()` to get `None`
/// for all position fields. The `Default` impl uses `CodeNodeKind::File` as the kind
/// placeholder — callers must always set `id`, `kind`, and `name` explicitly.
#[derive(Debug, Clone)]
pub struct CodeNode {
    pub id: String,
    pub kind: CodeNodeKind,
    pub parent_id: Option<String>,
    pub name: String,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub body_hash: Option<String>,
    pub body: Option<String>,
    pub loc: Option<i32>,
    pub cyclomatic_complexity: Option<i32>,
    pub coverage_pct: Option<f64>,
    pub last_modified: Option<i64>,
    // Position metadata (EX-3168 / V12a-1) — None for nodes from old data or Python parser
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub start_col: Option<u32>,
    pub end_col: Option<u32>,
    pub file_path: Option<String>,
    pub byte_offset: Option<u64>,
}

impl Default for CodeNode {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: CodeNodeKind::File,
            parent_id: None,
            name: String::new(),
            signature: None,
            docstring: None,
            body_hash: None,
            body: None,
            loc: None,
            cyclomatic_complexity: None,
            coverage_pct: None,
            last_modified: None,
            start_line: None,
            end_line: None,
            start_col: None,
            end_col: None,
            file_path: None,
            byte_offset: None,
        }
    }
}

/// A code edge record for batch building.
#[derive(Debug, Clone)]
pub struct CodeEdge {
    pub source_id: String,
    pub target_id: String,
    pub predicate: CodeEdgePredicate,
    pub weight: Option<f32>,
    pub commit_id: Option<String>,
}

/// Build a RecordBatch of CodeNodes (without embeddings — those are added later).
pub fn build_code_nodes_batch(nodes: &[CodeNode]) -> Result<RecordBatch, arrow::error::ArrowError> {
    use arrow::array::{Int8DictionaryArray, PrimitiveBuilder};

    let schema = Arc::new(code_nodes_schema());
    let n = nodes.len();

    if n == 0 {
        return Ok(RecordBatch::new_empty(schema));
    }

    let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    let parent_ids: Vec<Option<&str>> = nodes.iter().map(|n| n.parent_id.as_deref()).collect();
    let signatures: Vec<Option<&str>> = nodes.iter().map(|n| n.signature.as_deref()).collect();
    let docstrings: Vec<Option<&str>> = nodes.iter().map(|n| n.docstring.as_deref()).collect();
    let body_hashes: Vec<Option<&str>> = nodes.iter().map(|n| n.body_hash.as_deref()).collect();
    let bodies: Vec<Option<&str>> = nodes.iter().map(|n| n.body.as_deref()).collect();

    // Build kind as dictionary array
    let kind_keys = Int8Array::from(
        nodes
            .iter()
            .map(|n| {
                CodeNodeKind::ALL
                    .iter()
                    .position(|k| *k == n.kind)
                    .expect("valid kind") as i8
            })
            .collect::<Vec<i8>>(),
    );
    let kind_values = StringArray::from(
        CodeNodeKind::ALL
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>(),
    );
    let kind_dict = Int8DictionaryArray::try_new(kind_keys, Arc::new(kind_values))?;

    // Embedding: null for all (populated later by embedding pipeline)
    let embedding_field = Arc::new(Field::new("item", DataType::Float32, false));
    let null_embedding = arrow::array::FixedSizeListArray::try_new(
        embedding_field,
        CODE_EMBEDDING_DIM,
        Arc::new(Float32Array::from(vec![
            0.0f32;
            n * CODE_EMBEDDING_DIM as usize
        ])),
        Some(arrow::buffer::NullBuffer::new(
            arrow::buffer::BooleanBuffer::from(vec![false; n]),
        )),
    )?;

    // Metrics
    let locs: Vec<Option<i32>> = nodes.iter().map(|n| n.loc).collect();
    let complexities: Vec<Option<i32>> = nodes.iter().map(|n| n.cyclomatic_complexity).collect();
    let coverages: Vec<Option<f64>> = nodes.iter().map(|n| n.coverage_pct).collect();

    let mut last_mod_builder =
        PrimitiveBuilder::<arrow::datatypes::TimestampMillisecondType>::new().with_timezone("UTC");
    for node in nodes {
        match node.last_modified {
            Some(ts) => last_mod_builder.append_value(ts),
            None => last_mod_builder.append_null(),
        }
    }

    // Position metadata (EX-3168 / V12a-1) — nullable; None for nodes without position info
    let start_lines: Vec<Option<u32>> = nodes.iter().map(|n| n.start_line).collect();
    let end_lines: Vec<Option<u32>> = nodes.iter().map(|n| n.end_line).collect();
    let start_cols: Vec<Option<u32>> = nodes.iter().map(|n| n.start_col).collect();
    let end_cols: Vec<Option<u32>> = nodes.iter().map(|n| n.end_col).collect();
    let file_paths: Vec<Option<&str>> = nodes.iter().map(|n| n.file_path.as_deref()).collect();
    let byte_offsets: Vec<Option<u64>> = nodes.iter().map(|n| n.byte_offset).collect();

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(kind_dict),
            Arc::new(StringArray::from(parent_ids)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(signatures)),
            Arc::new(StringArray::from(docstrings)),
            Arc::new(StringArray::from(body_hashes)),
            Arc::new(arrow::array::LargeStringArray::from(bodies)),
            Arc::new(null_embedding),
            Arc::new(Int32Array::from(locs)),
            Arc::new(Int32Array::from(complexities)),
            Arc::new(Float64Array::from(coverages)),
            Arc::new(last_mod_builder.finish()),
            Arc::new(UInt32Array::from(start_lines)),
            Arc::new(UInt32Array::from(end_lines)),
            Arc::new(UInt32Array::from(start_cols)),
            Arc::new(UInt32Array::from(end_cols)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(UInt64Array::from(byte_offsets)),
        ],
    )
}

/// Build a RecordBatch of CodeEdges.
pub fn build_code_edges_batch(edges: &[CodeEdge]) -> Result<RecordBatch, arrow::error::ArrowError> {
    use arrow::array::Int8DictionaryArray;

    let schema = Arc::new(code_edges_schema());
    let n = edges.len();

    if n == 0 {
        return Ok(RecordBatch::new_empty(schema));
    }

    let source_ids: Vec<&str> = edges.iter().map(|e| e.source_id.as_str()).collect();
    let target_ids: Vec<&str> = edges.iter().map(|e| e.target_id.as_str()).collect();
    let weights: Vec<Option<f32>> = edges.iter().map(|e| e.weight).collect();
    let commit_ids: Vec<Option<&str>> = edges.iter().map(|e| e.commit_id.as_deref()).collect();

    // Build predicate as dictionary array
    let pred_keys = Int8Array::from(
        edges
            .iter()
            .map(|e| {
                CodeEdgePredicate::ALL
                    .iter()
                    .position(|p| *p == e.predicate)
                    .expect("valid predicate") as i8
            })
            .collect::<Vec<i8>>(),
    );
    let pred_values = StringArray::from(
        CodeEdgePredicate::ALL
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
    );
    let pred_dict = Int8DictionaryArray::try_new(pred_keys, Arc::new(pred_values))?;

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(source_ids)),
            Arc::new(StringArray::from(target_ids)),
            Arc::new(pred_dict),
            Arc::new(Float32Array::from(weights)),
            Arc::new(StringArray::from(commit_ids)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;

    #[test]
    fn test_code_nodes_schema_field_count() {
        let schema = code_nodes_schema();
        assert_eq!(schema.fields().len(), 19);
    }

    #[test]
    fn test_code_edges_schema_field_count() {
        let schema = code_edges_schema();
        assert_eq!(schema.fields().len(), 5);
    }

    #[test]
    fn test_code_node_kind_roundtrip() {
        for kind in CodeNodeKind::ALL {
            let s = kind.as_str();
            let parsed = CodeNodeKind::parse(s).expect("should parse");
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_code_edge_predicate_roundtrip() {
        for pred in CodeEdgePredicate::ALL {
            let s = pred.as_str();
            let parsed = CodeEdgePredicate::parse(s).expect("should parse");
            assert_eq!(parsed, pred);
        }
    }

    #[test]
    fn test_build_code_nodes_batch() {
        let nodes = vec![
            CodeNode {
                id: "func:brain/main.py::main".to_string(),
                kind: CodeNodeKind::Function,
                parent_id: Some("mod:brain/main.py".to_string()),
                name: "main".to_string(),
                signature: Some("def main() -> None".to_string()),
                docstring: Some("Entry point.".to_string()),
                body_hash: Some("abc123".to_string()),
                body: Some("def main():\n    pass".to_string()),
                loc: Some(42),
                cyclomatic_complexity: Some(5),
                coverage_pct: Some(0.85),
                last_modified: None,
                ..Default::default()
            },
            CodeNode {
                id: "class:brain/store.py::Store".to_string(),
                kind: CodeNodeKind::Class,
                parent_id: Some("mod:brain/store.py".to_string()),
                name: "Store".to_string(),
                signature: None,
                docstring: Some("Main store class.".to_string()),
                body_hash: Some("def456".to_string()),
                body: Some("class Store:\n    pass".to_string()),
                loc: Some(120),
                cyclomatic_complexity: Some(12),
                coverage_pct: None,
                last_modified: Some(chrono::Utc::now().timestamp_millis()),
                ..Default::default()
            },
        ];

        let batch = build_code_nodes_batch(&nodes).expect("should build batch");
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 19);

        // Verify id column
        let ids = batch
            .column(node_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column");
        assert_eq!(ids.value(0), "func:brain/main.py::main");
        assert_eq!(ids.value(1), "class:brain/store.py::Store");

        // Verify kind is dictionary-encoded
        assert!(matches!(
            batch.column(node_col::KIND).data_type(),
            DataType::Dictionary(_, _)
        ));

        // Verify embedding column is all nulls (not yet populated)
        let emb = batch.column(node_col::EMBEDDING);
        assert!(emb.is_null(0));
        assert!(emb.is_null(1));

        // Verify metrics
        let locs = batch
            .column(node_col::LOC)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("loc column");
        assert_eq!(locs.value(0), 42);
        assert_eq!(locs.value(1), 120);
    }

    #[test]
    fn test_build_code_edges_batch() {
        let edges = vec![
            CodeEdge {
                source_id: "func:a.py::foo".to_string(),
                target_id: "func:b.py::bar".to_string(),
                predicate: CodeEdgePredicate::Calls,
                weight: Some(1.0),
                commit_id: Some("abc123".to_string()),
            },
            CodeEdge {
                source_id: "class:c.py::Child".to_string(),
                target_id: "class:c.py::Parent".to_string(),
                predicate: CodeEdgePredicate::InheritsFrom,
                weight: None,
                commit_id: None,
            },
            CodeEdge {
                source_id: "mod:a.py".to_string(),
                target_id: "mod:b.py".to_string(),
                predicate: CodeEdgePredicate::Imports,
                weight: None,
                commit_id: None,
            },
        ];

        let batch = build_code_edges_batch(&edges).expect("should build batch");
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.num_columns(), 5);

        // Verify predicate is dictionary-encoded
        assert!(matches!(
            batch.column(edge_col::PREDICATE).data_type(),
            DataType::Dictionary(_, _)
        ));

        // Verify source/target
        let sources = batch
            .column(edge_col::SOURCE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("source_id column");
        assert_eq!(sources.value(0), "func:a.py::foo");
        assert_eq!(sources.value(2), "mod:a.py");
    }

    #[test]
    fn test_empty_batches() {
        let nodes_batch = build_code_nodes_batch(&[]).expect("empty nodes batch");
        assert_eq!(nodes_batch.num_rows(), 0);
        assert_eq!(nodes_batch.num_columns(), 19);

        let edges_batch = build_code_edges_batch(&[]).expect("empty edges batch");
        assert_eq!(edges_batch.num_rows(), 0);
        assert_eq!(edges_batch.num_columns(), 5);
    }

    // ── EX-3168 / V12a-1 tests ───────────────────────────────────────────────

    #[test]
    fn test_all_35_code_node_kind_variants_roundtrip() {
        // Every variant in ALL must survive as_str() → parse() roundtrip
        assert_eq!(CodeNodeKind::ALL.len(), 35, "expected 35 variants in ALL");
        for kind in CodeNodeKind::ALL {
            let s = kind.as_str();
            let parsed = CodeNodeKind::parse(s)
                .unwrap_or_else(|| panic!("parse({s:?}) returned None for {kind:?}"));
            assert_eq!(parsed, kind, "roundtrip failed for {kind:?}");
        }
    }

    #[test]
    fn test_rust_specific_variants_are_flagged() {
        let rust_kinds = [
            CodeNodeKind::RustFn,
            CodeNodeKind::RustMethod,
            CodeNodeKind::RustImpl,
            CodeNodeKind::RustTrait,
            CodeNodeKind::RustStruct,
            CodeNodeKind::RustEnum,
            CodeNodeKind::RustMod,
            CodeNodeKind::RustMacro,
            CodeNodeKind::RustUse,
            CodeNodeKind::RustConst,
            CodeNodeKind::RustStatic,
            CodeNodeKind::RustTypeAlias,
            CodeNodeKind::RustAttribute,
            CodeNodeKind::RustLifetime,
            CodeNodeKind::RustTest,
        ];
        assert_eq!(rust_kinds.len(), 15, "expected 15 Rust-specific variants");
        for k in rust_kinds {
            assert!(k.is_rust_specific(), "{k:?} should be rust-specific");
        }

        // Generic and language-agnostic variants must NOT be flagged
        let generic_kinds = [
            CodeNodeKind::File,
            CodeNodeKind::Module,
            CodeNodeKind::Class,
            CodeNodeKind::Function,
            CodeNodeKind::Method,
            CodeNodeKind::Parameter,
            CodeNodeKind::Variable,
            CodeNodeKind::Test,
            CodeNodeKind::Type,
            CodeNodeKind::Constant,
            CodeNodeKind::Import,
        ];
        for k in generic_kinds {
            assert!(!k.is_rust_specific(), "{k:?} should NOT be rust-specific");
        }
    }

    #[test]
    fn test_rust_kind_strings_have_rust_prefix() {
        let rust_kinds = CodeNodeKind::ALL
            .iter()
            .filter(|k| k.is_rust_specific())
            .collect::<Vec<_>>();
        assert!(!rust_kinds.is_empty());
        for k in rust_kinds {
            assert!(
                k.as_str().starts_with("rust_"),
                "Rust-specific kind {k:?} string {:?} must start with 'rust_'",
                k.as_str()
            );
        }
    }

    #[test]
    fn test_position_columns_are_in_schema() {
        let schema = code_nodes_schema();
        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(field_names.contains(&"start_line"), "missing start_line");
        assert!(field_names.contains(&"end_line"), "missing end_line");
        assert!(field_names.contains(&"start_col"), "missing start_col");
        assert!(field_names.contains(&"end_col"), "missing end_col");
        assert!(field_names.contains(&"file_path"), "missing file_path");
        assert!(field_names.contains(&"byte_offset"), "missing byte_offset");
    }

    #[test]
    fn test_position_columns_are_nullable() {
        let schema = code_nodes_schema();
        for name in &[
            "start_line",
            "end_line",
            "start_col",
            "end_col",
            "file_path",
            "byte_offset",
        ] {
            let field = schema
                .field_with_name(name)
                .unwrap_or_else(|_| panic!("{name} not in schema"));
            assert!(
                field.is_nullable(),
                "{name} must be nullable for backward compatibility"
            );
        }
    }

    #[test]
    fn test_position_columns_correct_types() {
        use arrow::datatypes::DataType;
        let schema = code_nodes_schema();
        let check = |name: &str, expected: DataType| {
            let field = schema.field_with_name(name).expect(name);
            assert_eq!(
                *field.data_type(),
                expected,
                "{name} has wrong type: expected {expected:?}"
            );
        };
        check("start_line", DataType::UInt32);
        check("end_line", DataType::UInt32);
        check("start_col", DataType::UInt32);
        check("end_col", DataType::UInt32);
        check("file_path", DataType::Utf8);
        check("byte_offset", DataType::UInt64);
    }

    #[test]
    fn test_position_column_indices_match_schema() {
        // Named constants must match actual column positions in the schema
        let schema = code_nodes_schema();
        let check = |idx: usize, expected_name: &str| {
            let actual = schema.field(idx).name();
            assert_eq!(
                actual, expected_name,
                "node_col constant {idx} expected {expected_name}, got {actual}"
            );
        };
        check(node_col::START_LINE, "start_line");
        check(node_col::END_LINE, "end_line");
        check(node_col::START_COL, "start_col");
        check(node_col::END_COL, "end_col");
        check(node_col::FILE_PATH, "file_path");
        check(node_col::BYTE_OFFSET, "byte_offset");
    }

    #[test]
    fn test_build_node_with_position_metadata() {
        let nodes = vec![
            CodeNode {
                id: "rust_fn:src/lib.rs::process".to_string(),
                kind: CodeNodeKind::RustFn,
                parent_id: Some("rust_mod:src/lib.rs".to_string()),
                name: "process".to_string(),
                signature: Some("fn process(input: &str) -> Result<()>".to_string()),
                start_line: Some(42),
                end_line: Some(58),
                start_col: Some(0),
                end_col: Some(1),
                file_path: Some("src/lib.rs".to_string()),
                byte_offset: Some(1024),
                loc: Some(17),
                ..Default::default()
            },
            // Node without position (backward compat — all position fields None)
            CodeNode {
                id: "func:brain/old.py::legacy".to_string(),
                kind: CodeNodeKind::Function,
                name: "legacy".to_string(),
                ..Default::default()
            },
        ];

        let batch = build_code_nodes_batch(&nodes).expect("should build batch");
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 19);

        // Row 0: position is present
        let start_lines = batch
            .column(node_col::START_LINE)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .expect("start_line column");
        assert!(!start_lines.is_null(0), "row 0 start_line should be set");
        assert_eq!(start_lines.value(0), 42);

        let file_paths = batch
            .column(node_col::FILE_PATH)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("file_path column");
        assert_eq!(file_paths.value(0), "src/lib.rs");

        // Row 1: position is null (backward compat)
        assert!(start_lines.is_null(1), "row 1 start_line should be null");
        assert!(file_paths.is_null(1), "row 1 file_path should be null");

        // Rust kind is dictionary-encoded
        assert!(matches!(
            batch.column(node_col::KIND).data_type(),
            DataType::Dictionary(_, _)
        ));
    }

    #[test]
    fn test_dictionary_encoding_roundtrip_all_kinds() {
        use arrow::array::Int8DictionaryArray;

        // Build a batch with one node per kind in ALL
        let nodes: Vec<CodeNode> = CodeNodeKind::ALL
            .iter()
            .enumerate()
            .map(|(i, &kind)| CodeNode {
                id: format!("node_{i}"),
                kind,
                name: format!("item_{i}"),
                ..Default::default()
            })
            .collect();

        let batch = build_code_nodes_batch(&nodes).expect("batch with all 35 kinds");
        assert_eq!(batch.num_rows(), 35);

        // Extract kind column and verify each value decodes correctly
        let kind_col = batch
            .column(node_col::KIND)
            .as_any()
            .downcast_ref::<Int8DictionaryArray>()
            .expect("kind is Int8DictionaryArray");
        let kind_values = kind_col
            .values()
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("kind values are strings");

        for (i, &expected_kind) in CodeNodeKind::ALL.iter().enumerate() {
            let key = kind_col.keys().value(i) as usize;
            let decoded_str = kind_values.value(key);
            assert_eq!(
                decoded_str,
                expected_kind.as_str(),
                "row {i}: expected {:?}, got {decoded_str:?}",
                expected_kind.as_str()
            );
        }
    }

    // ── EX-3172 / V12b-1 tests ───────────────────────────────────────────────

    #[test]
    fn test_python_specific_variants_are_flagged() {
        let python_kinds = [
            CodeNodeKind::PythonFunction,
            CodeNodeKind::PythonMethod,
            CodeNodeKind::PythonClass,
            CodeNodeKind::PythonDecorator,
            CodeNodeKind::PythonImport,
            CodeNodeKind::PythonModule,
            CodeNodeKind::PythonLambda,
            CodeNodeKind::PythonAsync,
            CodeNodeKind::PythonProperty,
        ];
        assert_eq!(python_kinds.len(), 9, "expected 9 Python-specific variants");
        for k in python_kinds {
            assert!(k.is_python_specific(), "{k:?} should be python-specific");
        }

        // Generic, Rust, and language-agnostic variants must NOT be flagged
        let non_python = [
            CodeNodeKind::File,
            CodeNodeKind::Module,
            CodeNodeKind::Class,
            CodeNodeKind::Function,
            CodeNodeKind::Method,
            CodeNodeKind::RustFn,
            CodeNodeKind::RustStruct,
            CodeNodeKind::Import,
        ];
        for k in non_python {
            assert!(
                !k.is_python_specific(),
                "{k:?} should NOT be python-specific"
            );
        }
    }

    #[test]
    fn test_python_kind_strings_have_python_prefix() {
        let python_kinds = CodeNodeKind::ALL
            .iter()
            .filter(|k| k.is_python_specific())
            .collect::<Vec<_>>();
        assert_eq!(python_kinds.len(), 9);
        for k in python_kinds {
            assert!(
                k.as_str().starts_with("python_"),
                "Python-specific kind {k:?} string {:?} must start with 'python_'",
                k.as_str()
            );
        }
    }

    #[test]
    fn test_python_variants_roundtrip() {
        let python_kinds = [
            CodeNodeKind::PythonFunction,
            CodeNodeKind::PythonMethod,
            CodeNodeKind::PythonClass,
            CodeNodeKind::PythonDecorator,
            CodeNodeKind::PythonImport,
            CodeNodeKind::PythonModule,
            CodeNodeKind::PythonLambda,
            CodeNodeKind::PythonAsync,
            CodeNodeKind::PythonProperty,
        ];
        for k in python_kinds {
            let s = k.as_str();
            let parsed = CodeNodeKind::parse(s)
                .unwrap_or_else(|| panic!("parse({s:?}) returned None for {k:?}"));
            assert_eq!(parsed, k, "roundtrip failed for {k:?}");
        }
    }
}
