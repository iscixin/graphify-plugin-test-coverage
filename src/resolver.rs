//! 反轉解析器：將 AST node range → 行級覆蓋率統計。
//!
//! 與 review plugin 的「行號 → 單一最內層節點」解析器相反，
//! coverage 需要的是「給定 node range，算出涵蓋行中有多少 hit」。
//!
//! ## 演算法
//! 1. 對每個 GraphOutput node，依 `file_matches` 找到對應的 coverage 檔案
//! 2. 統計 `[start_line, end_line]` 範圍內有 coverage data 的行數與 hit 數
//! 3. 產出 `CoverageBinding`（含 line_rate 與 blindspot 判定）
//! 4. 檔案級 fallback：不在任何 node 範圍內的殘餘行，合併為 `canonical_node_id = ''`

use std::collections::{HashMap, HashSet};

use graphify_core::types::{GraphOutput, Node};

use crate::ingest::CoverageData;
use crate::registry::CoverageBinding;

/// 盲區閾值：line_rate < 0.5 視為 blindspot（固定，暫不支援 env 覆寫）。
const BLINDSPOT_THRESHOLD: f64 = 0.5;

/// 將覆蓋率資料升維對齊至 AST 節點，產出 coverage_bindings。
///
/// 輸入：
/// - `graph`：Graphify Core 的 AST 圖譜（含 node range）。
/// - `coverage`：LCOV/JSON 解析後的線級覆蓋率資料。
/// - `workspace_key`：當前 workspace 的 routing key。
///
/// 回傳：
/// - `Vec<CoverageBinding>`：每筆代表一個節點（或檔案級殘餘行）的覆蓋率快照。
///
/// 保證：
/// - 入參順序不影響結果（輸出不保證排序）。
/// - 無 coverage data 交集的節點不產出 binding。
#[must_use]
pub fn resolve_coverage(
    graph: &GraphOutput,
    coverage: &CoverageData,
    workspace_key: &str,
) -> Vec<CoverageBinding> {
    // 1. 將每個 coverage file 對應到其 graph nodes
    let file_to_nodes = group_nodes_by_coverage_file(&graph.nodes, &coverage.files);
    // 2. 逐 node 計算覆蓋率
    let mut bindings = resolve_nodes(&graph.nodes, &file_to_nodes, &coverage.files, workspace_key);
    // 3. 檔案級殘餘行 fallback
    let file_level = resolve_file_level(&graph.nodes, &file_to_nodes, &coverage.files, workspace_key);
    bindings.extend(file_level);
    bindings
}

/// 將 graph nodes 按 coverage file path 分組。
///
/// 使用 `file_matches` 容忍路由前綴差異（與 review resolver 同款）。
fn group_nodes_by_coverage_file<'a>(
    nodes: &'a [Node],
    coverage_files: &'a HashMap<String, HashMap<u32, u64>>,
) -> HashMap<&'a str, Vec<&'a Node>> {
    let mut map: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes {
        for coverage_file in coverage_files.keys() {
            if file_matches(&node.source_file, coverage_file) {
                map.entry(coverage_file.as_str()).or_default().push(node);
                break;
            }
        }
    }
    map
}

/// 對每個在 coverage 中有對應的 node，計算覆蓋率統計。
fn resolve_nodes(
    _nodes: &[Node],
    file_to_nodes: &HashMap<&str, Vec<&Node>>,
    coverage_files: &HashMap<String, HashMap<u32, u64>>,
    workspace_key: &str,
) -> Vec<CoverageBinding> {
    let mut bindings = Vec::new();
    // 用 HashSet 追蹤已處理過的 node（避免同 node 出現在多個 coverage file 下的重複）
    let mut seen: HashSet<&str> = HashSet::new();

    for (coverage_file, matched_nodes) in file_to_nodes {
        let some_lines = match coverage_files.get(*coverage_file) {
            Some(l) => l,
            None => continue,
        };
        for node in matched_nodes {
            if !seen.insert(node.id.0.as_str()) {
                continue;
            }
            let mut covered = 0u64;
            let mut uncovered = 0u64;
            for line in node.start_line..=node.end_line {
                let line = line as u32;
                if let Some(hit) = some_lines.get(&line) {
                    if *hit > 0 {
                        covered += 1;
                    } else {
                        uncovered += 1;
                    }
                }
            }
            let total = covered + uncovered;
            if total == 0 {
                continue;
            }
            let line_rate = covered as f64 / total as f64;
            bindings.push(CoverageBinding {
                workspace_key: workspace_key.to_string(),
                canonical_node_id: node.id.0.clone(),
                file_path: coverage_file.to_string(),
                total_lines: total as i64,
                covered_lines: covered as i64,
                line_rate,
                is_blindspot: line_rate < BLINDSPOT_THRESHOLD,
                updated_at: String::new(), // 由 caller 填入
            });
        }
    }
    bindings
}

/// 計算檔案級覆蓋率：不在任何 AST node 範圍內的殘餘行。
fn resolve_file_level(
    _nodes: &[Node],
    file_to_nodes: &HashMap<&str, Vec<&Node>>,
    coverage_files: &HashMap<String, HashMap<u32, u64>>,
    workspace_key: &str,
) -> Vec<CoverageBinding> {
    let mut bindings = Vec::new();

    for (coverage_file, matched_nodes) in file_to_nodes {
        let some_lines = match coverage_files.get(*coverage_file) {
            Some(l) => l,
            None => continue,
        };

        // 收集該檔案所有被 node 覆蓋的行
        let mut covered_by_node: HashSet<u32> = HashSet::new();
        for node in matched_nodes {
            for line in node.start_line..=node.end_line {
                if some_lines.contains_key(&(line as u32)) {
                    covered_by_node.insert(line as u32);
                }
            }
        }

        // 殘餘行 = 有 coverage data 但不在任何 node 範圍內
        let residual: Vec<u32> = some_lines
            .keys()
            .filter(|line| !covered_by_node.contains(line))
            .copied()
            .collect();

        if residual.is_empty() {
            continue;
        }

        let mut covered = 0u64;
        let mut uncovered = 0u64;
        for line in &residual {
            if let Some(hit) = some_lines.get(line) {
                if *hit > 0 {
                    covered += 1;
                } else {
                    uncovered += 1;
                }
            }
        }
        let total = covered + uncovered;
        let line_rate = if total > 0 {
            covered as f64 / total as f64
        } else {
            0.0
        };
        bindings.push(CoverageBinding {
            workspace_key: workspace_key.to_string(),
            canonical_node_id: format!("file:{coverage_file}"),
            file_path: coverage_file.to_string(),
            total_lines: total as i64,
            covered_lines: covered as i64,
            line_rate,
            is_blindspot: line_rate < BLINDSPOT_THRESHOLD,
            updated_at: String::new(),
        });
    }
    bindings
}

/// `node_path` 是否代表 `want`（workspace-root 相對路徑）。
/// 精確相等，或兩者都以 `/` 分隔且 node_path 以 `want` 結尾，
/// 或 want 以 node_path 結尾（反向匹配，處理 lcov 給絕對路徑而 graph 給相對路徑的情況）。
#[must_use]
pub fn file_matches(node_path: &str, want: &str) -> bool {
    if node_path == want {
        return true;
    }
    if want.is_empty() {
        return false;
    }

    // 正向：node_path 以 want 結尾（graph 路徑長，coverage 路徑短）
    if want.contains('/') {
        let n = node_path.strip_suffix(want);
        if let Some(prefix) = n {
            return prefix.is_empty() || prefix.ends_with('/');
        }
    }

    // 反向：want 以 node_path 結尾（coverage 路徑長，graph 路徑短）
    // 去掉 graph 路徑的 `./` 前綴再比對
    let clean_path = node_path.strip_prefix("./").unwrap_or(node_path);
    if clean_path.contains('/') {
        let n = want.strip_suffix(clean_path);
        if let Some(prefix) = n {
            return prefix.is_empty() || prefix.ends_with('/');
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphify_core::types::{FileType, Node, NodeId};

    fn node(id: &str, source_file: &str, start: usize, end: usize) -> Node {
        Node {
            id: NodeId(id.to_string()),
            label: id.rsplit(':').next().unwrap_or(id).to_string(),
            file_type: FileType::Code,
            kind: "function".to_string(),
            language: "rust".to_string(),
            source_file: source_file.to_string(),
            start_line: start,
            end_line: end,
            doc_comment: None,
            description: None,
            metadata: None,
        }
    }

    fn graph_with(nodes: Vec<Node>) -> GraphOutput {
        GraphOutput { nodes, edges: Vec::new(), metadata: Default::default() }
    }

    fn coverage(files: Vec<(&str, Vec<(u32, u64)>)>) -> CoverageData {
        let mut map = HashMap::new();
        for (file, lines) in files {
            let mut lm = HashMap::new();
            for (line, hit) in lines {
                lm.insert(line, hit);
            }
            map.insert(file.to_string(), lm);
        }
        CoverageData { files: map }
    }

    #[test]
    fn node_with_full_coverage() {
        let g = graph_with(vec![
            node("src/auth.rs:function:verify", "src/auth.rs", 30, 42),
        ]);
        let c = coverage(vec![
            ("src/auth.rs", vec![(30, 1), (31, 5), (35, 0), (42, 3)]),
        ]);
        let bindings = resolve_coverage(&g, &c, "w-1");
        assert_eq!(bindings.len(), 1);
        let b = &bindings[0];
        assert_eq!(b.canonical_node_id, "src/auth.rs:function:verify");
        // 4 lines in range [30,42] have coverage data: 30,31,35,42
        // covered: 30(1),31(5),42(3) = 3; uncovered: 35(0) = 1
        assert_eq!(b.total_lines, 4);
        assert_eq!(b.covered_lines, 3);
        assert!((b.line_rate - 0.75).abs() < 0.001);
        assert!(!b.is_blindspot);
    }

    #[test]
    fn node_with_zero_coverage() {
        let g = graph_with(vec![
            node("src/auth.rs:function:blind", "src/auth.rs", 1, 5),
        ]);
        let c = coverage(vec![
            ("src/auth.rs", vec![(1, 0), (2, 0), (3, 0)]),
        ]);
        let bindings = resolve_coverage(&g, &c, "w-1");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].covered_lines, 0);
        assert!((bindings[0].line_rate - 0.0).abs() < 0.001);
        assert!(bindings[0].is_blindspot);
    }

    #[test]
    fn node_with_no_coverage_data_returns_no_binding() {
        let g = graph_with(vec![
            node("src/auth.rs:function:f", "src/auth.rs", 1, 100),
        ]);
        // coverage data 只有另一隻檔案
        let c = coverage(vec![("src/other.rs", vec![(1, 1)])]);
        let bindings = resolve_coverage(&g, &c, "w-1");
        assert!(bindings.is_empty());
    }

    #[test]
    fn multiple_nodes_in_same_file() {
        let g = graph_with(vec![
            node("src/a.rs:function:f1", "src/a.rs", 1, 10),
            node("src/a.rs:function:f2", "src/a.rs", 20, 30),
        ]);
        let c = coverage(vec![
            ("src/a.rs", vec![(1, 1), (5, 0), (20, 3), (25, 0)]),
        ]);
        let bindings = resolve_coverage(&g, &c, "w-1");
        assert_eq!(bindings.len(), 2);
        let f1 = bindings.iter().find(|b| b.canonical_node_id == "src/a.rs:function:f1").unwrap();
        assert_eq!(f1.total_lines, 2); // lines 1,5
        assert_eq!(f1.covered_lines, 1);
        let f2 = bindings.iter().find(|b| b.canonical_node_id == "src/a.rs:function:f2").unwrap();
        assert_eq!(f2.total_lines, 2); // lines 20,25
        assert_eq!(f2.covered_lines, 1);
    }

    #[test]
    fn file_level_residual_lines() {
        let g = graph_with(vec![
            node("src/a.rs:function:f", "src/a.rs", 10, 20),
        ]);
        let c = coverage(vec![
            ("src/a.rs", vec![(1, 1), (5, 0), (10, 1), (15, 0), (30, 3)]),
        ]);
        // Node 涵蓋 line 10,15；殘餘行為 1,5,30
        let bindings = resolve_coverage(&g, &c, "w-1");
        let file_bindings: Vec<_> = bindings.iter().filter(|b| b.canonical_node_id.starts_with("file:")).collect();
        assert_eq!(file_bindings.len(), 1, "file-level entry");
        let fb = &file_bindings[0];
        assert_eq!(fb.canonical_node_id, "file:src/a.rs");
        assert_eq!(fb.total_lines, 3); // lines 1,5,30
        assert_eq!(fb.covered_lines, 2); // lines 1,30 are covered
        assert!((fb.line_rate - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn suffix_path_matches() {
        let g = graph_with(vec![
            node("f:function:f", "/repo/src/a.rs", 1, 5),
        ]);
        let c = coverage(vec![
            ("src/a.rs", vec![(1, 1), (3, 0)]),
        ]);
        let bindings = resolve_coverage(&g, &c, "w-1");
        assert_eq!(bindings.len(), 1, "suffix match succeeded");
        assert_eq!(bindings[0].total_lines, 2);
    }

    #[test]
    fn file_matches_rules() {
        assert!(file_matches("src/auth.rs", "src/auth.rs"));
        assert!(file_matches("/repo/src/auth.rs", "src/auth.rs"));
        assert!(file_matches("src/auth/verify.rs", "auth/verify.rs"));
        assert!(!file_matches("src/auth.rs", "auth.rs")); // 純檔名不做 suffix
        assert!(!file_matches("src/foosrc/auth.rs", "src/auth.rs"));
        assert!(file_matches("src/foosrc/auth.rs", "src/foosrc/auth.rs"));

        // 反向匹配：coverage 給絕對路徑，graph 給相對路徑（含 ./ 前綴）
        assert!(file_matches(
            "./graphify-core/src/types.rs",
            "/home/user/project/graphify-core/src/types.rs"
        ));
        // 反向匹配：graph 相對路徑不帶 ./ 前綴
        assert!(file_matches(
            "graphify-core/src/types.rs",
            "/home/user/project/graphify-core/src/types.rs"
        ));
        // 反向匹配不應配對檔名級別
        assert!(!file_matches("src/auth.rs", "/repo/other/auth.rs"));
    }
}