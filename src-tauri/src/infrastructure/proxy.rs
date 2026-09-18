use std::sync::Arc;

use reqwest::Client;
use tokio::sync::RwLock;

/// 系统统一的 HTTP 客户端类型别名
pub type SharedHttpClient = Arc<RwLock<Client>>;

/// 构建配置了代理的 HTTP 客户端
/// 代理优先级：传入的 proxy_url > 系统环境变量/注册表探测。
/// 如果没有任何代理可用，返回直连客户端（开发环境可能不需要代理）。
pub fn build_http_client(proxy_url: Option<&str>) -> Result<Client, anyhow::Error> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(2)
        .tcp_nodelay(true);

    // 优先使用显式配置的代理
    let effective_proxy = proxy_url
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(detect_system_proxy);

    if let Some(ref url) = effective_proxy {
        let proxy = reqwest::Proxy::all(url)
            .map_err(|e| anyhow::anyhow!("Invalid proxy URL '{}': {}", url, e))?;
        builder = builder.proxy(proxy);
        tracing::info!("HTTP client built with proxy: {}", url);
    } else {
        tracing::info!("HTTP client built without proxy (direct)");
    }

    builder
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {}", e))
}

/// 构建直连 HTTP 客户端（不走代理）
/// 用于访问国内服务（如千帆 API），避免代理转发导致连接失败。
pub fn build_direct_http_client() -> Result<Client, anyhow::Error> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(2)
        .tcp_nodelay(true)
        .no_proxy()
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build direct HTTP client: {}", e))?;

    tracing::info!("Direct HTTP client built (no proxy, 60s timeout)");
    Ok(client)
}

/// 从系统环境变量探测代理地址
///
/// 依次检查 HTTPS_PROXY、HTTP_PROXY、ALL_PROXY（大小写不敏感）。
/// Windows 上还会检查 internet 设置（注册表），但环境变量优先。
pub fn detect_system_proxy() -> Option<String> {
    // 环境变量优先（大小写不敏感）
    for key in &[
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // Windows 注册表探测 Internet Settings 代理
    #[cfg(target_os = "windows")]
    {
        if let Some(proxy) = detect_windows_proxy() {
            return Some(proxy);
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn detect_windows_proxy() -> Option<String> {
    use std::process::Command;

    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.contains("ProxyServer") && line.contains("REG_SZ") {
            let parts: Vec<&str> = line.split("REG_SZ").collect();
            if parts.len() >= 2 {
                let proxy_addr = parts[1].trim();
                if !proxy_addr.is_empty() {
                    let first = proxy_addr.split(';').next()?.trim();
                    if first.starts_with("http://") || first.starts_with("https://") {
                        return Some(first.to_string());
                    }
                    return Some(format!("http://{}", first));
                }
            }
        }
    }

    None
}
