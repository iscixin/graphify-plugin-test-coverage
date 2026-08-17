# Design — graphify-plugin-test-coverage（Coverage Symbol Bridge）

> 狀態：**草案（Proposal）**。
> 方向變更記錄：無。
> 本文檔為唯一權威設計基礎（openspec design.md）。

## 1. 定位

`graphify-plugin-test-coverage` 是 Graphify 生態的**測試覆蓋率語意橋接器
（Coverage Symbol Bridge）**：以外部測試工具落盤的標準 LCOV/JSON 為覆蓋率
資料源，把行級覆蓋率數據（`file_path` + `line_number` + `hit_count`）
透過 Graphify Core 的 AST 圖譜升維對齊至穩定的 canonical symbol
（`{file_path}:{kind}:{name}`），並在 `.toon` 上下文中標註測試盲區。

- **不重造引擎**：不實作任何測試執行器、不安裝測試工具 SDK、不發送線上 API
  請求、不修改 Core AST 圖譜。
- **純 bridge**：覆蓋率資料 100% 來自外部工具落盤；本 plugin 負責「升維綁定 +
  快照持久化 + .toon 盲區合成」。

## 2. 架構

```
┌───────────────────────────────────────────────────────────────┐
│ 外部測試工具（Coverage 資料源）                                  │
│  ├─ lcov.info（通用 LCOV 格式，跨語言）                          │
│  │  （cargo-llvm-cov / tarpaulin / coverage.py / jest）         │
│  └─ coverage.json / cobertura.xml（JSON 次要格式）               │
└──────────────────────────────┬────────────────────────────────┘
                               │
┌──────────────────────────────▼────────────────────────────────┐
│ graphify-plugin-test-coverage（Rust GraphifyPlugin）            │
│  ├─ ingest.rs    # LCOV + JSON 解析器 + 轉譯                    │
│  ├─ resolver.rs  # 反轉 resolver：node range → 行號集合覆蓋率統計 │
│  ├─ registry.rs  # coverage_bindings DAO（併入 graphify.db）    │
│  └─ sync.rs      # .toon 盲區上下文合成                         │
└──────────────┬─────────────────────────────────▲───────────────┘
               │ 1. Auto-register coverageIngest │
┌──────────────▼─────────────────────────────────┴───────────────┐
│ graphify-mcp（MCP Gateway）                                     │
│  - 自動註冊 coverageIngest tool                                 │
└─────────────────────────────────────────────────────────────────┘
```

## 3. 鋼鐵邊界（Scope）

### 3.1 In-Scope

1. **LCOV 讀取**：解析標準 LCOV 格式（`SF:`、`DA:`、`end_of_record`）。
2. **JSON 讀取**：解析 cobertura `/` istanbul `coverage-final.json` 格式。
3. **Symbol Mapping**：將 `file_path + 行號集合` 轉換為每個 canonical symbol
   的覆蓋率統計。
4. **SQLite 管理**：`coverage_bindings` 表（**併入 graphify.db**），
   記錄每個 symbol 的 covered / uncovered 行數與百分比。
5. **.toon 盲區合成**：sync_toon 時對 < 50% 覆蓋率的節點標註 Blindspot 警示。

### 3.2 Out-of-Scope（硬性禁止）

- ⛔ 不實作測試執行器（不跑 `cargo test`、`pytest`、`jest`）。
- ⛔ 不支援其他覆蓋率格式（只認 LCOV + JSON/cobertura）。
- ⛔ 不發送線上 API 請求（無 Token/OAuth）。
- ⛔ 不修改 Core AST 圖譜（不把 coverage 節點塞進 petgraph）。
- ⛔ 不寫 MCP Protocol Server（transport 由 graphify-mcp 統一處理）。
- ⛔ 不實作 lifecycle API（無 `coverage_resolve` / `coverage_get_context`）。
- ⛔ 不走 CRG / Draco MCP client（無外部分析依賴）。
- ⛔ 不實作 `on_graph_updated` drift guard（快照取代模型不需要）。

## 4. 資料架構

### 4.1 輸入格式

#### LCOV 格式（主）

```lcov
SF:src/auth.rs
DA:30,0
DA:31,1
DA:35,0
DA:42,5
end_of_record
SF:src/db/query.rs
DA:10,3
end_of_record
```

- `SF:` = Source File（絕對或相對路徑）
- `DA:line_number,hit_count` = 行級執行次數（0 = 未覆蓋）
- `end_of_record` = 單檔結束

#### JSON 格式（次要，cobertura / istanbul）

```json
{
  "version": "1.0",
  "source": "cobertura",
  "workspace_key": "my-app-v1",
  "files": [
    {
      "file_path": "src/auth.rs",
      "lines": [
        { "line_number": 30, "hit_count": 0 },
        { "line_number": 31, "hit_count": 1 },
        { "line_number": 35, "hit_count": 0 },
        { "line_number": 42, "hit_count": 5 }
      ]
    }
  ]
}
```

### 4.2 coverage_bindings 表（graphify.db 內）

```sql
CREATE TABLE IF NOT EXISTS coverage_bindings (
    workspace_key     TEXT NOT NULL,     -- plugin 當前 bound 的 workspace_key
    canonical_node_id TEXT NOT NULL,     -- GraphOutput Node.id 原樣，空字串 = 檔案級
    file_path         TEXT NOT NULL,     -- 原始檔案路徑
    total_lines       INTEGER NOT NULL,  -- 該節點範圍內總行數
    covered_lines     INTEGER NOT NULL,  -- hit_count > 0 的行數
    line_rate         REAL NOT NULL,     -- 0.0 ~ 1.0（covered / total）
    is_blindspot      INTEGER NOT NULL,  -- 1 = line_rate < 0.5（盲區）
    updated_at        TEXT NOT NULL,     -- RFC 3339
    PRIMARY KEY (workspace_key, canonical_node_id)
);

CREATE INDEX IF NOT EXISTS idx_coverage_node
    ON coverage_bindings (workspace_key, canonical_node_id);

CREATE INDEX IF NOT EXISTS idx_coverage_blindspot
    ON coverage_bindings (workspace_key, is_blindspot);
```

> 裁決 R7：SQLite 併入既有 graphify.db（與 review plugin 同款 pattern —
> plugin 對同一檔開自己的 `rusqlite::Connection` + `CREATE TABLE IF NOT
> EXISTS`）。

### 4.3 快照取代語意

每次 `coverage_ingest` 被呼叫時：

```
BEGIN TRANSACTION;
DELETE FROM coverage_bindings WHERE workspace_key = ?;
-- 逐筆 INSERT 新的 binding
COMMIT;
```

單一 transaction 確保原子性：ingest 失敗（如格式錯誤）不殘留半筆舊資料。

## 5. 反轉解析器（Reverse Resolver）

與 review plugin 的「行號 → 單一最內層節點」不同，coverage 需要的是
「給定節點 range，算出涵蓋行中有多少 hit」。

### 演算法

```
輸入：GraphOutput + HashMap<file_path, HashMap<line_number, hit_count>>
輸出：Vec<CoverageBinding>

for each node in GraphOutput.nodes:
    relevant_lines = coverage_map[node.source_file]
        .iter()
        .filter(|(line, _)| node.start_line <= *line && *line <= node.end_line)
    total = relevant_lines.count()
    covered = relevant_lines.filter(|(_, hit)| *hit > 0).count()
    line_rate = covered / total（若 total == 0 則 line_rate = 0）
    產出一筆 CoverageBinding { canonical_node_id, file_path, total_lines, covered_lines, line_rate, is_blindspot }
```

### 檔案級 fallback（R8）

對有 coverage data 但不在任何 AST node 範圍內的行，合併為一筆
`canonical_node_id = ''` 的記錄，代表該檔案未被 symbol 涵蓋的殘餘行。
這筆記錄的 `file_path` 為原始 LCOV 路徑，`line_rate` 只計算殘餘行。

## 6. .toon 盲區合成

### sync_toon 輸出

```json
{
  "coverage": {
    "workspace_key": "my-app-v1",
    "total_nodes": 42,
    "blindspots": 5,
    "avg_coverage": 73.4
  }
}
```

### 節點層級標註

sync_toon 時，對每個 GraphOutput node，查 `coverage_bindings` 中
`workspace_key = 此工作區 AND canonical_node_id = node.id`：

- 無記錄 → 不標註（該節點無 coverage data 交集）
- `is_blindspot = 1` → `.toon` 加入 `🧪 [Coverage Bridge] X% (BLINDSPOT)`
- `is_blindspot = 0` → 不標註（僅盲區警示，不造成噪音）

## 7. 無需實作項目（與 review plugin 對比）

| 功能 | review plugin | test-coverage plugin | 理由 |
|------|--------------|---------------------|------|
| `on_graph_updated` | Drift auto-resolver | 不需要 | 快照取代，下次 ingest 自動覆蓋 |
| `review_resolve` | 手動銷案 | 不需要 | 無 resolved/unresolved 生命週期 |
| `review_get_context` | 查詢指定 node 的 review | 不需要 | sync_toon 時已附註盲區 |
| `crg_client` | CRG MCP client 骨架 | 不需要 | 無外部分析依賴 |
| `impact.rs` | BFS 衝擊半徑 | 不需要 | 無 domain event |
| `set_notify_callback` | ImpactAlert 推送 | 不需要 | 無 domain event |

## 8. 對 graphify-core v1 契約驗證

- `GraphifyPlugin` trait：`get_id` / `bind` / `get_workspace_key` /
  `sync_toon` / `on_graph_updated`（default no-op 即可）。
- `Node` 欄位：`id` / `source_file` / `start_line` / `end_line` —
  反轉解析器依賴這三個欄位計算覆蓋率。

## 9. 已知限制 / [待討論]

- **僅線級覆蓋率**：branch coverage、function coverage、mutation score
  不在 Slice 0 範圍內。LCOV 的 `BRDA:`（branch data）保留欄位但暫不解析。
- **盲區閾值固定**：預設 < 50% 為 blindspot。如需可配置，留待 Slice 1
  env `COVERAGE_BLINDSPOT_THRESHOLD` 覆寫。
- **無覆蓋率趨勢**：快照取代模型不保留歷史覆蓋率數據。如需趨勢分析（如
  「這個函數覆蓋率從 80% 降到 30%」），需在 Slice 1 加 `coverage_history` 表。
- **檔案級覆蓋率**：`canonical_node_id = ''` 的記錄目前僅供參考，不影響
  .toon 節點標註。如需在 .toon 顯示檔案級覆蓋率摘要，留待 Slice 1。