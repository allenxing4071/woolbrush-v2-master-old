use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::infrastructure::clob::ClobClient;
use crate::infrastructure::data::DataClient;
use crate::infrastructure::proxy::{build_http_client, detect_system_proxy};
use crate::state::AppState;

// ── Types matching frontend TypeScript interfaces ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsForm {
    pub private_key: Option<String>,
    pub wallet_address: String,
    pub funder_address: Option<String>,
    pub proxy_url: Option<String>,
    pub feishu_webhook: Option<String>,
    pub feishu_chat_id: Option<String>,
    pub qianfan_api_key: Option<String>,
    pub qianfan_secret_key: Option<String>,
    pub llm_provider: Option<String>,
    pub bailian_api_key: Option<String>,
    pub bailian_model: Option<String>,
    pub ollama_api_key: Option<String>,
    pub ollama_url: Option<String>,
    pub ollama_model: Option<String>,
    pub llm_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsDto {
    pub wallet_address: String,
    pub funder_address: Option<String>,
    pub proxy_url: Option<String>,
    pub feishu_webhook: Option<String>,
    pub feishu_chat_id: Option<String>,
    pub has_private_key: bool,
    pub qianfan_api_key: Option<String>,
    pub qianfan_secret_key: Option<String>,
    pub llm_provider: Option<String>,
    pub bailian_api_key: Option<String>,
    pub bailian_model: Option<String>,
    pub ollama_api_key: Option<String>,
    pub ollama_url: Option<String>,
    pub ollama_model: Option<String>,
    pub llm_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub bound: bool,
    pub wallet_address: String,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub pusd_balance: Option<f64>,
    pub portfolio_value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStep {
    pub name: String,
    pub success: bool,
    pub message: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub success: bool,
    pub message: String,
    pub steps: Vec<TestStep>,
    pub pusd_balance: Option<f64>,
}

// ── Commands ──

/// 获取用户设置（从 DB 读取，private_key 不返回明文）
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<SettingsDto, String> {
    let settings = state.db.get_settings().await.map_err(|e| e.to_string())?;
    Ok(SettingsDto {
        wallet_address: settings.wallet_address,
        funder_address: settings.funder_address,
        proxy_url: settings.proxy_url,
        feishu_webhook: settings.feishu_webhook,
        feishu_chat_id: settings.feishu_chat_id,
        has_private_key: settings.private_key.is_some(),
        qianfan_api_key: settings.qianfan_api_key,
        qianfan_secret_key: settings.qianfan_secret_key,
        llm_provider: settings.llm_provider,
        bailian_api_key: settings.bailian_api_key,
        bailian_model: settings.bailian_model,
        ollama_api_key: settings.ollama_api_key,
        ollama_url: settings.ollama_url,
        ollama_model: settings.ollama_model,
        llm_prompt: settings.llm_prompt,
    })
}

/// 保存用户设置到 DB
#[tauri::command]
pub async fn save_settings(form: SettingsForm, state: State<'_, AppState>) -> Result<(), String> {
    // 如果 private_key 为空字符串，视为不修改（保留旧值）
    let pk = form
        .private_key
        .filter(|s| !s.is_empty())
        .or(state.db.get_settings().await.ok().and_then(|s| s.private_key));

    let settings = crate::infrastructure::db::UserSettings {
        private_key: pk,
        wallet_address: form.wallet_address,
        funder_address: form.funder_address,
        proxy_url: form.proxy_url,
        feishu_webhook: form.feishu_webhook,
        feishu_chat_id: form.feishu_chat_id,
        qianfan_api_key: form.qianfan_api_key,
        qianfan_secret_key: form.qianfan_secret_key,
        llm_provider: form.llm_provider,
        bailian_api_key: form.bailian_api_key,
        bailian_model: form.bailian_model,
        ollama_api_key: form.ollama_api_key,
        ollama_url: form.ollama_url,
        ollama_model: form.ollama_model,
        llm_prompt: form.llm_prompt,
    };

    state.db.save_settings(&settings).await.map_err(|e| e.to_string())?;

    // Clear cached wallet (settings changed, L2 credentials need re-derivation)
    {
        let mut cache = state.cached_wallet.lock().await;
        *cache = None;
        tracing::info!("Cached wallet cleared (settings saved)");
    }

    // 重建全局 HTTP 客户端（同时更新 GammaClient 和所有共享引用）
    state
        .rebuild_http_client(settings.proxy_url.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// 测试连接（验证 Gamma API + CLOB API 连通性 + 代理可用性）
#[tauri::command]
pub async fn test_connection_with_settings(form: SettingsForm) -> Result<TestResult, String> {
    let mut steps = Vec::new();
    let mut all_success = true;

    // Step 1: 系统代理探测
    let t = Instant::now();
    let sys_proxy = detect_system_proxy();
    let sys_proxy_msg = match &sys_proxy {
        Some(p) => format!("Detected system proxy: {}", p),
        None => "No system proxy detected".to_string(),
    };
    steps.push(TestStep {
        name: "System Proxy".to_string(),
        success: true,
        message: sys_proxy_msg,
        duration_ms: t.elapsed().as_millis() as u64,
    });

    // Step 2: 构建测试用 HTTP 客户端
    let effective_proxy = form
        .proxy_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(sys_proxy.as_deref());

    let t = Instant::now();
    let client = match build_http_client(effective_proxy) {
        Ok(c) => {
            steps.push(TestStep {
                name: "HTTP Client".to_string(),
                success: true,
                message: format!(
                    "HTTP client built (proxy: {})",
                    effective_proxy.unwrap_or("direct")
                ),
                duration_ms: t.elapsed().as_millis() as u64,
            });
            c
        }
        Err(e) => {
            steps.push(TestStep {
                name: "HTTP Client".to_string(),
                success: false,
                message: format!("Failed to build HTTP client: {}", e),
                duration_ms: t.elapsed().as_millis() as u64,
            });
            return Ok(TestResult {
                success: false,
                message: "HTTP client build failed".to_string(),
                steps,
                pusd_balance: None,
            });
        }
    };

    // Step 3: Gamma API 连通性测试
    let t = Instant::now();
    match client
        .get("https://gamma-api.polymarket.com/events?tag_id=103040&active=true&closed=false&limit=1")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                steps.push(TestStep {
                    name: "Gamma API".to_string(),
                    success: true,
                    message: format!("Gamma API responded ({})", status),
                    duration_ms: t.elapsed().as_millis() as u64,
                });
            } else {
                steps.push(TestStep {
                    name: "Gamma API".to_string(),
                    success: false,
                    message: format!("Gamma API returned status {}", status),
                    duration_ms: t.elapsed().as_millis() as u64,
                });
                all_success = false;
            }
        }
        Err(e) => {
            steps.push(TestStep {
                name: "Gamma API".to_string(),
                success: false,
                message: format!("Gamma API connection failed: {}", e),
                duration_ms: t.elapsed().as_millis() as u64,
            });
            all_success = false;
        }
    }

    // Step 4: CLOB API 连通性测试
    let t = Instant::now();
    match client
        .get("https://clob.polymarket.com/markets?next_cursor=MA==")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                steps.push(TestStep {
                    name: "CLOB API".to_string(),
                    success: true,
                    message: format!("CLOB API responded ({})", status),
                    duration_ms: t.elapsed().as_millis() as u64,
                });
            } else {
                steps.push(TestStep {
                    name: "CLOB API".to_string(),
                    success: false,
                    message: format!("CLOB API returned status {}", status),
                    duration_ms: t.elapsed().as_millis() as u64,
                });
                all_success = false;
            }
        }
        Err(e) => {
            steps.push(TestStep {
                name: "CLOB API".to_string(),
                success: false,
                message: format!("CLOB API connection failed: {}", e),
                duration_ms: t.elapsed().as_millis() as u64,
            });
            all_success = false;
        }
    }

    // Step 5: 钱包地址校验（格式检查）
    let t = Instant::now();
    if !form.wallet_address.is_empty() {
        let valid = form.wallet_address.starts_with("0x") && form.wallet_address.len() == 42;
        steps.push(TestStep {
            name: "Wallet Address".to_string(),
            success: valid,
            message: if valid {
                "Wallet address format valid".to_string()
            } else {
                "Wallet address format invalid (expected 0x + 40 hex chars)".to_string()
            },
            duration_ms: t.elapsed().as_millis() as u64,
        });
        if !valid {
            all_success = false;
        }
    } else {
        steps.push(TestStep {
            name: "Wallet Address".to_string(),
            success: true,
            message: "No wallet address provided (optional)".to_string(),
            duration_ms: 0,
        });
    }

    Ok(TestResult {
        success: all_success,
        message: if all_success {
            "All connection tests passed".to_string()
        } else {
            "Some connection tests failed".to_string()
        },
        steps,
        pusd_balance: None,
    })
}

/// 获取账户摘要：头像、名称、可用现金(pusd_balance)、总资产(portfolio_value)
///
/// 并发查询三个数据源：
/// - Gamma get_profile → username + avatar_url
/// - ClobClient get_pusd_balance → pusd_balance（可用现金）
/// - DataClient get_portfolio_value → portfolio_value（持仓总市值）
///
/// portfolio_value = 持仓市值，总资产 = pusd_balance + portfolio_value
#[tauri::command]
pub async fn get_account_summary(state: State<'_, AppState>) -> Result<AccountSummary, String> {
    let settings = state.db.get_settings().await.map_err(|e| e.to_string())?;

    let bound = settings.private_key.is_some() && !settings.wallet_address.is_empty();
    let wallet_addr = settings.wallet_address.clone();

    if wallet_addr.is_empty() {
        return Ok(AccountSummary {
            bound: false,
            wallet_address: String::new(),
            username: None,
            avatar_url: None,
            pusd_balance: None,
            portfolio_value: None,
        });
    }

    let http = state.http.read().await;

    // 并发查询四个数据源
    let gamma = state.gamma.read().await;
    let gamma_clone = gamma.clone();

    let funder = settings
        .funder_address
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("");

    let (profile_result, pusd_result, wallet_positions_result, funder_positions_result) = tokio::join!(
        gamma_clone.get_profile(&wallet_addr),
        async {
            // pUSD 余额通过 Polygon RPC eth_call 查询，不需要 L2 凭证
            // 直接使用共享 HTTP 客户端，避免每次创建 Wallet + 派生 L2 凭证
            let balance_addr = if !funder.is_empty() { funder } else { &wallet_addr };
            ClobClient::get_pusd_balance(&http, balance_addr).await
        },
        DataClient::get_positions(&http, &wallet_addr),
        async {
            if funder.is_empty() {
                Ok(Vec::new())
            } else {
                DataClient::get_positions(&http, funder).await
            }
        },
    );

    // 用户名：优先 name，为空回退 pseudonym
    let (username, avatar_url) = match profile_result {
        Ok(p) => {
            let name = if !p.name.is_empty() {
                Some(p.name)
            } else if !p.pseudonym.is_empty() {
                Some(p.pseudonym)
            } else {
                None
            };
            let avatar = p.profile_image.clone();
            (name, avatar)
        }
        Err(e) => {
            tracing::warn!("get_profile failed: {}", e);
            (None, None)
        }
    };

    let pusd_balance = match pusd_result {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!("get_pusd_balance failed: {}", e);
            None
        }
    };

    // 持仓总额 = 所有持仓的 size * cur_price 之和
    let mut positions_value = 0.0_f64;
    if let Ok(ref positions) = wallet_positions_result {
        for p in positions {
            positions_value += p.size * p.cur_price;
        }
    }
    if let Ok(ref positions) = funder_positions_result {
        for p in positions {
            positions_value += p.size * p.cur_price;
        }
    }

    // 总资产 = 可用现金 + 持仓总额
    let portfolio_value = pusd_balance.map(|cash| cash + positions_value);

    Ok(AccountSummary {
        bound,
        wallet_address: wallet_addr,
        username,
        avatar_url,
        pusd_balance,
        portfolio_value,
    })
}
