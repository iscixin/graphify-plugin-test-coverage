//! .toon 封包協定（與 review plugin 同款 text-based 格式）。
//!
//! - `from_toon` 解析.graph 封包為 GraphOutput
//! - `emit_packet` 產出 metadata + plugin_data 封包

use graphify_core::from_toon;
use graphify_core::types::GraphOutput;

/// 封包契約版本。
pub const FORMAT_VERSION: &str = "1.0.0";

/// 從 .toon 位元組解析並回傳 GraphOutput（None = 解析失敗）。
#[must_use]
pub fn parse_graph(toon_bytes: &[u8]) -> Option<GraphOutput> {
    let raw = String::from_utf8_lossy(toon_bytes);
    let g = from_toon(&raw).ok()?;
    if g.nodes.is_empty() && g.edges.is_empty() {
        return None;
    }
    Some(g)
}

/// TOON 字串轉義（與 review plugin `sync.rs` 同款）。
fn escape_string(s: &str) -> String {
    let needs_quoting = s.is_empty()
        || s == "null"
        || s == "true"
        || s == "false"
        || s.chars().any(|c| {
            c.is_whitespace()
                || c == ':'
                || c == '['
                || c == ']'
                || c == '{'
                || c == '}'
                || c == '-'
                || c == '\\'
                || c == '"'
        })
        || s.starts_with('-')
        || s.chars().next().is_some_and(|c| c.is_ascii_digit());

    if needs_quoting {
        let mut escaped = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                _ => escaped.push(c),
            }
        }
        escaped.push('"');
        escaped
    } else {
        s.to_string()
    }
}

/// 產出承載封包：metadata（format_version + workspace_key）+ plugin_data。
#[must_use]
pub fn emit_packet(workspace_key: &str, plugin_data: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str("metadata:\n");
    out.push_str(&format!("  format_version: {}\n", escape_string(FORMAT_VERSION)));
    out.push_str(&format!("  workspace_key: {}\n", escape_string(workspace_key)));
    out.push_str(&format!("  plugin_data: {}\n", escape_string(&plugin_data.to_string())));
    out
}

/// 合成 plugin_data JSON（盲區摘要）。
#[must_use]
pub fn build_coverage_plugin_data(
    workspace_key: &str,
    total_nodes: usize,
    blindspots: usize,
    avg_coverage: f64,
) -> serde_json::Value {
    serde_json::json!({
        "coverage": {
            "workspace_key": workspace_key,
            "total_nodes": total_nodes,
            "blindspots": blindspots,
            "avg_coverage": (avg_coverage * 10.0).round() / 10.0,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphify_core::types::{FileType, Node, NodeId};
    use graphify_core::to_toon;

    fn node(id: &str, file: &str, start: usize, end: usize) -> Node {
        Node {
            id: NodeId(id.to_string()),
            label: id.rsplit(':').next().unwrap_or(id).to_string(),
            file_type: FileType::Code,
            kind: "function".to_string(),
            language: "rust".to_string(),
            source_file: file.to_string(),
            start_line: start,
            end_line: end,
            doc_comment: None,
            description: None,
            metadata: None,
        }
    }

    #[test]
    fn parse_graph_from_valid_toon() {
        let graph = GraphOutput {
            nodes: vec![node("src/a.rs:function:f", "src/a.rs", 1, 10)],
            edges: Vec::new(),
            metadata: Default::default(),
        };
        let toon = to_toon(&graph);
        let parsed = parse_graph(toon.as_bytes()).unwrap();
        assert_eq!(parsed.nodes.len(), 1);
        assert_eq!(parsed.nodes[0].id.0, "src/a.rs:function:f");
    }

    #[test]
    fn parse_graph_empty_returns_none() {
        assert!(parse_graph(b"").is_none());
        assert!(parse_graph(b"metadata:\n  format_version: \"1.0.0\"\n").is_none());
    }

    #[test]
    fn emit_and_reparse() {
        let data = build_coverage_plugin_data("w-abc", 10, 2, 73.45);
        let packet = emit_packet("w-abc", &data);
        assert!(packet.contains("workspace_key"));
        assert!(packet.contains("coverage"));
    }

    #[test]
    fn build_coverage_plugin_data_rounds_avg() {
        let data = build_coverage_plugin_data("w-1", 10, 2, 73.45);
        let cov = &data["coverage"];
        assert_eq!(cov["workspace_key"], "w-1");
        assert_eq!(cov["total_nodes"], 10);
        assert_eq!(cov["blindspots"], 2);
        let avg = cov["avg_coverage"].as_f64().unwrap();
        assert!((avg - 73.5).abs() < 0.01, "expected 73.5, got {avg}");
    }
}