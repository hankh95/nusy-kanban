//! Export — render Arrow items to various formats.
//!
//! Supports:
//! - `item_to_markdown` — single item with YAML frontmatter
//! - `export_board_index` — full board index (KANBAN-BOARD.md / RESEARCH-BOARD.md)
//! - `export_json` — JSON array of items
//! - `export_markdown_table` — simple markdown table
//! - `export_board_html` — standalone HTML board with inline CSS
//! - `export_research_index_html` — paper→hypothesis→experiment→measure tree
//! - `burndown_svg` — inline SVG burndown chart

use crate::hdd::RegistryChain;
use crate::schema::items_col;
use crate::stats::BurndownPoint;
use arrow::array::{Array, ListArray, RecordBatch, StringArray};

/// Status emoji mapping for board index.
fn status_icon(status: &str) -> &'static str {
    match status {
        "done" | "complete" | "arrived" => "\u{2705}", // ✅
        "in_progress" | "underway" => "\u{1f504}",     // 🔄
        "review" | "approaching" => "\u{1f4cb}",       // 📋
        "backlog" | "harbor" | "draft" => "\u{1f4cb}", // 📋
        "blocked" | "stranded" => "\u{274c}",          // ❌
        _ => "\u{2796}",                               // ➖
    }
}

/// Priority abbreviation for board index.
fn priority_abbrev(priority: &str) -> &'static str {
    match priority {
        "critical" => "CRIT",
        "high" => "HIGH",
        "medium" => "MEDI",
        "low" => "LOW",
        _ => "-",
    }
}

/// Extract the numeric ID part from an item ID (e.g., "EXP-1264" → 1264).
fn extract_id_number(id: &str) -> u32 {
    id.split('-')
        .next_back()
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Format depends_on list as short references.
fn format_depends_short(depends: &[String]) -> String {
    if depends.is_empty() {
        return "-".to_string();
    }
    depends
        .iter()
        .map(|d| {
            // Shorten: "EXP-1264" → "1264", "VOY-145" → "145"
            let num = extract_id_number(d);
            if num > 0 { num.to_string() } else { d.clone() }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Export a full board index — the KANBAN-BOARD.md format.
///
/// Generates a table of all items sorted by ID (descending), with status icons,
/// priority abbreviations, assignee, and dependency references.
pub fn export_board_index(
    batches: &[RecordBatch],
    board_name: &str,
    item_type_filter: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M");

    lines.push(format!(
        "# {} Board",
        if board_name == "development" {
            "Development"
        } else {
            "Research"
        }
    ));
    lines.push(String::new());
    lines.push(format!("*Generated: {now}*"));
    lines.push(String::new());

    // Collect all items into a sortable vec
    let mut items: Vec<ItemRow> = Vec::new();
    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);
        let depends_col = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("depends_on column");
        let tags_col = batch
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("tags column");

        for i in 0..batch.num_rows() {
            let item_type = types.value(i);
            if let Some(filter) = item_type_filter
                && item_type != filter
            {
                continue;
            }

            let depends = list_values(depends_col, i);
            let tags = list_values(tags_col, i);
            // Derive "For" column from first tag
            let for_col = tags.first().cloned().unwrap_or_else(|| "-".to_string());

            items.push(ItemRow {
                _id: ids.value(i).to_string(),
                id_num: extract_id_number(ids.value(i)),
                title: titles.value(i).to_string(),
                _item_type: item_type.to_string(),
                status: statuses.value(i).to_string(),
                priority: if priorities.is_null(i) {
                    "-".to_string()
                } else {
                    priorities.value(i).to_string()
                },
                assignee: if assignees.is_null(i) {
                    "-".to_string()
                } else {
                    assignees.value(i).to_string()
                },
                depends,
                for_tag: for_col,
            });
        }
    }

    // Sort by ID number descending (newest first)
    items.sort_by(|a, b| b.id_num.cmp(&a.id_num));

    // Group by item type for the header
    let type_label = item_type_filter.unwrap_or("Items");
    let count = items.len();

    lines.push(format!("## {} ({})", capitalize(type_label), count));
    lines.push(String::new());
    lines.push("| # | Title | Status | Pri | Agent | Depends | For |".to_string());
    lines.push("|---|-------|--------|-----|-------|---------|-----|".to_string());

    for item in &items {
        let icon = status_icon(&item.status);
        let pri = priority_abbrev(&item.priority);
        let depends_str = format_depends_short(&item.depends);
        // Truncate title to 50 chars (UTF-8 safe)
        let title = if item.title.chars().count() > 50 {
            let truncated: String = item.title.chars().take(47).collect();
            format!("{truncated}...")
        } else {
            item.title.clone()
        };
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            item.id_num, title, icon, pri, item.assignee, depends_str, item.for_tag
        ));
    }

    lines.push(String::new());
    lines.push(
        "**Legend:** \u{1f504} Active | \u{1f4cb} Backlog/Review | \u{2705} Done | \u{274c} Blocked"
            .to_string(),
    );
    lines.push(String::new());

    // Status summary
    let mut status_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for item in &items {
        *status_counts.entry(item.status.clone()).or_insert(0) += 1;
    }
    lines.push("## Status Summary".to_string());
    lines.push(String::new());
    lines.push("| Status | Count |".to_string());
    lines.push("|--------|-------|".to_string());
    let mut sorted_statuses: Vec<_> = status_counts.iter().collect();
    sorted_statuses.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (status, count) in &sorted_statuses {
        lines.push(format!(
            "| {} {} | {} |",
            status_icon(status),
            status,
            count
        ));
    }
    lines.push(format!("| **Total** | **{}** |", count));
    lines.push(String::new());

    lines.join("\n")
}

/// Export items as a JSON array (includes all fields needed for dependency analysis).
pub fn export_json(batches: &[RecordBatch]) -> String {
    let mut items = Vec::new();

    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);
        let bodies = col_str(batch, items_col::BODY);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<arrow::array::BooleanArray>()
            .expect("deleted column");
        let tags_col = batch
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("tags");
        let related_col = batch
            .column(items_col::RELATED)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("related");
        let depends_col = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("depends_on");

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }

            let tags = list_values(tags_col, i);
            let related = list_values(related_col, i);
            let depends_on = list_values(depends_col, i);

            let fmt_list = |vals: &[String]| -> String {
                vals.iter()
                    .map(|v| format!("\"{}\"", escape_json(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            };

            let body_json = if bodies.is_null(i) {
                "null".to_string()
            } else {
                format!("\"{}\"", escape_json(bodies.value(i)))
            };

            items.push(format!(
                r#"  {{"id":"{}","title":"{}","type":"{}","status":"{}","priority":{},"assignee":{},"tags":[{}],"related":[{}],"depends_on":[{}],"body":{}}}"#,
                escape_json(ids.value(i)),
                escape_json(titles.value(i)),
                escape_json(types.value(i)),
                escape_json(statuses.value(i)),
                if priorities.is_null(i) { "null".to_string() } else { format!("\"{}\"", escape_json(priorities.value(i))) },
                if assignees.is_null(i) { "null".to_string() } else { format!("\"{}\"", escape_json(assignees.value(i))) },
                fmt_list(&tags),
                fmt_list(&related),
                fmt_list(&depends_on),
                body_json,
            ));
        }
    }

    format!("[\n{}\n]", items.join(",\n"))
}

/// Export items as a simple markdown table.
pub fn export_markdown_table(batches: &[RecordBatch]) -> String {
    let mut lines = Vec::new();
    lines.push("| ID | Title | Type | Status | Priority | Assignee |".to_string());
    lines.push("|---|---|---|---|---|---|".to_string());

    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);

        for i in 0..batch.num_rows() {
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                ids.value(i),
                titles.value(i),
                types.value(i),
                statuses.value(i),
                if priorities.is_null(i) {
                    "-"
                } else {
                    priorities.value(i)
                },
                if assignees.is_null(i) {
                    "-"
                } else {
                    assignees.value(i)
                },
            ));
        }
    }

    lines.join("\n")
}

/// Export a full board as standalone HTML with inline CSS.
///
/// Generates a responsive HTML page with:
/// - Status badges with colored backgrounds
/// - Sortable responsive table layout
/// - Status summary section
/// - Optional burndown SVG chart
pub fn export_board_html(
    batches: &[RecordBatch],
    board_name: &str,
    item_type_filter: Option<&str>,
    burndown_points: Option<&[BurndownPoint]>,
) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let board_label = if board_name == "development" {
        "Development"
    } else {
        "Research"
    };

    // Collect items
    let mut items: Vec<ItemRow> = Vec::new();
    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);
        let depends_col = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("depends_on column");
        let tags_col = batch
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("tags column");

        for i in 0..batch.num_rows() {
            let item_type = types.value(i);
            if let Some(filter) = item_type_filter
                && item_type != filter
            {
                continue;
            }

            let depends = list_values(depends_col, i);
            let tags = list_values(tags_col, i);
            let for_col = tags.first().cloned().unwrap_or_else(|| "-".to_string());

            items.push(ItemRow {
                _id: ids.value(i).to_string(),
                id_num: extract_id_number(ids.value(i)),
                title: titles.value(i).to_string(),
                _item_type: item_type.to_string(),
                status: statuses.value(i).to_string(),
                priority: if priorities.is_null(i) {
                    "-".to_string()
                } else {
                    priorities.value(i).to_string()
                },
                assignee: if assignees.is_null(i) {
                    "-".to_string()
                } else {
                    assignees.value(i).to_string()
                },
                depends,
                for_tag: for_col,
            });
        }
    }

    items.sort_by(|a, b| b.id_num.cmp(&a.id_num));

    // Status summary
    let mut status_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for item in &items {
        *status_counts.entry(item.status.clone()).or_insert(0) += 1;
    }

    let type_label = item_type_filter.unwrap_or("Items");

    // Build table rows
    let mut rows_html = String::new();
    for item in &items {
        let status_class = status_css_class(&item.status);
        let pri_class = priority_css_class(&item.priority);
        let title_escaped = escape_html(&item.title);
        let depends_str = format_depends_short(&item.depends);

        rows_html.push_str(&format!(
            "      <tr>\n\
             \x20       <td class=\"id\">{}</td>\n\
             \x20       <td class=\"title\">{}</td>\n\
             \x20       <td><span class=\"badge {}\">{}</span></td>\n\
             \x20       <td><span class=\"badge {}\">{}</span></td>\n\
             \x20       <td>{}</td>\n\
             \x20       <td class=\"depends\">{}</td>\n\
             \x20       <td class=\"tag\">{}</td>\n\
             \x20     </tr>\n",
            item.id_num,
            title_escaped,
            status_class,
            escape_html(&item.status),
            pri_class,
            escape_html(priority_abbrev(&item.priority)),
            escape_html(&item.assignee),
            escape_html(&depends_str),
            escape_html(&item.for_tag),
        ));
    }

    // Status summary rows
    let mut summary_html = String::new();
    let mut sorted_statuses: Vec<_> = status_counts.iter().collect();
    sorted_statuses.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (status, count) in &sorted_statuses {
        let cls = status_css_class(status);
        summary_html.push_str(&format!(
            "      <tr><td><span class=\"badge {cls}\">{status}</span></td><td>{count}</td></tr>\n"
        ));
    }
    summary_html.push_str(&format!(
        "      <tr class=\"total\"><td><strong>Total</strong></td><td><strong>{}</strong></td></tr>\n",
        items.len()
    ));

    // Burndown SVG
    let burndown_section = if let Some(points) = burndown_points {
        format!("  <h2>Burndown</h2>\n  {}\n", burndown_svg(points))
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{board_label} Board — NuSy Kanban</title>
  <style>
{CSS}
  </style>
</head>
<body>
  <h1>{board_label} Board</h1>
  <p class="generated">Generated: {now}</p>

  <h2>{type_label_cap} ({count})</h2>
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>#</th><th>Title</th><th>Status</th><th>Pri</th>
          <th>Agent</th><th>Depends</th><th>For</th>
        </tr>
      </thead>
      <tbody>
{rows_html}      </tbody>
    </table>
  </div>

  <h2>Status Summary</h2>
  <table class="summary">
    <thead><tr><th>Status</th><th>Count</th></tr></thead>
    <tbody>
{summary_html}    </tbody>
  </table>

{burndown_section}</body>
</html>"#,
        type_label_cap = capitalize(type_label),
        count = items.len(),
    )
}

/// Generate an inline SVG burndown chart from burndown data points.
pub fn burndown_svg(points: &[BurndownPoint]) -> String {
    if points.is_empty() {
        return "<p>No burndown data.</p>".to_string();
    }

    let width = 600u32;
    let height = 300u32;
    let pad_left = 50u32;
    let pad_right = 20u32;
    let pad_top = 20u32;
    let pad_bottom = 40u32;
    let chart_w = width - pad_left - pad_right;
    let chart_h = height - pad_top - pad_bottom;

    let max_remaining = points.iter().map(|p| p.remaining).max().unwrap_or(1).max(1);
    let n = points.len();

    // Build polyline points
    let mut polyline = String::new();
    let mut circles = String::new();
    let mut labels = String::new();

    for (i, p) in points.iter().enumerate() {
        let x = pad_left as f64 + (i as f64 / (n - 1).max(1) as f64) * chart_w as f64;
        let y = pad_top as f64 + (1.0 - p.remaining as f64 / max_remaining as f64) * chart_h as f64;

        polyline.push_str(&format!("{:.1},{:.1} ", x, y));
        circles.push_str(&format!(
            "    <circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"3\" fill=\"#2563eb\"/>\n"
        ));

        // X-axis labels (show every other label if many points)
        let show = n <= 8 || i % 2 == 0 || i == n - 1;
        if show {
            labels.push_str(&format!(
                "    <text x=\"{x:.1}\" y=\"{}\" text-anchor=\"middle\" \
                 class=\"axis-label\">{}</text>\n",
                height - 8,
                escape_html(&p.label)
            ));
        }
    }

    // Y-axis labels (5 ticks)
    let mut y_labels = String::new();
    for tick in 0..=4 {
        let val = max_remaining as f64 * tick as f64 / 4.0;
        let y = pad_top as f64 + (1.0 - tick as f64 / 4.0) * chart_h as f64;
        y_labels.push_str(&format!(
            "    <text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\" \
             class=\"axis-label\">{}</text>\n\
             \x20   <line x1=\"{pad_left}\" y1=\"{y:.1}\" x2=\"{}\" y2=\"{y:.1}\" \
             stroke=\"#e5e7eb\" stroke-width=\"1\"/>\n",
            pad_left - 6,
            y + 4.0,
            val.round() as u32,
            width - pad_right,
        ));
    }

    format!(
        r##"<svg viewBox="0 0 {width} {height}" xmlns="http://www.w3.org/2000/svg"
   style="max-width:{width}px;width:100%;font-family:system-ui,sans-serif;">
    <style>
      .axis-label {{ font-size: 11px; fill: #6b7280; }}
    </style>
{y_labels}{circles}    <polyline points="{polyline}" fill="none" stroke="#2563eb" stroke-width="2"/>
{labels}  </svg>"##
    )
}

/// Export a research index as standalone HTML: paper → hypothesis → experiment → measure tree.
pub fn export_research_index_html(chains: &[RegistryChain]) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");

    let mut tree_html = String::new();
    if chains.is_empty() {
        tree_html.push_str("  <p>No papers found on the research board.</p>\n");
    } else {
        tree_html.push_str("  <ul class=\"tree\">\n");
        for chain in chains {
            tree_html.push_str(&format!(
                "    <li class=\"paper\"><span class=\"badge badge-paper\">{}</span> {}\n",
                escape_html(&chain.paper_id),
                escape_html(&chain.paper_title),
            ));
            if !chain.hypotheses.is_empty() {
                tree_html.push_str("      <ul>\n");
                for hyp in &chain.hypotheses {
                    tree_html.push_str(&format!(
                        "        <li class=\"hypothesis\"><span class=\"badge badge-hyp\">{}</span> {}\n",
                        escape_html(&hyp.id),
                        escape_html(&hyp.title),
                    ));
                    if !hyp.experiments.is_empty() {
                        tree_html.push_str("          <ul>\n");
                        for expr in &hyp.experiments {
                            tree_html.push_str(&format!(
                                "            <li class=\"experiment\"><span class=\"badge badge-expr\">{}</span> {}\n",
                                escape_html(&expr.id),
                                escape_html(&expr.title),
                            ));
                            if !expr.measures.is_empty() {
                                tree_html.push_str("              <ul>\n");
                                for m in &expr.measures {
                                    tree_html.push_str(&format!(
                                        "                <li class=\"measure\"><span class=\"badge badge-measure\">{}</span> {}</li>\n",
                                        escape_html(&m.id),
                                        escape_html(&m.title),
                                    ));
                                }
                                tree_html.push_str("              </ul>\n");
                            }
                            tree_html.push_str("            </li>\n");
                        }
                        tree_html.push_str("          </ul>\n");
                    }
                    tree_html.push_str("        </li>\n");
                }
                tree_html.push_str("      </ul>\n");
            }
            tree_html.push_str("    </li>\n");
        }
        tree_html.push_str("  </ul>\n");
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Research Index — NuSy Kanban</title>
  <style>
{CSS}
{TREE_CSS}
  </style>
</head>
<body>
  <h1>Research Index</h1>
  <p class="generated">Generated: {now}</p>
  <p>Paper &rarr; Hypothesis &rarr; Experiment &rarr; Measure</p>

{tree_html}</body>
</html>"#,
    )
}

// ─── HTML helpers ──────────────────────────────────────────────────────────

/// Inline CSS for HTML exports (no external dependencies).
const CSS: &str = r#"    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
      max-width: 960px; margin: 0 auto; padding: 1rem;
      color: #1f2937; background: #f9fafb;
    }
    h1 { margin-bottom: 0.25rem; }
    h2 { margin: 1.5rem 0 0.5rem; }
    p.generated { color: #6b7280; font-size: 0.85rem; margin-bottom: 1rem; }
    .table-wrap { overflow-x: auto; }
    table { border-collapse: collapse; width: 100%; font-size: 0.9rem; }
    th, td { padding: 0.4rem 0.6rem; text-align: left; border-bottom: 1px solid #e5e7eb; }
    th { background: #f3f4f6; font-weight: 600; position: sticky; top: 0; }
    tr:hover { background: #f0f4ff; }
    .id { font-family: monospace; white-space: nowrap; }
    .title { max-width: 320px; }
    .depends, .tag { font-family: monospace; font-size: 0.85rem; color: #6b7280; }
    .badge {
      display: inline-block; padding: 0.15rem 0.5rem; border-radius: 9999px;
      font-size: 0.78rem; font-weight: 500; white-space: nowrap;
    }
    .status-done       { background: #d1fae5; color: #065f46; }
    .status-in_progress { background: #dbeafe; color: #1e40af; }
    .status-review     { background: #fef3c7; color: #92400e; }
    .status-backlog    { background: #f3f4f6; color: #374151; }
    .status-blocked    { background: #fee2e2; color: #991b1b; }
    .status-default    { background: #f3f4f6; color: #374151; }
    .pri-critical      { background: #fee2e2; color: #991b1b; }
    .pri-high          { background: #fef3c7; color: #92400e; }
    .pri-medium        { background: #e0e7ff; color: #3730a3; }
    .pri-low           { background: #f3f4f6; color: #374151; }
    .pri-default       { background: #f3f4f6; color: #6b7280; }
    table.summary { max-width: 280px; }
    tr.total td { border-top: 2px solid #9ca3af; }
    @media (max-width: 640px) {
      body { padding: 0.5rem; }
      th, td { padding: 0.3rem 0.4rem; font-size: 0.8rem; }
    }"#;

/// Additional CSS for the research index tree view.
const TREE_CSS: &str = r#"    .tree, .tree ul { list-style: none; padding-left: 1.5rem; }
    .tree > li { padding-left: 0; }
    .tree li { padding: 0.3rem 0; position: relative; }
    .tree li::before {
      content: ''; position: absolute; left: -1rem; top: 0;
      border-left: 1px solid #d1d5db; height: 100%;
    }
    .tree li::after {
      content: ''; position: absolute; left: -1rem; top: 0.9rem;
      border-top: 1px solid #d1d5db; width: 0.8rem;
    }
    .tree > li::before, .tree > li::after { display: none; }
    .tree li:last-child::before { height: 0.9rem; }
    .badge-paper   { background: #dbeafe; color: #1e40af; }
    .badge-hyp     { background: #fef3c7; color: #92400e; }
    .badge-expr    { background: #d1fae5; color: #065f46; }
    .badge-measure { background: #e0e7ff; color: #3730a3; }"#;

fn status_css_class(status: &str) -> &'static str {
    match status {
        "done" | "complete" | "arrived" => "status-done",
        "in_progress" | "underway" => "status-in_progress",
        "review" | "approaching" => "status-review",
        "backlog" | "harbor" | "draft" => "status-backlog",
        "blocked" | "stranded" => "status-blocked",
        _ => "status-default",
    }
}

fn priority_css_class(priority: &str) -> &'static str {
    match priority {
        "critical" => "pri-critical",
        "high" => "pri-high",
        "medium" => "pri-medium",
        "low" => "pri-low",
        _ => "pri-default",
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Export a single item batch row to a markdown string with YAML frontmatter.
pub fn item_to_markdown(batch: &RecordBatch, row: usize) -> String {
    let ids = col_str(batch, items_col::ID);
    let titles = col_str(batch, items_col::TITLE);
    let types = col_str(batch, items_col::ITEM_TYPE);
    let statuses = col_str(batch, items_col::STATUS);
    let priorities = col_str(batch, items_col::PRIORITY);
    let assignees = col_str(batch, items_col::ASSIGNEE);
    let tags_col = batch
        .column(items_col::TAGS)
        .as_any()
        .downcast_ref::<ListArray>()
        .expect("tags column");
    let related_col = batch
        .column(items_col::RELATED)
        .as_any()
        .downcast_ref::<ListArray>()
        .expect("related column");
    let depends_col = batch
        .column(items_col::DEPENDS_ON)
        .as_any()
        .downcast_ref::<ListArray>()
        .expect("depends_on column");

    let id = ids.value(row);
    let title = titles.value(row);
    let item_type = types.value(row);
    let status = statuses.value(row);

    let mut lines = Vec::new();
    lines.push("---".to_string());
    lines.push(format!("id: {id}"));
    lines.push(format!("title: \"{title}\""));
    lines.push(format!("type: {item_type}"));
    lines.push(format!("status: {status}"));

    if !priorities.is_null(row) {
        lines.push(format!("priority: {}", priorities.value(row)));
    }

    if !assignees.is_null(row) {
        lines.push(format!("assignee: {}", assignees.value(row)));
    }

    let tags = list_values(tags_col, row);
    if !tags.is_empty() {
        lines.push(format!("tags: [{}]", tags.join(", ")));
    }

    let related = list_values(related_col, row);
    if !related.is_empty() {
        lines.push(format!("related: [{}]", related.join(", ")));
    }

    let depends = list_values(depends_col, row);
    if !depends.is_empty() {
        lines.push(format!("depends_on: [{}]", depends.join(", ")));
    }

    lines.push("---".to_string());
    lines.push(String::new());

    // Append body content if present; otherwise emit a heading
    let bodies = col_str(batch, items_col::BODY);
    if !bodies.is_null(row) {
        let body = bodies.value(row).trim();
        if !body.is_empty() {
            lines.push(body.to_string());
            lines.push(String::new());
        } else {
            lines.push(format!("# {id}: {title}"));
            lines.push(String::new());
        }
    } else {
        lines.push(format!("# {id}: {title}"));
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Get the next available ID for a given type prefix, as JSON.
pub fn next_id_json(prefix: &str, next_num: u32) -> String {
    format!(
        r#"{{"prefix":"{}","number":{},"id":"{}-{}"}}"#,
        prefix, next_num, prefix, next_num
    )
}

// ─── Internal types ─────────────────────────────────────────────────────────

struct ItemRow {
    _id: String,
    id_num: u32,
    title: String,
    _item_type: String,
    status: String,
    priority: String,
    assignee: String,
    depends: Vec<String>,
    for_tag: String,
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Escape a string for safe JSON interpolation.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn col_str(batch: &RecordBatch, col: usize) -> &StringArray {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

fn list_values(arr: &ListArray, row: usize) -> Vec<String> {
    if arr.is_null(row) {
        return Vec::new();
    }
    let values = arr.value(row);
    let str_arr = values
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("list string values");
    (0..str_arr.len())
        .map(|i| str_arr.value(i).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::{CreateItemInput, KanbanStore};
    use crate::item_type::ItemType;

    fn create_test_store() -> KanbanStore {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("critical".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec!["v14".to_string()],
                related: vec!["VOY-145".to_string()],
                depends_on: vec!["EXP-100".to_string()],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "CLI Parity".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("Mini".to_string()),
                tags: vec!["v14".to_string(), "cli".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Research Paper".to_string(),
                item_type: ItemType::Paper,
                priority: Some("medium".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
    }

    #[test]
    fn test_item_to_markdown() {
        let store = create_test_store();
        let item = store.get_item("EX-1300").expect("get");
        let md = item_to_markdown(&item, 0);

        assert!(md.contains("---"));
        assert!(md.contains("id: EX-1300"));
        assert!(md.contains("title: \"Arrow Engine\""));
        assert!(md.contains("type: expedition"));
        assert!(md.contains("priority: critical"));
        assert!(md.contains("assignee: M5"));
        assert!(md.contains("tags: [v14]"));
        assert!(md.contains("depends_on: [EXP-100]"));
    }

    #[test]
    fn test_next_id_json() {
        let json = next_id_json("EX", 1300);
        assert_eq!(json, r#"{"prefix":"EX","number":1300,"id":"EX-1300"}"#);
    }

    #[test]
    fn test_item_to_markdown_no_optional_fields() {
        let store = create_test_store();
        let item = store.get_item("PAPER-1302").expect("get");
        let md = item_to_markdown(&item, 0);

        assert!(md.contains("id: PAPER-1302"));
        assert!(!md.contains("assignee:"));
    }

    #[test]
    fn test_board_index_contains_all_items() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let output = export_board_index(&batches, "development", None);

        assert!(output.contains("# Development Board"));
        assert!(output.contains("Arrow Engine"));
        assert!(output.contains("CLI Parity"));
        assert!(output.contains("CRIT"));
        assert!(output.contains("HIGH"));
        assert!(output.contains("M5"));
        assert!(output.contains("Mini"));
    }

    #[test]
    fn test_board_index_has_status_summary() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let output = export_board_index(&batches, "development", None);

        assert!(output.contains("## Status Summary"));
        assert!(output.contains("backlog"));
        assert!(output.contains("**Total**"));
    }

    #[test]
    fn test_board_index_depends_formatting() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let output = export_board_index(&batches, "development", None);

        // EXP-1 depends on EXP-100 → should show "100"
        assert!(output.contains("100"));
    }

    #[test]
    fn test_board_index_with_type_filter() {
        let mut store = create_test_store();
        // Add a non-expedition item to dev board
        store
            .create_item(&CreateItemInput {
                title: "Cleanup Task".to_string(),
                item_type: ItemType::Chore,
                priority: Some("low".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create chore");

        let batches = store.query_items(None, None, Some("development"), None);
        let output = export_board_index(&batches, "development", Some("expedition"));

        // Expeditions should appear
        assert!(output.contains("Arrow Engine"));
        assert!(output.contains("CLI Parity"));
        // Chore should NOT appear (filtered by type)
        assert!(
            !output.contains("Cleanup Task"),
            "Chore should be excluded by expedition type filter"
        );
    }

    #[test]
    fn test_export_json() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let json = export_json(&batches);

        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains("\"id\":\"EX-1300\""));
        assert!(json.contains("\"title\":\"Arrow Engine\""));
        assert!(json.contains("\"priority\":\"critical\""));
    }

    #[test]
    fn test_export_markdown_table() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let md = export_markdown_table(&batches);

        assert!(md.contains("| ID | Title |"));
        assert!(md.contains("| EX-1300 | Arrow Engine |"));
        assert!(md.contains("| EX-1301 | CLI Parity |"));
    }

    #[test]
    fn test_research_board_index() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("research"), None);
        let output = export_board_index(&batches, "research", None);

        assert!(output.contains("# Research Board"));
        assert!(output.contains("Research Paper"));
    }

    #[test]
    fn test_empty_board_index() {
        let batches: Vec<RecordBatch> = vec![];
        let output = export_board_index(&batches, "development", None);

        assert!(output.contains("# Development Board"));
        assert!(output.contains("Items (0)"));
        assert!(output.contains("**Total** | **0**"));
    }

    #[test]
    fn test_export_json_empty() {
        let batches: Vec<RecordBatch> = vec![];
        let json = export_json(&batches);
        assert_eq!(json, "[\n\n]");
    }

    #[test]
    fn test_status_icons() {
        assert_eq!(status_icon("done"), "\u{2705}");
        assert_eq!(status_icon("in_progress"), "\u{1f504}");
        assert_eq!(status_icon("backlog"), "\u{1f4cb}");
        assert_eq!(status_icon("blocked"), "\u{274c}");
    }

    #[test]
    fn test_priority_abbrev() {
        assert_eq!(priority_abbrev("critical"), "CRIT");
        assert_eq!(priority_abbrev("high"), "HIGH");
        assert_eq!(priority_abbrev("medium"), "MEDI");
        assert_eq!(priority_abbrev("low"), "LOW");
    }

    #[test]
    fn test_item_to_markdown_with_body() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Rich Content".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("DGX".to_string()),
                tags: vec!["v14".to_string()],
                related: vec![],
                depends_on: vec![],
                body: Some(
                    "# EX-1300: Rich Content\n\n## Why This Exists\n\nBecause reasons.\n\n## Phase 1\n\nDo it.".to_string(),
                ),
            })
            .expect("create");
        let item = store.get_item("EX-1300").expect("get");
        let md = item_to_markdown(&item, 0);

        // Should contain frontmatter
        assert!(md.contains("---"));
        assert!(md.contains("id: EX-1300"));
        // Body replaces the heading — should NOT have duplicate heading
        assert!(md.contains("# EX-1300: Rich Content"));
        // Should contain body content
        assert!(md.contains("## Why This Exists"));
        assert!(md.contains("Because reasons."));
        assert!(md.contains("## Phase 1"));
    }

    #[test]
    fn test_item_to_markdown_without_body() {
        let store = create_test_store();
        let item = store.get_item("EX-1300").expect("get");
        let md = item_to_markdown(&item, 0);

        assert!(md.contains("# EX-1300: Arrow Engine"));
        // No body content — just frontmatter + heading
        assert!(!md.contains("## Phase"));
    }

    #[test]
    fn test_export_json_with_body() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "JSON Body".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("Body content here".to_string()),
            })
            .expect("create");

        let batches = store.query_items(None, None, None, None);
        let json = export_json(&batches);
        assert!(json.contains("\"body\":\"Body content here\""));
    }

    #[test]
    fn test_export_json_null_body() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let json = export_json(&batches);
        assert!(json.contains("\"body\":null"));
    }

    #[test]
    fn test_round_trip_create_body_export() {
        // Create an item with body content (simulating --body-file)
        let body = "# EX-1300: Round Trip Test\n\n\
                     ## Phase 1: Setup\n\n\
                     - **What:** Do the thing\n\
                     - **Acceptance criteria:** Thing is done\n\n\
                     ## Phase 2: Verify\n\n\
                     Run tests.\n\n\
                     ## Constraints\n\n\
                     - Don't break anything";

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Round Trip Test".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("DGX".to_string()),
                tags: vec!["v14".to_string(), "test".to_string()],
                related: vec!["VOY-152".to_string()],
                depends_on: vec!["EXP-1280".to_string()],
                body: Some(body.to_string()),
            })
            .expect("create");

        // Export to markdown
        let item = store.get_item("EX-1300").expect("get");
        let md = item_to_markdown(&item, 0);

        // Verify frontmatter is correct
        assert!(md.contains("id: EX-1300"));
        assert!(md.contains("title: \"Round Trip Test\""));
        assert!(md.contains("type: expedition"));
        assert!(md.contains("status: backlog"));
        assert!(md.contains("priority: high"));
        assert!(md.contains("assignee: DGX"));
        assert!(md.contains("tags: [v14, test]"));
        assert!(md.contains("related: [VOY-152]"));
        assert!(md.contains("depends_on: [EXP-1280]"));

        // Verify body content survives round-trip exactly
        assert!(md.contains("## Phase 1: Setup"));
        assert!(md.contains("- **What:** Do the thing"));
        assert!(md.contains("## Phase 2: Verify"));
        assert!(md.contains("Run tests."));
        assert!(md.contains("## Constraints"));
        assert!(md.contains("- Don't break anything"));

        // Verify NO duplicate heading (body already contains # EX-1300: ...)
        let heading_count = md.matches("# EX-1300: Round Trip Test").count();
        assert_eq!(heading_count, 1, "heading should appear exactly once");
    }

    // ─── Phase 1: HTML Export Tests ────────────────────────────────────────

    #[test]
    fn test_html_export_is_standalone() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html lang=\"en\">"));
        assert!(html.contains("<style>"));
        assert!(html.contains("</html>"));
        // No external CSS/JS references
        assert!(!html.contains("<link"));
        assert!(!html.contains("<script src"));
    }

    #[test]
    fn test_html_export_contains_items() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.contains("Arrow Engine"));
        assert!(html.contains("CLI Parity"));
        assert!(html.contains("Development Board"));
    }

    #[test]
    fn test_html_export_status_badges() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        // Status badges should have CSS classes
        assert!(html.contains("status-backlog"));
        assert!(html.contains("class=\"badge"));
    }

    #[test]
    fn test_html_export_priority_badges() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.contains("CRIT"));
        assert!(html.contains("HIGH"));
        assert!(html.contains("pri-critical"));
        assert!(html.contains("pri-high"));
    }

    #[test]
    fn test_html_export_status_summary() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.contains("Status Summary"));
        assert!(html.contains("<strong>Total</strong>"));
    }

    #[test]
    fn test_html_export_responsive() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.contains("viewport"));
        assert!(html.contains("@media"));
        assert!(html.contains("table-wrap"));
    }

    #[test]
    fn test_html_export_escapes_html() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "XSS <script>alert(1)</script>".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_html_export_empty_board() {
        let batches: Vec<RecordBatch> = vec![];
        let html = export_board_html(&batches, "development", None, None);

        assert!(html.contains("Development Board"));
        assert!(html.contains("(0)"));
        assert!(html.contains("<strong>0</strong>"));
    }

    #[test]
    fn test_html_export_research_board() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("research"), None);
        let html = export_board_html(&batches, "research", None, None);

        assert!(html.contains("Research Board"));
        assert!(html.contains("Research Paper"));
    }

    #[test]
    fn test_html_export_with_burndown() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let points = vec![
            BurndownPoint {
                timestamp_ms: 0,
                label: "Mar 4".to_string(),
                remaining: 10,
            },
            BurndownPoint {
                timestamp_ms: 604800000,
                label: "Mar 11".to_string(),
                remaining: 7,
            },
            BurndownPoint {
                timestamp_ms: 1209600000,
                label: "Mar 18".to_string(),
                remaining: 3,
            },
        ];
        let html = export_board_html(&batches, "development", None, Some(&points));

        assert!(html.contains("<svg"));
        assert!(html.contains("Burndown"));
        assert!(html.contains("Mar 4"));
        assert!(html.contains("Mar 18"));
    }

    #[test]
    fn test_html_export_without_burndown() {
        let store = create_test_store();
        let batches = store.query_items(None, None, Some("development"), None);
        let html = export_board_html(&batches, "development", None, None);

        assert!(!html.contains("<svg"));
        assert!(!html.contains("Burndown"));
    }

    // ─── Phase 2: Research Index Tests ─────────────────────────────────────

    #[test]
    fn test_research_index_empty() {
        let chains: Vec<RegistryChain> = vec![];
        let html = export_research_index_html(&chains);

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("Research Index"));
        assert!(html.contains("No papers found"));
    }

    #[test]
    fn test_research_index_with_chains() {
        use crate::hdd::{RegistryExperiment, RegistryHypothesis, RegistryItem};

        let chains = vec![RegistryChain {
            paper_id: "PAPER-130".to_string(),
            paper_title: "Perception Study".to_string(),
            hypotheses: vec![RegistryHypothesis {
                id: "H130.1".to_string(),
                title: "Beings prefer visual cues".to_string(),
                experiments: vec![RegistryExperiment {
                    id: "EXPR-130.1".to_string(),
                    title: "A/B visual vs text".to_string(),
                    measures: vec![RegistryItem {
                        id: "M-42".to_string(),
                        title: "Response accuracy".to_string(),
                    }],
                }],
            }],
        }];

        let html = export_research_index_html(&chains);

        assert!(html.contains("Research Index"));
        assert!(html.contains("PAPER-130"));
        assert!(html.contains("Perception Study"));
        assert!(html.contains("H130.1"));
        assert!(html.contains("Beings prefer visual cues"));
        assert!(html.contains("EXPR-130.1"));
        assert!(html.contains("A/B visual vs text"));
        assert!(html.contains("M-42"));
        assert!(html.contains("Response accuracy"));
    }

    #[test]
    fn test_research_index_tree_badges() {
        use crate::hdd::{RegistryExperiment, RegistryHypothesis, RegistryItem};

        let chains = vec![RegistryChain {
            paper_id: "PAPER-5".to_string(),
            paper_title: "Test Paper".to_string(),
            hypotheses: vec![RegistryHypothesis {
                id: "H5.1".to_string(),
                title: "Test Hypothesis".to_string(),
                experiments: vec![RegistryExperiment {
                    id: "EXPR-5.1".to_string(),
                    title: "Test Experiment".to_string(),
                    measures: vec![RegistryItem {
                        id: "M-1".to_string(),
                        title: "Test Measure".to_string(),
                    }],
                }],
            }],
        }];

        let html = export_research_index_html(&chains);

        assert!(html.contains("badge-paper"));
        assert!(html.contains("badge-hyp"));
        assert!(html.contains("badge-expr"));
        assert!(html.contains("badge-measure"));
        assert!(html.contains("class=\"tree\""));
    }

    #[test]
    fn test_research_index_standalone() {
        let chains: Vec<RegistryChain> = vec![];
        let html = export_research_index_html(&chains);

        assert!(!html.contains("<link"));
        assert!(!html.contains("<script src"));
        assert!(html.contains("<style>"));
    }

    #[test]
    fn test_research_index_escapes_html() {
        use crate::hdd::RegistryHypothesis;

        let chains = vec![RegistryChain {
            paper_id: "PAPER-1".to_string(),
            paper_title: "Paper with <dangerous> chars & \"quotes\"".to_string(),
            hypotheses: vec![RegistryHypothesis {
                id: "H1.1".to_string(),
                title: "Hyp with <b>bold</b>".to_string(),
                experiments: vec![],
            }],
        }];

        let html = export_research_index_html(&chains);

        assert!(html.contains("&lt;dangerous&gt;"));
        assert!(html.contains("&amp; &quot;quotes&quot;"));
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"));
    }

    // ─── Phase 3: Burndown SVG Tests ───────────────────────────────────────

    #[test]
    fn test_burndown_svg_empty() {
        let points: Vec<BurndownPoint> = vec![];
        let svg = burndown_svg(&points);

        assert!(svg.contains("No burndown data"));
        assert!(!svg.contains("<svg"));
    }

    #[test]
    fn test_burndown_svg_generates_svg() {
        let points = vec![
            BurndownPoint {
                timestamp_ms: 0,
                label: "Mar 4".to_string(),
                remaining: 10,
            },
            BurndownPoint {
                timestamp_ms: 604800000,
                label: "Mar 11".to_string(),
                remaining: 5,
            },
        ];

        let svg = burndown_svg(&points);

        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("<polyline"));
        assert!(svg.contains("<circle"));
        assert!(svg.contains("Mar 4"));
        assert!(svg.contains("Mar 11"));
    }

    #[test]
    fn test_burndown_svg_y_axis_labels() {
        let points = vec![
            BurndownPoint {
                timestamp_ms: 0,
                label: "W1".to_string(),
                remaining: 20,
            },
            BurndownPoint {
                timestamp_ms: 604800000,
                label: "W2".to_string(),
                remaining: 10,
            },
        ];

        let svg = burndown_svg(&points);

        // Y-axis should show values from 0 to max
        assert!(svg.contains(">0<"));
        assert!(svg.contains(">20<"));
    }

    #[test]
    fn test_burndown_svg_single_point() {
        let points = vec![BurndownPoint {
            timestamp_ms: 0,
            label: "Now".to_string(),
            remaining: 5,
        }];

        let svg = burndown_svg(&points);

        assert!(svg.contains("<svg"));
        assert!(svg.contains("<circle"));
        assert!(svg.contains("Now"));
    }

    // ─── HTML helper tests ─────────────────────────────────────────────────

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("a < b"), "a &lt; b");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("a > b"), "a &gt; b");
        assert_eq!(escape_html("a \"b\""), "a &quot;b&quot;");
        assert_eq!(escape_html("no special"), "no special");
    }

    #[test]
    fn test_status_css_class() {
        assert_eq!(status_css_class("done"), "status-done");
        assert_eq!(status_css_class("in_progress"), "status-in_progress");
        assert_eq!(status_css_class("review"), "status-review");
        assert_eq!(status_css_class("backlog"), "status-backlog");
        assert_eq!(status_css_class("blocked"), "status-blocked");
        assert_eq!(status_css_class("unknown"), "status-default");
    }

    #[test]
    fn test_priority_css_class() {
        assert_eq!(priority_css_class("critical"), "pri-critical");
        assert_eq!(priority_css_class("high"), "pri-high");
        assert_eq!(priority_css_class("medium"), "pri-medium");
        assert_eq!(priority_css_class("low"), "pri-low");
        assert_eq!(priority_css_class("-"), "pri-default");
    }
}
