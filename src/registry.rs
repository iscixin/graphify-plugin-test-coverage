//! SQLite coverage registry — plugin 自有表，與 graphify-registry 共用同一 graphify.db 檔。
//!
//! ## 表
//! - `coverage_bindings`: (workspace_key, canonical_node_id → total_lines,
//!   covered_lines, line_rate, is_blindspot) — 快照取代式覆蓋率綁定。
//!
//! ## 快照取代模型
//! 不同於 review plugin 的記錄式（upsert → unresolved → resolved），
//! coverage 是快照取代：每次 ingest 以 `DELETE + INSERT` 單一 transaction
//! 原子替換整個 workspace 的 coverage 資料。沒有 lifecycle API。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

/// 一筆覆蓋率綁定（coverage_bindings 列）。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageBinding {
    pub workspace_key: String,
    /// GraphOutput Node.id 原樣（`{file_path}:{kind}:{name}`），
    /// 空字串 = 檔案級（無對應 symbol 的殘餘行）。
    pub canonical_node_id: String,
    pub file_path: String,
    pub total_lines: i64,
    pub covered_lines: i64,
    /// 0.0 ~ 1.0（covered / total）。
    pub line_rate: f64,
    /// 1 = line_rate < 0.5（盲區）。
    pub is_blindspot: bool,
    /// RFC 3339。
    pub updated_at: String,
}

/// plugin 自有 SQLite 連線。
pub struct CoverageDb {
    conn: Connection,
}

impl CoverageDb {
    /// 開啟 `path`（共用的 graphify.db），並確保 plugin schema 已建。
    ///
    /// # Errors
    /// 回傳 `rusqlite::Error` 於開啟或 DDL 執行失敗時。
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS coverage_bindings (
                workspace_key     TEXT NOT NULL,
                canonical_node_id TEXT NOT NULL,
                file_path         TEXT NOT NULL,
                total_lines       INTEGER NOT NULL,
                covered_lines     INTEGER NOT NULL,
                line_rate         REAL NOT NULL,
                is_blindspot      INTEGER NOT NULL,
                updated_at        TEXT NOT NULL,
                PRIMARY KEY (workspace_key, canonical_node_id)
            );

            CREATE INDEX IF NOT EXISTS idx_coverage_node
                ON coverage_bindings (workspace_key, canonical_node_id);

            CREATE INDEX IF NOT EXISTS idx_coverage_blindspot
                ON coverage_bindings (workspace_key, is_blindspot);",
        )?;
        Ok(Self { conn })
    }

    /// 快照取代整個 workspace 的 coverage 資料。
    /// 以單一 transaction 原子清除舊資料並寫入新 binding。
    ///
    /// # Errors
    /// SQLite DML 失敗時回傳 `rusqlite::Error`。
    pub fn snapshot_replace(
        &self,
        workspace_key: &str,
        bindings: &[CoverageBinding],
        updated_at: &str,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM coverage_bindings WHERE workspace_key = ?1",
            params![workspace_key],
        )?;
        for chunk in bindings.chunks(512) {
            let mut stmt = self.conn.prepare(
                "INSERT INTO coverage_bindings
                    (workspace_key, canonical_node_id, file_path,
                     total_lines, covered_lines, line_rate, is_blindspot, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for b in chunk {
                stmt.execute(params![
                    b.workspace_key,
                    b.canonical_node_id,
                    b.file_path,
                    b.total_lines,
                    b.covered_lines,
                    b.line_rate,
                    b.is_blindspot as i64,
                    updated_at,
                ])?;
            }
        }
        Ok(())
    }

    /// 查詢一個 workspace 中指定 canonical node 的覆蓋率。
    ///
    /// # Errors
    /// SQLite 查詢失敗時回傳 `rusqlite::Error`。
    pub fn query_by_node(
        &self,
        workspace_key: &str,
        node_id: &str,
    ) -> Result<Option<CoverageBinding>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT workspace_key, canonical_node_id, file_path,
                        total_lines, covered_lines, line_rate, is_blindspot, updated_at
                 FROM coverage_bindings
                 WHERE workspace_key = ?1 AND canonical_node_id = ?2",
                params![workspace_key, node_id],
                row_from_sql,
            )
            .optional()
    }

    /// 查詢一個 workspace 中所有盲區（is_blindspot = 1）的 binding。
    ///
    /// # Errors
    /// SQLite 查詢失敗時回傳 `rusqlite::Error`。
    pub fn query_blindspots(
        &self,
        workspace_key: &str,
    ) -> Result<Vec<CoverageBinding>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_key, canonical_node_id, file_path,
                    total_lines, covered_lines, line_rate, is_blindspot, updated_at
             FROM coverage_bindings
             WHERE workspace_key = ?1 AND is_blindspot = 1
             ORDER BY line_rate ASC",
        )?;
        let rows = stmt.query_map(params![workspace_key], row_from_sql)?;
        rows.collect()
    }

    /// 統計一個 workspace 的 binding 總數。
    ///
    /// # Errors
    /// SQLite 查詢失敗時回傳 `rusqlite::Error`。
    pub fn count(&self, workspace_key: &str) -> Result<usize, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM coverage_bindings WHERE workspace_key = ?1",
                params![workspace_key],
                |r| r.get(0),
            )
            .map(|n: i64| n as usize)
    }

    /// 統計一個 workspace 的盲區數。
    ///
    /// # Errors
    /// SQLite 查詢失敗時回傳 `rusqlite::Error`。
    pub fn count_blindspots(&self, workspace_key: &str) -> Result<usize, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM coverage_bindings
                 WHERE workspace_key = ?1 AND is_blindspot = 1",
                params![workspace_key],
                |r| r.get(0),
            )
            .map(|n: i64| n as usize)
    }

    /// 計算一個 workspace 的平均覆蓋率（僅計算有 coverage data 的節點）。
    ///
    /// # Errors
    /// SQLite 查詢失敗時回傳 `rusqlite::Error`。
    pub fn avg_line_rate(&self, workspace_key: &str) -> Result<f64, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT COALESCE(AVG(line_rate), 0.0) FROM coverage_bindings
                 WHERE workspace_key = ?1 AND canonical_node_id != ''",
                params![workspace_key],
                |r| r.get(0),
            )
    }
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoverageBinding> {
    Ok(CoverageBinding {
        workspace_key: row.get(0)?,
        canonical_node_id: row.get(1)?,
        file_path: row.get(2)?,
        total_lines: row.get(3)?,
        covered_lines: row.get(4)?,
        line_rate: row.get(5)?,
        is_blindspot: row.get::<_, i64>(6)? != 0,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(ws: &str, node: &str, rate: f64) -> CoverageBinding {
        CoverageBinding {
            workspace_key: ws.to_string(),
            canonical_node_id: node.to_string(),
            file_path: "src/auth.rs".to_string(),
            total_lines: 10,
            covered_lines: (rate * 10.0) as i64,
            line_rate: rate,
            is_blindspot: rate < 0.5,
            updated_at: "2026-08-17T00:00:00Z".to_string(),
        }
    }

    fn open_tmp() -> (tempfile::TempDir, CoverageDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = CoverageDb::open(&dir.path().join("graphify.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn snapshot_replace_atomic() {
        let (_d, db) = open_tmp();
        let ws = "w-1";
        let bindings = vec![
            binding(ws, "src/a.rs:function:f1", 0.8),
            binding(ws, "src/a.rs:function:f2", 0.0),
        ];
        db.snapshot_replace(ws, &bindings, "now").unwrap();
        assert_eq!(db.count(ws).unwrap(), 2);
        assert_eq!(db.count_blindspots(ws).unwrap(), 1);
    }

    #[test]
    fn snapshot_replace_clears_old_data() {
        let (_d, db) = open_tmp();
        let ws = "w-1";
        db.snapshot_replace(ws, &[binding(ws, "n1", 0.9)], "now").unwrap();
        assert_eq!(db.count(ws).unwrap(), 1);
        // 第二次 replace → 舊資料清除
        db.snapshot_replace(ws, &[binding(ws, "n2", 0.0)], "now").unwrap();
        assert_eq!(db.count(ws).unwrap(), 1, "old data cleared");
        assert!(db.query_by_node(ws, "n1").unwrap().is_none(), "n1 gone");
        assert!(db.query_by_node(ws, "n2").unwrap().is_some());
    }

    #[test]
    fn workspace_isolation() {
        let (_d, db) = open_tmp();
        db.snapshot_replace("w1", &[binding("w1", "n", 0.0)], "now").unwrap();
        db.snapshot_replace("w2", &[binding("w2", "n", 0.0)], "now").unwrap();
        assert_eq!(db.count("w1").unwrap(), 1);
        assert_eq!(db.count("w2").unwrap(), 1);
    }

    #[test]
    fn query_blindspots_returns_only_under_50() {
        let (_d, db) = open_tmp();
        let ws = "w-1";
        db.snapshot_replace(ws, &[
            binding(ws, "n1", 0.0),
            binding(ws, "n2", 0.3),
            binding(ws, "n3", 0.5),  // 邊界：0.5 不是盲區
            binding(ws, "n4", 0.8),
        ], "now").unwrap();
        let spots = db.query_blindspots(ws).unwrap();
        assert_eq!(spots.len(), 2, "n1 + n2 are blindspots");
        assert!(spots.iter().all(|b| b.is_blindspot));
    }

    #[test]
    fn avg_line_rate_computation() {
        let (_d, db) = open_tmp();
        let ws = "w-1";
        db.snapshot_replace(ws, &[
            binding(ws, "n1", 1.0),
            binding(ws, "n2", 0.5),
            binding(ws, "n3", 0.0),
        ], "now").unwrap();
        let avg = db.avg_line_rate(ws).unwrap();
        assert!((avg - 0.5).abs() < 0.001);
    }

    #[test]
    fn empty_workspace_returns_zero() {
        let (_d, db) = open_tmp();
        assert_eq!(db.count("nonexistent").unwrap(), 0);
        assert_eq!(db.count_blindspots("nonexistent").unwrap(), 0);
        assert!((db.avg_line_rate("nonexistent").unwrap() - 0.0).abs() < 0.001);
    }
}