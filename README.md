# graphify-plugin-test-coverage

**Pure Symbol Bridge: 測試覆蓋率 → Graphify AST 節點綁定**

將 LCOV（`.info`）或 cobertura JSON 格式的測試覆蓋率資料解析後，透過 graphify 的 Line-to-Symbol Resolver 解析到 canonical AST node id，寫入 `graphify.db` 的 `coverage_bindings` 表。`.toon` 合成階段將覆蓋率 < 50% 的節點標註為 `Blindspot`，讓 Agent 在修改前先補測試。

## 設計原則

- **Snapshot-Replacement Lifecycle**：每次 `coverageIngest` 以 `DELETE + INSERT` 取代舊資料，不做增量更新。
- **無外部依賴**：不依賴 CRG client、uuid、HTTP client。純 Rust 標準庫 + rusqlite + serde_json。
- **與 Review Plugin 同構**：共用同一套 `resolver.rs` 的 Line-to-Symbol 解析路徑，但無需 drift 自動解析、review lifecycle API。

## 輸入格式

| 格式 | 來源工具 | 範例 |
|------|---------|------|
| LCOV `.info` | `cargo-tarpaulin`, `lcov`, `geninfo` | `SF:src/main.rs\nDA:1,1\nDA:2,0\nend_of_record` |
| cobertura JSON | `coverage.py`, `jest --coverage`, `vitest --coverage` | `{"coverage": {"files": [{"file": "src/main.rs", "lines": {"covered": [1], "missed": [2]}}]}}` |

## 嵌入方式

### CLI (`graphify coverage`)

```bash
# 從檔案匯入 LCOV
graphify coverage ingest-lcov --payload coverage/lcov.info

# 從 stdin 匯入 JSON
cat coverage/cobertura.json | graphify coverage ingest-json

# 查詢單一節點
graphify coverage query --node "src/a.rs:function:f"

# 列出所有盲區
graphify coverage blindspots
```

### MCP (`graphify-mcp`)

| 工具 | 說明 |
|------|------|
| `coverageIngest` | 匯入 LCOV 或 JSON 覆蓋率資料 |
| `coverageGetContext` | 查詢單一節點覆蓋率 |
| `coverageBlindspots` | 列出所有盲區 |

## 盲區閾值

預設：**line_rate < 0.5**（50%）。可透過 `CoveragePlugin::new().with_blindspot_threshold(0.7)` 調整。

## 架構

```
src/
├── lib.rs          # CoveragePlugin 主體：ingest/cli/mcp 入口
├── ingest.rs       # LCOV + cobertura JSON parser
├── registry.rs     # coverage_bindings DDL + DAO
├── resolver.rs     # Reverse resolver: node range → line statistics
└── sync.rs         # Graph cache + .toon blindspot synthesis
```

## 授權

MIT