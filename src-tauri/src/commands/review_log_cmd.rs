use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;

use chrono::Utc;
use tauri::State;

use crate::infrastructure::paths::data_dir;
use crate::state::AppState;

/// 账户标识缓存。取自 funder_address（缺失时退回 wallet_address）的末 6 位。
/// 只在成功拿到地址后才写入缓存——否则下一次调用会重试，避免程序启动早期
/// 设置尚未就绪时把标识永久固化成 "unknown"。
static ACCT_TAG: OnceLock<String> = OnceLock::new();

/// 取地址末 6 位小写十六进制作为账户标识。
/// 用末尾而非开头，因为同一批钱包的地址前缀有时相近。
fn short_addr(addr: &str) -> String {
    let hex: String = addr
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let tail: String = hex.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    if tail.is_empty() {
        "unknown".to_string()
    } else {
        tail.to_lowercase()
    }
}

async fn acct_tag(state: &State<'_, AppState>) -> String {
    if let Some(t) = ACCT_TAG.get() {
        return t.clone();
    }
    let addr = match state.db.get_settings().await {
        Ok(s) => s
            .funder_address
            .filter(|a| !a.trim().is_empty())
            .or_else(|| {
                let w = s.wallet_address.trim().to_string();
                if w.is_empty() { None } else { Some(w) }
            }),
        Err(_) => None,
    };
    match addr {
        Some(a) => {
            let tag = short_addr(&a);
            let _ = ACCT_TAG.set(tag.clone());
            tag
        }
        // 未取到地址：本次用 unknown，但不写缓存，下次继续尝试
        None => "unknown".to_string(),
    }
}

/// 追加一行 JSONL 到按「日期 + 账户」组织的复盘日志文件
///
/// 文件路径: <data_dir>/review/YYYY-MM-DD_<acct>.jsonl
/// data_dir 在 dev 模式下为项目根的 data/，release 模式下为 exe 同级的 data/。
///
/// 多账户并行时每个账户写自己的文件，且每行额外注入 `acct` 字段，
/// 这样即使日志被汇总合并到一起也能还原来源账户。
#[tauri::command]
pub async fn append_review_log(line: String, state: State<'_, AppState>) -> Result<(), String> {
    let tag = acct_tag(&state).await;

    let date = Utc::now().format("%Y-%m-%d").to_string();
    let mut path = data_dir();
    path.push("review");
    path.push(format!("{}_{}.jsonl", date, tag));

    // 在行首注入账户标识。前端序列化出的行必定以 '{' 开头，此处按位置插入
    // 而不是重新解析 JSON，避免每条日志都付出一次反序列化开销。
    let tagged = match line.strip_prefix('{') {
        Some(rest) => format!("{{\"acct\":\"{}\",{}", tag, rest),
        None => line,
    };

    // 创建目录（如不存在）
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Create dir failed: {}", e))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Open file failed: {}", e))?;

    file.write_all(tagged.as_bytes())
        .map_err(|e| format!("Write failed: {}", e))?;
    file.write_all(b"\n")
        .map_err(|e| format!("Write newline failed: {}", e))?;

    Ok(())
}
