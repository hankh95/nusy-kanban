//! TTL-based theme system — maps type URIs to display labels.
//!
//! EX-3224: Themes control how item types are displayed (nautical: "Expedition",
//! agile: "Story", standard: "Work Item") without changing the underlying type URIs
//! or SHACL shapes.
//!
//! Override hierarchy: project (.yurtle-kanban/themes/) → built-in (compiled).

use std::collections::HashMap;
use std::path::Path;

/// A loaded theme — maps item type keys to display labels.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Theme name (e.g., "nautical", "agile", "standard").
    pub name: String,
    /// Type key → display label (e.g., "expedition" → "Story" in agile theme).
    labels: HashMap<String, String>,
    /// Type key → default priority.
    defaults: HashMap<String, String>,
}

impl Theme {
    /// Get the display label for an item type. Falls back to the type key if not themed.
    pub fn label<'a>(&'a self, type_key: &'a str) -> &'a str {
        self.labels
            .get(type_key)
            .map(|s| s.as_str())
            .unwrap_or(type_key)
    }

    /// Get the default priority for a type. Returns "medium" if not set.
    pub fn default_priority<'a>(&'a self, type_key: &'a str) -> &'a str {
        self.defaults
            .get(type_key)
            .map(|s| s.as_str())
            .unwrap_or("medium")
    }

    /// List all themed type keys.
    pub fn themed_types(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.labels.keys().map(|s| s.as_str()).collect();
        keys.sort();
        keys
    }

    /// Load the nautical theme (NuSy default).
    pub fn nautical() -> Self {
        parse_theme("nautical", include_str!("../themes/nautical.ttl"))
    }

    /// Load the agile theme.
    pub fn agile() -> Self {
        parse_theme("agile", include_str!("../themes/agile.ttl"))
    }

    /// Load the standard theme.
    pub fn standard() -> Self {
        parse_theme("standard", include_str!("../themes/standard.ttl"))
    }

    /// Load a theme by name. Checks project-local first, then built-in.
    pub fn load(name: &str, project_root: Option<&Path>) -> Option<Self> {
        // Project-local override
        if let Some(root) = project_root {
            let path = root.join(format!(".yurtle-kanban/themes/{name}.ttl"));
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(parse_theme(name, &content));
            }
        }

        // Built-in
        match name {
            "nautical" => Some(Self::nautical()),
            "agile" => Some(Self::agile()),
            "standard" => Some(Self::standard()),
            _ => None,
        }
    }

    /// List all available theme names (built-in + project-local).
    pub fn available_themes(project_root: Option<&Path>) -> Vec<String> {
        let mut names = vec![
            "nautical".to_string(),
            "agile".to_string(),
            "standard".to_string(),
        ];

        if let Some(root) = project_root {
            let themes_dir = root.join(".yurtle-kanban/themes");
            if let Ok(entries) = std::fs::read_dir(themes_dir) {
                for entry in entries.flatten() {
                    if let Some(name) = entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        && !names.contains(&name)
                    {
                        names.push(name);
                    }
                }
            }
        }

        names.sort();
        names
    }

    /// Format theme for display (nk themes show <name>).
    pub fn format_display(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Theme: {}", self.name));
        lines.push("-".repeat(40));

        for type_key in self.themed_types() {
            let label = self.label(type_key);
            let priority = self.default_priority(type_key);
            lines.push(format!(
                "  {type_key:<15} → {label:<15} (default: {priority})"
            ));
        }

        lines.join("\n") + "\n"
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::nautical()
    }
}

/// Parse a TTL theme file into a Theme struct.
///
/// Minimal parser — extracts `rdfs:label`, `kb:defaultPriority` from each type line.
fn parse_theme(name: &str, content: &str) -> Theme {
    let mut labels = HashMap::new();
    let mut defaults = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with('@') || trimmed.is_empty() {
            continue;
        }

        // Extract type key: "kb:Expedition" → "expedition"
        let type_key = if let Some(rest) = trimmed.strip_prefix("kb:") {
            rest.split_whitespace().next().unwrap_or("").to_lowercase()
        } else {
            continue;
        };

        // Extract rdfs:label value
        if let Some(label) = extract_ttl_value(trimmed, "rdfs:label") {
            labels.insert(type_key.clone(), label);
        }

        // Extract kb:defaultPriority value
        if let Some(priority) = extract_ttl_value(trimmed, "kb:defaultPriority") {
            defaults.insert(type_key.clone(), priority);
        }
    }

    Theme {
        name: name.to_string(),
        labels,
        defaults,
    }
}

/// Extract a quoted string value for a predicate from a TTL line.
fn extract_ttl_value(line: &str, predicate: &str) -> Option<String> {
    let pred_pos = line.find(predicate)?;
    let after = &line[pred_pos + predicate.len()..];
    let start = after.find('"')? + 1;
    let rest = &after[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nautical_labels() {
        let theme = Theme::nautical();
        assert_eq!(theme.name, "nautical");
        assert_eq!(theme.label("expedition"), "Expedition");
        assert_eq!(theme.label("voyage"), "Voyage");
        assert_eq!(theme.label("chore"), "Chore");
    }

    #[test]
    fn test_agile_labels() {
        let theme = Theme::agile();
        assert_eq!(theme.label("expedition"), "Story");
        assert_eq!(theme.label("voyage"), "Epic");
        assert_eq!(theme.label("chore"), "Task");
    }

    #[test]
    fn test_standard_labels() {
        let theme = Theme::standard();
        assert_eq!(theme.label("expedition"), "Work Item");
        assert_eq!(theme.label("voyage"), "Project");
    }

    #[test]
    fn test_unknown_type_falls_back_to_key() {
        let theme = Theme::nautical();
        assert_eq!(theme.label("unknown_type"), "unknown_type");
    }

    #[test]
    fn test_default_priority() {
        let theme = Theme::nautical();
        assert_eq!(theme.default_priority("expedition"), "medium");
        assert_eq!(theme.default_priority("hazard"), "high");
        assert_eq!(theme.default_priority("signal"), "low");
        assert_eq!(theme.default_priority("unknown"), "medium"); // fallback
    }

    #[test]
    fn test_load_builtin() {
        assert!(Theme::load("nautical", None).is_some());
        assert!(Theme::load("agile", None).is_some());
        assert!(Theme::load("standard", None).is_some());
        assert!(Theme::load("nonexistent", None).is_none());
    }

    #[test]
    fn test_available_themes_includes_builtins() {
        let themes = Theme::available_themes(None);
        assert!(themes.contains(&"nautical".to_string()));
        assert!(themes.contains(&"agile".to_string()));
        assert!(themes.contains(&"standard".to_string()));
    }

    #[test]
    fn test_project_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = dir.path().join(".yurtle-kanban/themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir");
        std::fs::write(
            themes_dir.join("custom.ttl"),
            "kb:Expedition rdfs:label \"Quest\" ; kb:defaultPriority \"high\" .\n",
        )
        .expect("write");

        let theme = Theme::load("custom", Some(dir.path())).expect("load custom");
        assert_eq!(theme.label("expedition"), "Quest");
        assert_eq!(theme.default_priority("expedition"), "high");
    }

    #[test]
    fn test_format_display() {
        let theme = Theme::nautical();
        let output = theme.format_display();
        assert!(output.contains("Theme: nautical"));
        assert!(output.contains("Expedition"));
        assert!(output.contains("medium"));
    }

    #[test]
    fn test_themed_types() {
        let theme = Theme::nautical();
        let types = theme.themed_types();
        assert!(types.contains(&"expedition"));
        assert!(types.contains(&"voyage"));
        assert_eq!(types.len(), 6); // 6 dev types
    }

    #[test]
    fn test_shapes_unaffected_by_theme() {
        // Shapes use kb:Expedition (URI), not "Story" or "Quest"
        // Theme only affects display labels — the underlying type system is unchanged
        let nautical = Theme::nautical();
        let agile = Theme::agile();

        // Both themes have the same type keys (URIs)
        assert_eq!(nautical.themed_types().len(), agile.themed_types().len());

        // Labels differ but keys are identical
        assert_eq!(nautical.label("expedition"), "Expedition");
        assert_eq!(agile.label("expedition"), "Story");
        // The key "expedition" is the same in both — shapes reference this, not the label
    }
}
