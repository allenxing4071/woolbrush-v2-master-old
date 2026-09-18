use std::fs;

use serde_json::Value;

use crate::infrastructure::paths::data_dir;

/// 工具栏参数持久化文件路径: <data_dir>/toolbar.json
/// data_dir 在 dev 模式下为项目根的 data/，release 模式下为 exe 同级的 data/。
fn toolbar_path() -> std::path::PathBuf {
    let mut path = data_dir();
    path.push("toolbar.json");
    path
}

/// 读取工具栏参数（完整 JSON object）
#[tauri::command]
pub fn load_toolbar() -> Result<Value, String> {
    let path = toolbar_path();
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("Read toolbar failed: {}", e))?;
    let val: Value =
        serde_json::from_str(&raw).map_err(|e| format!("Parse toolbar failed: {}", e))?;
    Ok(val)
}

/// 保存工具栏参数（完整 JSON object，前端传整个对象）
#[tauri::command]
pub fn save_toolbar(data: Value) -> Result<(), String> {
    let path = toolbar_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Create dir failed: {}", e))?;
    }
    let json =
        serde_json::to_string_pretty(&data).map_err(|e| format!("Serialize failed: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("Write toolbar failed: {}", e))?;
    Ok(())
}
