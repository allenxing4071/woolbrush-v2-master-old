use std::path::PathBuf;

/// 返回应用的持久化数据目录。
///
/// - **Debug (dev)**: `../data` — 相对于 `src-tauri/`（CWD），指向项目根的 `data/`，
///   避免 Tauri dev watcher 检测到文件变更触发重启。
/// - **Release (打包 exe)**: 基于 exe 所在目录拼 `data/`，确保数据文件与 exe 同级。
///
/// 所有需要写本地文件的模块都应调用此函数，而非硬编码相对路径。
pub fn data_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from("../data")
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("data")
    }
}
