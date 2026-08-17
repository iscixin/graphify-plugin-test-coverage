# Tasks — graphify-plugin-test-coverage（Coverage Symbol Bridge）

> 對齊 design.md / proposal.md。

## Slice 0 — 基礎單向 Bridge（快照取代式覆蓋率綁定）

- [ ] **T0.1 Crate Setup & Trait Stub**：Cargo.toml + `CoveragePlugin` struct
      實作 `GraphifyPlugin` trait（get_id / bind / get_workspace_key /
      sync_toon / on_graph_updated 預設 no-op）。
      Dependencies：`graphify-core`、`graphify-registry`、`serde` + `serde_json`、
      `rusqlite`（bundled）、`thiserror`。無 ureq、無 uuid、無 CRG 依賴。

- [ ] **T0.2 Database Migration**：`registry.rs` — coverage_bindings DDL 建表
      + CRUD DAO（併入 graphify.db，`workspace_key + canonical_node_id` PK，
      快照取代：DELETE + INSERT 單一 transaction）。

- [ ] **T0.3 LCOV Parser**：`ingest.rs` — LCOV 格式解析器：
      - `SF:` → 檔案路徑
      - `DA:line_number,hit_count` → 行級覆蓋率
      - `end_of_record` → 單檔結束
      - 輸出：`HashMap<String, HashMap<u32, u64>>`（file_path → {line → hit}）

- [ ] **T0.4 JSON Parser**：`ingest.rs` — JSON 格式（cobertura / istanbul）
      解析器，與 LCOV 解析器共用同一內部表示（`CoverageData` struct）。

- [ ] **T0.5 Reverse Resolver**：`resolver.rs` — 反轉解析器：
      - 對每個 GraphOutput node，統計其 range 內 covered / uncovered 行數
      - 產出 `coverage_bindings` 記錄
      - 檔案級 fallback：不在任何 node 範圍內的行 → `canonical_node_id = ''`
      - 複用 review plugin 的 `file_matches` 邏輯（suffix path 比對）

- [ ] **T0.6 Graph Cache**：`sync.rs` — sync_toon 收圖 → from_toon → 記憶體
      GraphOutput 快取（與 review plugin 同款全寬容 pattern）。

- [ ] **T0.7 Domain Logic**：`lib.rs` — `coverage_ingest` 業務 API：
      - 接收 LCOV 文字或 JSON 文字
      - 解析 → 查 graph_cache 反轉解析 → 快照取代寫入 coverage_bindings
      - 回傳統計：`{bound_nodes, total_lines, covered_lines, blindspots}`

- [ ] **T0.8 .toon 盲區合成**：`sync.rs` — sync_toon 時：
      - 查詢 workspace 內所有 `is_blindspot = 1` 的 binding
      - 合成 plugin_data 摘要（total_nodes, blindspots, avg_coverage）
      - 註：sync_toon 回傳的 `.toon` 封包僅含 metadata 摘要；節點級盲區
        標註由 graphify-mcp 在 sync_toon 後依 coverage_bindings 查詢結果
        附加到每個節點的 `.toon` 區塊（與 review plugin 同款協作模式）

- [ ] **T0.9 Tests + clippy**：
      - LCOV 解析測試（含 edge cases：空行、DA:0、無 coverage 檔案）
      - JSON 解析測試
      - 反轉解析測試（精確 range、多重重疊、檔案級 fallback）
      - 快照取代測試（DELETE+INSERT 原子性、舊資料清除）
      - .toon 盲區合成測試
      - clippy clean

## 非 Slice 0（未來）

- [ ] **Slice 1**：可配置盲區閾值（`COVERAGE_BLINDSPOT_THRESHOLD` env）
- [ ] **Slice 1**：branch coverage 支援（LCOV `BRDA:` 解析）
- [ ] **Slice 1**：覆蓋率趨勢歷史表（`coverage_history`）
- [ ] **Slice 1**：graphify-mcp `coverageIngest` 工具 auto-register + e2e