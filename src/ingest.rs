//! LCOV + JSON 覆蓋率解析器。
//!
//! - `parse_lcov(text)` → `CoverageData`（map[file_path] → map[line → hit]）
//! - `parse_json(text)` → `CoverageData`（同上內部表示）
//! - `CoverageIngestPayload` 為 JSON 格式的輸入序列化型別。

use std::collections::HashMap;

use serde::Deserialize;

/// 通用線級覆蓋率資料（LCOV 與 JSON 共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageData {
    /// file_path → {line_number → hit_count}
    pub files: HashMap<String, HashMap<u32, u64>>,
}

/// JSON 輸入格式（cobertura / istanbul 通用）。
#[derive(Debug, Deserialize)]
pub struct CoverageIngestPayload {
    pub version: String,
    pub source: String,
    pub workspace_key: Option<String>,
    pub files: Vec<CoverageFileEntry>,
}

#[derive(Debug, Deserialize)]
pub struct CoverageFileEntry {
    pub file_path: String,
    pub lines: Vec<CoverageLineEntry>,
}

#[derive(Debug, Deserialize)]
pub struct CoverageLineEntry {
    pub line_number: u32,
    pub hit_count: u64,
}

/// 解析 LCOV 文字，回傳檔案級行覆蓋率資料。
///
/// LCOV 格式（簡化子集）：
/// ```text
/// SF:src/auth.rs
/// DA:30,0
/// DA:31,1
/// end_of_record
/// ```
///
/// # Errors
/// 回傳 `CoverageParseError` 於格式不合預期時。
pub fn parse_lcov(text: &str) -> Result<CoverageData, CoverageParseError> {
    let mut files: HashMap<String, HashMap<u32, u64>> = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut current_lines: HashMap<u32, u64> = HashMap::new();

    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sf) = line.strip_prefix("SF:") {
            // 前一個檔案尚未 end_of_record 時自動關閉（容錯）
            if let Some(prev) = current_file.take() {
                files.insert(prev, std::mem::take(&mut current_lines));
            }
            current_file = Some(sf.to_string());
        } else if let Some(da) = line.strip_prefix("DA:") {
            let parts: Vec<&str> = da.splitn(2, ',').collect();
            if parts.len() != 2 {
                return Err(CoverageParseError::new(format!(
                    "line {}: malformed DA record: {line:?}",
                    i + 1,
                )));
            }
            let line_num: u32 = parts[0].trim().parse().map_err(|e| {
                CoverageParseError::new(format!(
                    "line {}: invalid line number {parts:?}: {e}",
                    i + 1,
                ))
            })?;
            let hit_count: u64 = parts[1].trim().parse().map_err(|e| {
                CoverageParseError::new(format!(
                    "line {}: invalid hit count {parts:?}: {e}",
                    i + 1,
                ))
            })?;
            current_lines.insert(line_num, hit_count);
        } else if line == "end_of_record" {
            if let Some(file) = current_file.take() {
                files.insert(file, std::mem::take(&mut current_lines));
            }
        }
        // 忽略其他 TAG（如 TN:、LF:、LH:）
    }

    // 結尾尚未關閉的檔案
    if let Some(file) = current_file.take() {
        files.insert(file, current_lines);
    }

    Ok(CoverageData { files })
}

/// 解析 JSON IngestPayload 文字，回傳檔案級行覆蓋率資料。
///
/// # Errors
/// 回傳 `CoverageParseError` 於 JSON 格式不合預期時。
pub fn parse_json(text: &str) -> Result<CoverageData, CoverageParseError> {
    let payload: CoverageIngestPayload = serde_json::from_str(text).map_err(|e| {
        CoverageParseError::new(format!("invalid JSON: {e}"))
    })?;

    let mut files: HashMap<String, HashMap<u32, u64>> = HashMap::new();
    for entry in payload.files {
        let mut lines = HashMap::new();
        for line in entry.lines {
            lines.insert(line.line_number, line.hit_count);
        }
        files.insert(entry.file_path, lines);
    }

    Ok(CoverageData { files })
}

/// 解析錯誤。
#[derive(Debug, Clone)]
pub struct CoverageParseError {
    pub message: String,
}

impl CoverageParseError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for CoverageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "coverage parse error: {}", self.message)
    }
}

impl std::error::Error for CoverageParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_lcov() {
        let lcov = r#"
SF:src/auth.rs
DA:30,0
DA:31,1
DA:35,0
DA:42,5
end_of_record
SF:src/db/query.rs
DA:10,3
DA:11,0
end_of_record
"#;
        let data = parse_lcov(lcov).unwrap();
        assert_eq!(data.files.len(), 2);

        let auth = data.files.get("src/auth.rs").unwrap();
        assert_eq!(auth.get(&30), Some(&0));
        assert_eq!(auth.get(&31), Some(&1));
        assert_eq!(auth.get(&42), Some(&5));
        assert_eq!(auth.len(), 4);

        let query = data.files.get("src/db/query.rs").unwrap();
        assert_eq!(query.get(&10), Some(&3));
        assert_eq!(query.len(), 2);
    }

    #[test]
    fn parse_lcov_empty() {
        let data = parse_lcov("").unwrap();
        assert!(data.files.is_empty());
    }

    #[test]
    fn parse_lcov_da_zero_is_covered() {
        // DA:0 表示該行被執行 0 次 = 未覆蓋
        let lcov = "SF:a.rs\nDA:1,0\nend_of_record\n";
        let data = parse_lcov(lcov).unwrap();
        assert_eq!(data.files["a.rs"][&1], 0);
    }

    #[test]
    fn parse_lcov_ignores_unknown_tags() {
        let lcov = "SF:a.rs\nTN:test\nDA:1,1\nda:2,0\nend_of_record\n";
        let data = parse_lcov(lcov).unwrap();
        let lines = &data.files["a.rs"];
        assert_eq!(lines.len(), 1, "DA: is case-sensitive, 'da:' ignored");
        assert_eq!(lines[&1], 1);
    }

    #[test]
    fn parse_lcov_malformed_da() {
        assert!(parse_lcov("SF:a.rs\nDA:abc\nend_of_record\n").is_err());
        assert!(parse_lcov("SF:a.rs\nDA:1,abc\nend_of_record\n").is_err());
    }

    #[test]
    fn parse_lcov_no_end_of_record() {
        // 結尾缺少 end_of_record 應容錯處理
        let lcov = "SF:a.rs\nDA:1,1\nDA:2,0\n";
        let data = parse_lcov(lcov).unwrap();
        let lines = &data.files["a.rs"];
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn parse_json_basic() {
        let json = r#"{
            "version": "1.0",
            "source": "cobertura",
            "files": [
                {
                    "file_path": "src/auth.rs",
                    "lines": [
                        {"line_number": 30, "hit_count": 0},
                        {"line_number": 31, "hit_count": 1}
                    ]
                }
            ]
        }"#;
        let data = parse_json(json).unwrap();
        assert_eq!(data.files.len(), 1);
        let auth = data.files.get("src/auth.rs").unwrap();
        assert_eq!(auth.len(), 2);
        assert_eq!(auth[&30], 0);
        assert_eq!(auth[&31], 1);
    }

    #[test]
    fn parse_json_invalid() {
        assert!(parse_json("not json").is_err());
        assert!(parse_json(r#"{"version":"1.0"}"#).is_err()); // 缺 files
    }

    #[test]
    fn parse_json_empty_files() {
        let json = r#"{"version":"1.0","source":"test","files":[]}"#;
        let data = parse_json(json).unwrap();
        assert!(data.files.is_empty());
    }
}