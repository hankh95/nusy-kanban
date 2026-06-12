//! Arrow schemas for the crate-level dependency graph.
//!
//! Two tables:
//! - **CrateNodes**: one row per crate (workspace members + notable external crates)
//! - **CrateEdges**: one row per dependency relationship (source depends on target)
//!
//! These tables are produced by [`crate::crate_graph::build_crate_graph`] and can be
//! stored as Parquet snapshots via the Arrow persistence layer.
//!
//! # Schema rationale
//!
//! CrateNode `id` is the crate name (e.g. `"nusy-arrow-core"`), matching the `name`
//! field in `[package]`.  CrateEdge `source` → `target` represents "source depends on
//! target".  `topo_sort_crates` returns targets before sources (dependency-first order).

use arrow::datatypes::{DataType, Field, Schema};
use std::sync::Arc;

// ─── CrateNode column indices ────────────────────────────────────────────────

/// Named column indices for the CrateNodes schema.
pub mod crate_node_col {
    pub const ID: usize = 0;
    pub const VERSION: usize = 1;
    pub const WORKSPACE_MEMBER: usize = 2;
    pub const DESCRIPTION: usize = 3;
    pub const EDITION: usize = 4;
}

// ─── CrateEdge column indices ────────────────────────────────────────────────

/// Named column indices for the CrateEdges schema.
pub mod crate_edge_col {
    pub const SOURCE: usize = 0;
    pub const TARGET: usize = 1;
    pub const VERSION_REQ: usize = 2;
    pub const OPTIONAL: usize = 3;
    pub const DEV_DEP: usize = 4;
    pub const BUILD_DEP: usize = 5;
    pub const SOURCE_KIND: usize = 6;
}

// ─── Schemas ─────────────────────────────────────────────────────────────────

/// Arrow schema for crate nodes.
///
/// Columns:
/// - `id`: crate name, e.g. `"nusy-arrow-core"` — primary key
/// - `version`: resolved semver string
/// - `workspace_member`: true when listed under `[workspace].members`
/// - `description`: optional description from `[package].description`
/// - `edition`: Rust edition, e.g. `"2024"`
pub fn crate_node_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("version", DataType::Utf8, false),
        Field::new("workspace_member", DataType::Boolean, false),
        Field::new("description", DataType::Utf8, true),
        Field::new("edition", DataType::Utf8, false),
    ]))
}

/// Arrow schema for crate dependency edges.
///
/// Columns:
/// - `source`: the crate that declares the dependency
/// - `target`: the crate being depended upon
/// - `version_req`: semver requirement, e.g. `"55"`, `">=1, <2"`, `"*"` for path deps
/// - `optional`: `true` if the dep is `optional = true`
/// - `dev_dep`: `true` if the dep came from `[dev-dependencies]`
/// - `build_dep`: `true` if the dep came from `[build-dependencies]`
/// - `source_kind`: provenance string — one of `"workspace"`, `"crates_io"`, `"git"`, `"path"`
pub fn crate_edge_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("target", DataType::Utf8, false),
        Field::new("version_req", DataType::Utf8, false),
        Field::new("optional", DataType::Boolean, false),
        Field::new("dev_dep", DataType::Boolean, false),
        Field::new("build_dep", DataType::Boolean, false),
        Field::new("source_kind", DataType::Utf8, false),
    ]))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crate_node_schema_field_count() {
        let schema = crate_node_schema();
        assert_eq!(
            schema.fields().len(),
            5,
            "CrateNode schema should have 5 fields"
        );
    }

    #[test]
    fn test_crate_edge_schema_field_count() {
        let schema = crate_edge_schema();
        assert_eq!(
            schema.fields().len(),
            7,
            "CrateEdge schema should have 7 fields"
        );
    }

    #[test]
    fn test_crate_node_schema_field_names() {
        let schema = crate_node_schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec![
                "id",
                "version",
                "workspace_member",
                "description",
                "edition"
            ]
        );
    }

    #[test]
    fn test_crate_edge_schema_field_names() {
        let schema = crate_edge_schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec![
                "source",
                "target",
                "version_req",
                "optional",
                "dev_dep",
                "build_dep",
                "source_kind"
            ]
        );
    }

    #[test]
    fn test_crate_node_schema_nullability() {
        let schema = crate_node_schema();
        // id, version, workspace_member, edition must be non-null
        for name in &["id", "version", "edition"] {
            let field = schema.field_with_name(name).expect(name);
            assert!(!field.is_nullable(), "{name} should be non-nullable");
        }
        // workspace_member
        let wm = schema.field_with_name("workspace_member").unwrap();
        assert!(!wm.is_nullable(), "workspace_member should be non-nullable");
        // description is nullable
        let desc = schema.field_with_name("description").unwrap();
        assert!(desc.is_nullable(), "description should be nullable");
    }

    #[test]
    fn test_crate_edge_schema_nullability() {
        let schema = crate_edge_schema();
        let non_null = ["source", "target", "version_req", "source_kind"];
        for name in &non_null {
            let field = schema.field_with_name(name).expect(name);
            assert!(!field.is_nullable(), "{name} should be non-nullable");
        }
        let bool_cols = ["optional", "dev_dep", "build_dep"];
        for name in &bool_cols {
            let field = schema.field_with_name(name).expect(name);
            assert!(
                !field.is_nullable(),
                "{name} bool col should be non-nullable"
            );
        }
    }

    #[test]
    fn test_crate_node_col_constants_match_schema() {
        let schema = crate_node_schema();
        let check = |idx: usize, expected: &str| {
            let actual = schema.field(idx).name();
            assert_eq!(
                actual, expected,
                "crate_node_col[{idx}] expected {expected}, got {actual}"
            );
        };
        check(crate_node_col::ID, "id");
        check(crate_node_col::VERSION, "version");
        check(crate_node_col::WORKSPACE_MEMBER, "workspace_member");
        check(crate_node_col::DESCRIPTION, "description");
        check(crate_node_col::EDITION, "edition");
    }

    #[test]
    fn test_crate_edge_col_constants_match_schema() {
        let schema = crate_edge_schema();
        let check = |idx: usize, expected: &str| {
            let actual = schema.field(idx).name();
            assert_eq!(
                actual, expected,
                "crate_edge_col[{idx}] expected {expected}, got {actual}"
            );
        };
        check(crate_edge_col::SOURCE, "source");
        check(crate_edge_col::TARGET, "target");
        check(crate_edge_col::VERSION_REQ, "version_req");
        check(crate_edge_col::OPTIONAL, "optional");
        check(crate_edge_col::DEV_DEP, "dev_dep");
        check(crate_edge_col::BUILD_DEP, "build_dep");
        check(crate_edge_col::SOURCE_KIND, "source_kind");
    }

    #[test]
    fn test_crate_node_schema_types() {
        let schema = crate_node_schema();
        assert_eq!(
            *schema.field(crate_node_col::ID).data_type(),
            DataType::Utf8
        );
        assert_eq!(
            *schema.field(crate_node_col::VERSION).data_type(),
            DataType::Utf8
        );
        assert_eq!(
            *schema.field(crate_node_col::WORKSPACE_MEMBER).data_type(),
            DataType::Boolean
        );
        assert_eq!(
            *schema.field(crate_node_col::DESCRIPTION).data_type(),
            DataType::Utf8
        );
        assert_eq!(
            *schema.field(crate_node_col::EDITION).data_type(),
            DataType::Utf8
        );
    }

    #[test]
    fn test_crate_edge_schema_types() {
        let schema = crate_edge_schema();
        for col in &[
            crate_edge_col::SOURCE,
            crate_edge_col::TARGET,
            crate_edge_col::VERSION_REQ,
            crate_edge_col::SOURCE_KIND,
        ] {
            assert_eq!(*schema.field(*col).data_type(), DataType::Utf8);
        }
        for col in &[
            crate_edge_col::OPTIONAL,
            crate_edge_col::DEV_DEP,
            crate_edge_col::BUILD_DEP,
        ] {
            assert_eq!(*schema.field(*col).data_type(), DataType::Boolean);
        }
    }
}
