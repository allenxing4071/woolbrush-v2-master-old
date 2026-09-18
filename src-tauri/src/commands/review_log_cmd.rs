use std::fs::{self, OpenOptions};
use std::io::Write;

use chrono::Utc;

use crate::infrastructure::paths::data_dir;

/// 追加一行 JSONL 到按日期组织的复盘日志文件
///
/// 文件路径: <data_dir>/review/YYYY-MM-DD.jsonl
/// data_dir 在 dev 模式下为项目根的 data/，release 模式下为 exe 同级的 data/。
#[tauri::command]
pub fn append_review_log(line: String) -> Result<(), String> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let mut path = data_dir();
    path.push("review");
    path.push(format!("{}.jsonl", date));

    // 创建目录（如不存在）
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Create dir failed: {}", e))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Open file failed: {}", e))?;

    file.write_all(line.as_bytes())
        .map_err(|e| format!("Write failed: {}", e))?;
    file.write_all(b"\n")
        .map_err(|e| format!("Write newline failed: {}", e))?;

    Ok(())
}
