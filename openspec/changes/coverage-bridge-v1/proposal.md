# Proposal — graphify-plugin-test-coverage（Coverage Symbol Bridge）

## Executive Summary

`graphify-plugin-test-coverage` 是 Graphify 專用的**測試覆蓋率符號橋接器
（Coverage Symbol Bridge）**：以外部測試工具（`cargo-tarpaulin` / `llvm-cov` /
`coverage.py` / `jest --coverage`）落盤的標準 LCOV 或 JSON 格式為資料源，
將行級覆蓋率數據透過 Graphify Core AST 圖譜**0ms 升維對齊**至 canonical
symbol（`{file_path}:{kind}:{name}`），並標註測試盲區（Blindspot）至 `.toon`
上下文，使 Coding Agent 在重構或修改程式碼時即時感知未經測試覆蓋的風險區域。

本 plugin **不重造測試執行引擎** — 它是純 bridge。

## Problem Statement

- **Agent 改爆既有邏輯**：Coding Agent 重構函數時無從得知該函數沒有單元測試
  覆蓋，修改後無法被 CI 捕獲。
- **行級覆蓋率無語意**：傳統 LCOV 僅記錄「行號 → hit count」，無法對應到
  AST symbol（function / struct / impl block），Agent 無法判斷「這個函數
  是否被測試」。
- **資訊孤島**：覆蓋率數據與 .toon 上下文分離，Agent 無法在查閱節點時
  同步看見其測試覆蓋狀態。

## Proposed Solution：Snap-shot 取代式 Coverage Bridge

### 資料源

- **LCOV 優先**（最通用，跨語言支援）：`cargo-llvm-cov`、`cargo-tarpaulin`、
  `coverage.py --lcov`、`jest --coverage`（皆產出 `lcov.info`）。
- **JSON 次要**（cobertura / istanbul / golang）：`coverage-final.json` /
  `cobertura.xml` 統一轉譯為內部線級覆蓋率表示。

### 快照取代模型

不同於 review plugin 的「記錄式（upsert + unresolved → resolved 生命週期）」，
coverage 是**快照取代式**：新一次 coverage 資料進來，直接覆蓋該 workspace
的所有舊 binding。沒有「銷案」概念，沒有漂移偵測。

### 盲區標註（Blindspot）

僅在節點覆蓋率 < 50%（含 0%）時才在 .toon 標註，減少噪音。

```
[src/auth.rs:function:verify_token]
 ├── ⚠️ [Review Bridge] PR #42 Warning: Potential timing attack
 └── 🧪 [Coverage Bridge] 0% (CRITICAL BLINDSPOT — No Tests Found!)
```

## Key Decisions（裁決紀錄）

| # | 裁決 | 內容 |
|---|------|------|
| R1 | 快照取代 | coverage_bindings 以 workspace_key 為範圍，每次 ingest 先 DELETE 舊資料再 INSERT，非 upsert 累積 |
| R2 | 盲區閾值 | 預設 < 50% 視為盲區（Blindspot），寫入 `is_blindspot` 欄位；固定值，暫不支援 env 覆寫 |
| R3 | 無 lifecycle API | 不需要 `coverage_resolve` / `coverage_get_context` 這類 lifecycle API — 只有 ingest + sync |
| R4 | 無 CRG 依賴 | 不走 CRG MCP client，不分析 code review 資料；純 coverage 工具資料 |
| R5 | 無 on_graph_updated | 快照取代模型不需要 drift guard — symbol 改變後下次 ingest 自動覆蓋；無 unresolved 狀態可 drift |
| R6 | MCP 歸屬 | plugin 不寫 MCP Protocol Server；`coverageIngest` 工具由 graphify-mcp 自動註冊 |
| R7 | SQLite | 併入共用 graphify.db（coverage_bindings 表，`PRIMARY KEY (workspace_key, canonical_node_id)`），不單獨開檔 |
| R8 | 檔案級 fallback | 不在任何 AST node 範圍內的行，合併為 `canonical_node_id = ''` 的檔案級記錄 |
| R9 | 反轉解析 | 解析在 ingest 階段完成：對每個 AST node range，統計 covered/uncovered 行數，直接寫入 binding，非 sync 時才計算 |

## Success Criteria

### Slice 0 — 基礎單向 Bridge

- [ ] `coverage_ingest`（file-based LCOV + JSON）→ resolver 反轉升維 → coverage_bindings 寫入 → .toon 盲區合成，全鏈路 100% 本地閉環
- [ ] 零網絡依賴、零 mock、確定性輸出
- [ ] 開源安全：版本控制無私有主機名、本地 IP
- [ ] graphify-mcp 啟動時自動註冊 `coverageIngest` 工具
- [ ] 單元測試全綠、clippy clean