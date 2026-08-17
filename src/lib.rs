//! graphify-plugin-test-coverage — 純 bridge（測試覆蓋率資料源 → Graphify
//! canonical AST node id 升維綁定）。
//!
//! 不重造 Coverage 引擎：覆蓋率資料 100% 來自外部工具落盤的 LCOV/JSON；
//! 本 plugin 負責「行級覆蓋率 → canonical node 覆蓋率統計」升維、
//! coverage_bindings 持久化（併入 graphify.db）、與 .toon 盲區合成。

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use graphify_core::plugin::{GraphUpdateEvent, GraphifyPlugin, WorkspaceContext};
use graphify_core::types::GraphOutput;

use crate::ingest::{CoverageParseError, parse_json, parse_lcov};
use crate::registry::CoverageDb;
use crate::resolver::resolve_coverage;
use crate::sync::{build_coverage_plugin_data, emit_packet, parse_graph};

pub mod ingest;
pub mod registry;
pub mod resolver;
pub mod sync;

/// plugin 唯一識別（graphify-mcp auto-register 的 id 前綴）。
pub const PLUGIN_ID: &str = "graphify-plugin-test-coverage";

/// plugin 狀態。
pub struct CoveragePlugin {
    workspace_key: String,
    /// workspace 根目錄（`WorkspaceContext.root_path`）。
    root_path: String,
    /// 覆寫 graphify.db 路徑（測試注入用）；`None` = 預設 XDG 路徑。
    registry_path: Option<PathBuf>,
    /// 記憶體 GraphOutput 快取（sync_toon 填入；resolver 使用）。
    graph_cache: RwLock<Option<GraphOutput>>,
}

impl std::fmt::Debug for CoveragePlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoveragePlugin")
            .field("workspace_key", &self.workspace_key)
            .field("registry_path", &self.registry_path)
            .field("graph_cache", &self.graph_cache)
            .finish()
    }
}

impl Default for CoveragePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CoveragePlugin {
    /// 預設建構；以 [`graphify_registry::registry_db_path`] 為 db 路徑。
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace_key: String::new(),
            root_path: String::new(),
            registry_path: None,
            graph_cache: RwLock::new(None),
        }
    }

    /// 覆寫 registry db 路徑（測試注入）。
    #[must_use]
    pub fn with_registry_path(mut self, path: PathBuf) -> Self {
        self.registry_path = Some(path);
        self
    }

    /// 以 `cwd` 合成 `WorkspaceContext` 並 bind（CLI 整合模式）。
    #[must_use]
    pub fn bind_for_cli(mut self, cwd: impl AsRef<Path>) -> Self {
        let cwd_ref = cwd.as_ref();
        let workspace_key = graphify_core::plugin::derive_workspace_key(cwd_ref);
        let name = cwd_ref
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".to_string());
        let ctx = WorkspaceContext::new(
            workspace_key,
            name,
            cwd_ref.to_string_lossy().into_owned(),
        );
        self.bind(ctx);
        self
    }

    fn registry_path(&self) -> PathBuf {
        self.registry_path
            .clone()
            .unwrap_or_else(graphify_registry::registry_db_path)
    }

    pub fn db(&self) -> Result<CoverageDb, rusqlite::Error> {
        CoverageDb::open(&self.registry_path())
    }

    /// 目前快取的 GraphOutput（無則 `None`）。
    #[must_use]
    pub fn graph(&self) -> Option<GraphOutput> {
        self.graph_cache.read().ok()?.clone()
    }

    /// 解析 LCOV 文字並將覆蓋率資料升維綁定至 graph 節點，
    /// 以快照取代模式寫入 coverage_bindings。
    ///
    /// # Errors
    /// - `CoverageError::NotBound`：尚未呼叫 bind。
    /// - `CoverageError::NoGraphCache`：尚未收到 graph（sync_toon 尚未被呼叫）。
    /// - `CoverageError::Parse`：LCOV 格式錯誤。
    /// - `CoverageError::Db`：SQLite 操作失敗。
    pub fn coverage_ingest_lcov(&self, lcov_text: &str) -> Result<CoverageSummary, CoverageError> {
        let data = parse_lcov(lcov_text)?;
        self.ingest_inner(&data)
    }

    /// 解析 JSON IngestPayload 文字並將覆蓋率資料升維綁定至 graph 節點，
    /// 以快照取代模式寫入 coverage_bindings。
    ///
    /// # Errors
    /// 同 [`coverage_ingest_lcov`]。
    pub fn coverage_ingest_json(&self, json_text: &str) -> Result<CoverageSummary, CoverageError> {
        let data = parse_json(json_text)?;
        self.ingest_inner(&data)
    }

    /// 內部共用的 ingest 邏輯。
    fn ingest_inner(&self, data: &ingest::CoverageData) -> Result<CoverageSummary, CoverageError> {
        if self.workspace_key.is_empty() {
            return Err(CoverageError::NotBound);
        }
        let graph = self
            .graph_cache
            .read()
            .map_err(|_| CoverageError::Internal("graph_cache lock poisoned".to_string()))?
            .clone()
            .ok_or(CoverageError::NoGraphCache)?;

        let now = now_rfc3339();
        let mut bindings = resolve_coverage(&graph, data, &self.workspace_key);

        // 填入 updated_at
        for b in &mut bindings {
            b.updated_at = now.clone();
        }

        let db = self.db()?;
        db.snapshot_replace(&self.workspace_key, &bindings, &now)?;

        let total_lines: i64 = bindings.iter().map(|b| b.total_lines).sum();
        let covered_lines: i64 = bindings.iter().map(|b| b.covered_lines).sum();
        let blindspots = bindings.iter().filter(|b| b.is_blindspot).count();
        let bound_nodes = bindings.iter().filter(|b| !b.canonical_node_id.is_empty()).count();

        Ok(CoverageSummary {
            bound_nodes,
            total_lines,
            covered_lines,
            blindspots,
        })
    }
}

impl GraphifyPlugin for CoveragePlugin {
    fn get_id(&self) -> &str {
        PLUGIN_ID
    }

    fn bind(&mut self, ctx: WorkspaceContext) {
        self.workspace_key = ctx.workspace_key;
        self.root_path = ctx.root_path;
    }

    fn get_workspace_key(&self) -> &str {
        &self.workspace_key
    }

    fn sync_toon(&mut self, opt_toon: Option<Vec<u8>>) -> Vec<u8> {
        match opt_toon {
            Some(toon_bytes) => {
                // 被動 sync：快取 graph
                if let Some(graph) = parse_graph(&toon_bytes) {
                    *self.graph_cache.write().unwrap() = Some(graph);
                }
                let summary = coverage_summary_json(&self.workspace_key, &self.db().ok());
                emit_packet(&self.workspace_key, &summary).into_bytes()
            }
            None => {
                let summary = coverage_summary_json(&self.workspace_key, &self.db().ok());
                emit_packet(&self.workspace_key, &summary).into_bytes()
            }
        }
    }

    fn on_graph_updated(&mut self, _event: &GraphUpdateEvent) {
        // coverage 是快照取代式，不需 drift 自動解決
    }
}

/// 從 DB 查詢並合成 plugin_data 摘要 JSON。
fn coverage_summary_json(
    workspace_key: &str,
    db: &Option<CoverageDb>,
) -> serde_json::Value {
    match db {
        Some(db) => {
            let total = db.count(workspace_key).unwrap_or(0);
            let spots = db.count_blindspots(workspace_key).unwrap_or(0);
            let avg = db.avg_line_rate(workspace_key).unwrap_or(0.0);
            build_coverage_plugin_data(workspace_key, total, spots, avg)
        }
        None => {
            build_coverage_plugin_data(workspace_key, 0, 0, 0.0)
        }
    }
}

/// 覆蓋率 ingest 結果摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSummary {
    /// 綁定到 symbol 的節點數（排除檔案級）。
    pub bound_nodes: usize,
    /// 所有 binding 的總行數。
    pub total_lines: i64,
    /// 所有 binding 的已覆蓋行數。
    pub covered_lines: i64,
    /// 盲區數（line_rate < 0.5）。
    pub blindspots: usize,
}

/// plugin 錯誤。
#[derive(Debug, thiserror::Error)]
pub enum CoverageError {
    #[error("parse error: {0}")]
    Parse(#[from] CoverageParseError),
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("no graph cache — call sync_toon first")]
    NoGraphCache,
    #[error("plugin not bound — call bind first")]
    NotBound,
    #[error("internal error: {0}")]
    Internal(String),
}

/// RFC 3339 時間戳字串（UTC）。
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(i64::try_from(days).unwrap_or(0));
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Civil 日期轉換（Howard Hinnant 演算法，與 review plugin 同款）。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphify_core::to_toon;
    use graphify_core::types::{FileType, Node, NodeId};

    fn make_graph() -> GraphOutput {
        GraphOutput {
            nodes: vec![
                Node {
                    id: NodeId("src/a.rs:function:f".to_string()),
                    label: "f".to_string(),
                    file_type: FileType::Code,
                    kind: "function".to_string(),
                    language: "rust".to_string(),
                    source_file: "src/a.rs".to_string(),
                    start_line: 1,
                    end_line: 10,
                    doc_comment: None,
                    description: None,
                    metadata: None,
                },
            ],
            edges: Vec::new(),
            metadata: Default::default(),
        }
    }

    fn plugin_with_tmp_db() -> (tempfile::TempDir, CoveragePlugin) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graphify.db");
        let p = CoveragePlugin::new()
            .with_registry_path(db_path);
        (dir, p)
    }

    fn inject_graph(p: &mut CoveragePlugin) {
        let toon = to_toon(&make_graph());
        p.sync_toon(Some(toon.into_bytes()));
    }

    #[test]
    fn bind_sets_workspace_key() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        assert_eq!(p.get_workspace_key(), "w-1");
    }

    #[test]
    fn not_bound_returns_error() {
        let (_d, p) = plugin_with_tmp_db();
        let result = p.coverage_ingest_lcov("SF:a.rs\nDA:1,1\nend_of_record\n");
        assert!(matches!(result, Err(CoverageError::NotBound)));
    }

    #[test]
    fn no_graph_cache_returns_error() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        let result = p.coverage_ingest_lcov("SF:a.rs\nDA:1,1\nend_of_record\n");
        assert!(matches!(result, Err(CoverageError::NoGraphCache)));
    }

    #[test]
    fn full_ingest_lcov_flow() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        inject_graph(&mut p);
        assert!(p.graph().is_some());

        let lcov = "SF:src/a.rs\nDA:1,1\nDA:5,0\nDA:10,3\nend_of_record\n";
        let summary = p.coverage_ingest_lcov(lcov).unwrap();
        assert_eq!(summary.bound_nodes, 1);
        assert_eq!(summary.total_lines, 3);
        assert_eq!(summary.covered_lines, 2);
        assert_eq!(summary.blindspots, 0);

        let db = p.db().unwrap();
        let binding = db.query_by_node("w-1", "src/a.rs:function:f").unwrap().unwrap();
        assert!((binding.line_rate - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn full_ingest_json_flow() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        inject_graph(&mut p);

        let json = r#"{"version":"1.0","source":"cobertura","files":[{"file_path":"src/a.rs","lines":[{"line_number":1,"hit_count":0},{"line_number":3,"hit_count":0}]}]}"#;
        let summary = p.coverage_ingest_json(json).unwrap();
        assert_eq!(summary.bound_nodes, 1);
        assert_eq!(summary.blindspots, 1);

        let db = p.db().unwrap();
        let spots = db.query_blindspots("w-1").unwrap();
        assert_eq!(spots.len(), 1);
    }

    #[test]
    fn sync_toon_none_does_not_panic() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        let out = p.sync_toon(None);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("workspace_key"));
        assert!(text.contains("coverage"));
    }

    #[test]
    fn sync_toon_garbage_is_lenient() {
        let (_d, p) = plugin_with_tmp_db();
        let mut p = p;
        p.bind(WorkspaceContext::new("w-1", "ws", "/tmp/ws"));
        let out = p.sync_toon(Some(b"not-a-toon".to_vec()));
        assert!(String::from_utf8_lossy(&out).contains("workspace_key"));
        assert!(p.graph().is_none(), "garbage toon should not cache graph");
    }

    #[test]
    fn bind_for_cli_sets_workspace_key() {
        let (_d, p) = plugin_with_tmp_db();
        let p = p.bind_for_cli("/tmp/workspace");
        assert!(!p.get_workspace_key().is_empty());
    }
}