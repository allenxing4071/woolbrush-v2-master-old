use std::path::PathBuf;

/// 返回应用的持久化数据目录。
///
/// - **Debug (dev)**: `../data` — 相对于 `src-tauri/`（CWD），指向项目根的 `data/`，
///   避免 Tauri dev watcher 检测到文件变更触发重启。
/// - **Release**: 与可执行程序同级的 `data/`。
///
/// macOS 的 `.app` 是一个目录包，可执行文件位于 `X.app/Contents/MacOS/` 内。若直接
/// 取 exe 同级目录，数据会写进包内部：替换新版本时整包被覆盖，数据库（含私钥）、
/// toolbar.json 与复盘日志会一并丢失，同时也破坏 app 签名。因此在 `.app` 形态下
/// 上溯三层，把 `data/` 放到 `.app` 同级，与 Windows 上「exe 旁边」的语义保持一致。
///
/// 所有需要写本地文件的模块都应调用此函数，而非硬编码相对路径。
pub fn data_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        return PathBuf::from("../data");
    }

    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let in_app_bundle = base.ends_with("MacOS")
        && base.parent().is_some_and(|p| p.ends_with("Contents"))
        && base
            .parent()
            .and_then(|p| p.parent())
            .is_some_and(|p| p.extension().is_some_and(|e| e == "app"));

    let base = if in_app_bundle {
        base.ancestors().nth(3).map_or(base.clone(), |p| p.to_path_buf())
    } else {
        base
    };

    base.join("data")
}
