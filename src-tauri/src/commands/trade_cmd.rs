use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::AppError;
use crate::infrastructure::clob::{ClobClient, OrderArgs, OrderBookEntry, OrderType};
use crate::infrastructure::data::{DataClient, Position};
use crate::infrastructure::wallet::Wallet;
use crate::state::AppState;

const CLOB_BASE_URL: &str = "https://clob.polymarket.com";
const POLYGON_CHAIN_ID: u64 = 137;
const MIN_ORDER_SIZE: u64 = 5;
/// 上限价以内的可吃深度低于目标数量的该比例时放弃开仓（少下的下限）
const MIN_FILL_RATIO: f64 = 0.5;

/// 深度规划结果
#[derive(Debug, Clone, PartialEq)]
struct FillPlan {
    /// 实际计划下单数量（≤ target，≤ available）
    size: u64,
    /// 上限价以内可吃的总量（向下取整）
    available: u64,
    /// 限价：吃到 size 所需用到的最深一档价格（FAK 下不会成交在此价之上）
    limit_price: f64,
}

/// 逐档累加 ≤ price_cap 的卖单，规划买入数量与限价。
/// asks 需按价格升序（get_orderbook 已排序）。
fn plan_fill(asks: &[OrderBookEntry], target: u64, price_cap: f64) -> FillPlan {
    let mut cum = 0u64;
    let mut size = 0u64;
    let mut limit_price = asks.first().map(|a| a.price).unwrap_or(price_cap);
    for lvl in asks {
        if lvl.price > price_cap + 1e-9 {
            break;
        }
        let lvl_size = lvl.size.max(0.0).floor() as u64;
        if lvl_size == 0 {
            continue;
        }
        cum += lvl_size;
        if size < target {
            size = target.min(cum);
            limit_price = lvl.price;
        }
    }
    FillPlan { size, available: cum, limit_price }
}

// ── Types matching frontend TypeScript interfaces ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub market_id: String,
    pub question: String,
    pub city: String,
    pub city_tz: String,
    pub side: String,
    pub token_id: String,
    pub threshold: String,
    pub current_price: f64,
    pub expected_profit: f64,
    pub confirmed_threshold: Option<String>,
    pub strategy_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub id: String,
    pub market_id: String,
    pub question: String,
    pub city: String,
    pub side: String,
    pub token_id: String,
    pub threshold: String,
    pub entry_price: f64,
    pub size: f64,
    pub cost: f64,
    pub timestamp: String,
    pub exit_price: Option<f64>,
    pub exit_timestamp: Option<String>,
    pub realized_pnl: Option<f64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyPnl {
    pub date: String,
    pub pnl: f64,
}

/// Sync result returned by sync_open_positions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub total_open: usize,
    pub synced: usize,
    pub still_open: usize,
    pub errors: usize,
    pub details: Vec<SyncDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncDetail {
    pub token_id: String,
    pub city: String,
    pub action: String, // "synced" | "still_open" | "error"
    pub message: String,
}

// ── Commands ──

/// 开仓（实盘）
///
/// 前端传入 Opportunity 对象和固定金额。
/// 后端获取实时 best_ask 价格，计算 size，提交 CLOB 买单。
///
/// `max_price`：可选的成交价上限（通常为操作栏 ASK 上限）。LLM 决策与实际下单之间
/// 存在数秒延迟，盘口可能已经上移；若实时 best_ask 超过上限则拒绝下单，
/// 避免以 0.999 这类几乎无盈利空间的价格追单（复盘：denver 88-89°F 决策价 0.99，成交 0.999）。
#[tauri::command]
pub async fn open_position(
    opp: Opportunity,
    amount: f64,
    max_price: Option<f64>,
    state: State<'_, AppState>,
) -> Result<TradeRecord, String> {
    if amount <= 0.0 {
        return Err("Order amount must be positive".to_string());
    }

    let wallet = get_cached_wallet(&state).await?;
    let clob = ClobClient::new().with_wallet(wallet);

    let http = state.http.read().await;
    let book = clob
        .get_orderbook(&http, &opp.token_id)
        .await
        .map_err(|e| format!("Failed to get orderbook: {}", e))?;

    let best_ask = book
        .asks
        .first()
        .map(|a| a.price)
        .ok_or("No ask price in orderbook".to_string())?;

    // 成交价上限硬校验：盘口已上移则拒绝，不追单
    let cap = max_price.filter(|c| c.is_finite() && *c > 0.0);
    if let Some(cap) = cap {
        if best_ask > cap + 1e-9 {
            return Err(format!(
                "Best ask {:.4} exceeds max price {:.4}, order rejected (price moved after decision)",
                best_ask, cap
            ));
        }
    }

    // 深度感知：只吃 ≤ 上限价的卖单；深度不够就少下，太少就不下
    let price_cap = cap.unwrap_or(best_ask);
    let mut target_size = (amount / best_ask) as u64;
    if target_size < MIN_ORDER_SIZE {
        // 目标金额太小：若不超预算 30% 则按最小下单量下
        let min_cost = MIN_ORDER_SIZE as f64 * best_ask;
        if min_cost > amount * 1.30 {
            return Err(format!(
                "Order size {} < minimum {} (amount ${:.2} / price {:.4}, need ${:.2} for minimum)",
                target_size, MIN_ORDER_SIZE, amount, best_ask, min_cost
            ));
        }
        tracing::info!(
            "Size bumped to minimum {} (cost ${:.2} vs amount ${:.2})",
            MIN_ORDER_SIZE, min_cost, amount
        );
        target_size = MIN_ORDER_SIZE;
    }
    let plan = plan_fill(&book.asks, target_size, price_cap);
    // 深度不足一半：不下；不足最小量：不下
    if plan.available < MIN_ORDER_SIZE
        || (plan.available as f64) < target_size as f64 * MIN_FILL_RATIO
    {
        return Err(format!(
            "depth_insufficient: only {} shares available at <= {:.3}, need >= {:.0}% of target {} (min {}) — skipped",
            plan.available, price_cap, MIN_FILL_RATIO * 100.0, target_size, MIN_ORDER_SIZE
        ));
    }
    let size = plan.size;
    let price = plan.limit_price;
    if size < target_size {
        tracing::info!(
            "Depth-limited order: target {} -> {} shares (available {} at <= {:.3})",
            target_size, size, plan.available, price
        );
    }

    let order_args = OrderArgs {
        price,
        size,
        token_id: opp.token_id.clone(),
        // FAK：吃掉可成交部分即撤，不在盘口留挂单，避免账面持仓与实际不一致
        order_type: OrderType::Fak,
    };

    let order_resp = clob
        .create_order(&http, &order_args)
        .await
        .map_err(|e| format!("Order submission failed: {}", e))?;

    // 以回执为准记账：FAK 可能部分成交。CLOB 返回十进制 shares/USDC；若疑似 1e6 原始单位则归一化
    let normalize = |v: Option<f64>| v.map(|x| if x > size as f64 * 1_000.0 { x / 1e6 } else { x });
    let (filled_size, filled_cost) = match (normalize(order_resp.taking_amount), normalize(order_resp.making_amount)) {
        (Some(t), Some(m)) if t > 0.0 && t <= size as f64 + 1e-6 && m > 0.0 => (t, m),
        (Some(t), None) if t > 0.0 && t <= size as f64 + 1e-6 => (t, t * price),
        // status=delayed 表示已撮合但延迟结算，此时数量可能为 0，不能当作未成交
        (Some(t), _) if t <= 0.0 && order_resp.status.as_deref() != Some("delayed") => {
            return Err(format!(
                "Order {} not filled (status={:?}); nothing bought, no position recorded",
                order_resp.order_id,
                order_resp.status
            ));
        }
        _ => {
            tracing::warn!(
                "Order {} response lacks fill amounts (status={:?}); recording requested size {} @ {:.4}",
                order_resp.order_id, order_resp.status, size, price
            );
            (size as f64, size as f64 * price)
        }
    };
    let entry_price = if filled_size > 0.0 { filled_cost / filled_size } else { price };

    tracing::info!(
        "Position opened: {} {} size={:.2}/{} price={:.4} cost={:.2} order_id={} status={:?}",
        opp.city,
        opp.side,
        filled_size,
        size,
        entry_price,
        filled_cost,
        order_resp.order_id,
        order_resp.status
    );

    let record = TradeRecord {
        id: uuid::Uuid::new_v4().to_string(),
        market_id: opp.market_id,
        question: opp.question,
        city: opp.city,
        side: opp.side,
        token_id: opp.token_id,
        threshold: opp.threshold,
        entry_price,
        size: filled_size,
        cost: filled_cost,
        timestamp: chrono::Utc::now().to_rfc3339(),
        exit_price: None,
        exit_timestamp: None,
        realized_pnl: None,
        status: "open".to_string(),
    };

    // 持久化到数据库
    state.db.save_trade(&record).await
        .map_err(|e| format!("Order submitted (id={}) but failed to save trade record: {}. Please verify position manually.", order_resp.order_id, e))?;

    Ok(record)
}

/// 平仓结果（供监控器和前端使用）
#[derive(Debug, Clone, Serialize)]
pub struct CloseResult {
    pub token_id: String,
    pub city: String,
    pub size: u64,
    pub exit_price: f64,
    pub proceeds: f64,
    pub realized_pnl: f64,
    /// 平仓原因: "manual" | "stop_loss" | "take_profit"
    pub reason: String,
}

/// 执行平仓的核心逻辑（供 close_position Tauri command 和 position_monitor 共用）
///
/// 1. 查询链上持仓获取 size（fallback 到 DB）
/// 2. 获取 best_bid
/// 3. 构建 wallet + ClobClient 提交卖单
/// 4. 更新 DB 交易记录
pub async fn execute_close_position(
    state: &AppState,
    token_id: &str,
    reason: &str,
) -> Result<CloseResult, String> {
    let settings = state
        .db
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))?;

    let wallet_addr = settings.wallet_address.clone();
    let funder = settings
        .funder_address
        .as_deref()
        .filter(|s| !s.is_empty());

    if wallet_addr.is_empty() {
        return Err("Wallet address is empty, please configure in settings".into());
    }

    let http = state.http.read().await;

    // 查询持仓
    let (wallet_positions, funder_positions) = tokio::join!(
        DataClient::get_positions(&http, &wallet_addr),
        async {
            match funder {
                Some(f) => DataClient::get_positions(&http, f).await,
                None => Ok(Vec::new()),
            }
        },
    );

    let mut positions = wallet_positions.unwrap_or_default();
    positions.extend(funder_positions.unwrap_or_default());

    let target = normalize_token(token_id);

    let position = positions.into_iter().find(|p| normalize_token(&p.token_id) == target);

    let size = match position {
        Some(p) => p.size as u64,
        None => {
            tracing::warn!(
                "Chain position not found for token {}, falling back to DB trade record",
                token_id
            );
            match state.db.get_open_trade_by_token(token_id).await {
                Ok(Some(trade)) => trade.size as u64,
                Ok(None) => {
                    return Err(format!(
                        "No position found for token {} (not on chain and no open trade record)",
                        token_id
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "No position found for token {} (DB query failed: {})",
                        token_id, e
                    ));
                }
            }
        }
    };

    if size == 0 {
        return Err(format!("Position size is 0 for token {}", token_id));
    }

    // 获取 best_bid
    let clob = ClobClient::new();
    let book = clob
        .get_orderbook(&http, token_id)
        .await
        .map_err(|e| format!("Failed to get orderbook: {}", e))?;

    let best_bid = book
        .bids
        .first()
        .map(|b| b.price)
        .ok_or("No bid price in orderbook".to_string())?;

    // 构建带 wallet 的 ClobClient 用于卖单签名
    let wallet = get_cached_wallet(state).await?;
    let clob = ClobClient::new().with_wallet(wallet);

    let order_resp = clob
        .create_sell_order(&http, token_id, size, best_bid)
        .await
        .map_err(|e| format!("Sell order failed: {}", e))?;

    let proceeds = size as f64 * best_bid;

    // Update trade record in database
    let exit_timestamp = chrono::Utc::now().to_rfc3339();
    let mut city = String::new();
    let mut entry_price = 0.0;
    let mut trade_size = 0.0;

    match state.db.get_open_trade_by_token(token_id).await {
        Ok(Some(trade)) => {
            city = trade.city.clone();
            entry_price = trade.entry_price;
            trade_size = trade.size;
            let realized_pnl = (best_bid - entry_price) * trade_size;

            if let Err(e) = state.db.update_exit(
                &trade.id,
                best_bid,
                &exit_timestamp,
                realized_pnl,
                "closed",
            ).await {
                tracing::warn!("Sell order succeeded but failed to update trade record: {}", e);
            } else {
                tracing::info!(
                    "Trade record updated: id={} exit_price={:.4} pnl={:.2} reason={}",
                    trade.id, best_bid, realized_pnl, reason
                );
            }
        }
        Ok(None) => {
            tracing::warn!("No open trade record found for token {}, skipping DB update", token_id);
        }
        Err(e) => {
            tracing::warn!("Failed to query trade record for token {}: {}", token_id, e);
        }
    }

    tracing::info!(
        "Position closed: token={} size={} price={:.4} proceeds={:.2} order_id={} reason={}",
        token_id, size, best_bid, proceeds, order_resp.order_id, reason
    );

    let realized_pnl = (best_bid - entry_price) * trade_size;

    Ok(CloseResult {
        token_id: token_id.to_string(),
        city,
        size,
        exit_price: best_bid,
        proceeds,
        realized_pnl,
        reason: reason.to_string(),
    })
}

/// 平仓（实盘）
///
/// 前端传入 token_id，后端查询持仓数量后提交 CLOB 卖单。
/// 成功后更新数据库中对应交易记录的平仓信息。
#[tauri::command]
pub async fn close_position(
    token_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let result = execute_close_position(&state, &token_id, "manual").await?;
    Ok(format!(
        "Closed: token {} size={} proceeds ${:.2}",
        result.token_id, result.size, result.proceeds
    ))
}

/// 查询最近 N 天的每日盈亏数据
#[tauri::command]
pub async fn get_daily_pnl(
    days: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<DailyPnl>, String> {
    let days = days.unwrap_or(30);
    let rows = state
        .db
        .get_daily_pnl(days)
        .await
        .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| DailyPnl {
            date: r.date,
            pnl: r.pnl,
        })
        .collect())
}

/// 按时间范围查询交易记录
///
/// `time_field` = "open"  → 按开仓时间过滤
/// `time_field` = "close" → 按平仓时间过滤
#[tauri::command]
pub async fn get_trade_records(
    start_date: String,
    end_date: String,
    time_field: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<TradeRecord>, String> {
    let time_field = time_field.as_deref().unwrap_or("open");
    let rows = state
        .db
        .get_trades_by_date_range(&start_date, &end_date, time_field)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 清空 trades 表所有记录
#[tauri::command]
pub async fn clear_trades(state: State<'_, AppState>) -> Result<(), String> {
    state.db.clear_trades().await.map_err(|e| e.to_string())?;
    tracing::info!("trades table cleared");
    Ok(())
}

/// 弹出原生保存对话框并将 Excel 文件写入用户选择路径
#[tauri::command]
pub async fn save_excel(data: Vec<u8>, default_name: String) -> Result<Option<String>, String> {
    // 用 PowerShell 弹原生 SaveFileDialog
    let ps_script = format!(
        r#"Add-Type -AssemblyName System.Windows.Forms
$dlg = New-Object System.Windows.Forms.SaveFileDialog
$dlg.Filter = 'Excel Files|*.xlsx'
$dlg.FileName = '{name}'
$dlg.Title = 'Export Trades'
if ($dlg.ShowDialog() -eq 'OK') {{ Write-Output $dlg.FileName }} else {{ Write-Output 'CANCELLED' }}"#,
        name = default_name
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .output()
        .map_err(|e| format!("Failed to open dialog: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout == "CANCELLED" || stdout.is_empty() {
        return Ok(None);
    }
    let path = stdout;
    std::fs::write(&path, &data).map_err(|e| format!("Failed to write file: {}", e))?;
    tracing::info!("Excel exported to {}", path);
    Ok(Some(path))
}

// ── 辅助函数 ──

/// Get or create a cached Wallet with L2 credentials.
///
/// The wallet is cached in AppState and only rebuilt when the private key
/// or funder address changes. This avoids re-deriving L2 API credentials
/// on every trade (which adds an extra HTTP round-trip).
pub async fn get_cached_wallet(state: &AppState) -> Result<Wallet, String> {
    let settings = state
        .db
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))?;

    let private_key = settings
        .private_key
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or("No private key configured")?;

    let funder = settings
        .funder_address
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Check if cached wallet is still valid (same key + funder)
    {
        let cache = state.cached_wallet.lock().await;
        if let Some(ref cached) = *cache {
            if cached.address() == settings.wallet_address
                && cached.funder_address().map(|s| s.to_string()) == funder.clone()
            {
                tracing::debug!("Reusing cached wallet with L2 credentials");
                return Ok(cached.clone());
            }
        }
    }

    // Build new wallet and derive L2 credentials
    tracing::info!("Building new wallet (settings changed or first use)");
    let mut wallet = Wallet::new(private_key, POLYGON_CHAIN_ID)
        .map_err(|e: AppError| format!("Wallet init failed: {}", e))?;

    let http_client = {
        let client = state.http.read().await;
        client.clone()
    };

    match wallet
        .create_or_derive_api_creds(&http_client, CLOB_BASE_URL)
        .await
    {
        Ok(creds) => {
            tracing::info!("L2 API credentials derived successfully");
            wallet.set_creds(creds);
        }
        Err(e) => {
            tracing::error!("Failed to derive L2 creds: {}", e);
            return Err(format!("Failed to derive L2 credentials: {}", e));
        }
    }

    if let Some(ref funder) = funder {
        wallet.set_funder(funder.clone());
    }

    // Cache the wallet
    let mut cache = state.cached_wallet.lock().await;
    *cache = Some(wallet.clone());

    Ok(wallet)
}

// ── Sync helpers ──

/// Normalize token_id for comparison (strip 0x prefix, lowercase)
fn normalize_token(t: &str) -> String {
    let t = t.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        t[2..].to_lowercase()
    } else {
        t.to_lowercase()
    }
}

/// Extract token_id from a Polymarket activity JSON value.
/// The `asset` field can be a plain string or an object with `assetId`.
fn extract_token_id_from_json(act: &serde_json::Value) -> Option<String> {
    // Try "asset" as string
    if let Some(s) = act.get("asset").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Try "asset" as object with nested assetId / asset / tokenId
    if let Some(obj) = act.get("asset").and_then(|v| v.as_object()) {
        for key in ["assetId", "asset", "token_id", "tokenId"] {
            if let Some(id) = obj.get(key).and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    // Try top-level fields
    for key in ["assetId", "token_id", "tokenId"] {
        if let Some(id) = act.get(key).and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

/// Extract timestamp from a JSON value and convert to RFC3339 string.
/// Handles both Unix milliseconds (i64 > 1e12) and seconds.
fn extract_timestamp_from_json(ts: Option<&serde_json::Value>) -> Option<String> {
    let ts = ts?;
    if let Some(n) = ts.as_i64() {
        // Heuristic: > 1e12 → milliseconds
        let dt = if n > 1_000_000_000_000 {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(n)
        } else {
            chrono::DateTime::<chrono::Utc>::from_timestamp(n, 0)
        };
        return dt.map(|d| d.to_rfc3339());
    }
    if let Some(f) = ts.as_f64() {
        let n = f as i64;
        let dt = if n > 1_000_000_000_000 {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(n)
        } else {
            chrono::DateTime::<chrono::Utc>::from_timestamp(n, 0)
        };
        return dt.map(|d| d.to_rfc3339());
    }
    if let Some(s) = ts.as_str() {
        return Some(s.to_string());
    }
    None
}

/// 对账同步：遍历本地所有 status='open' 的订单，逐个与 Polymarket 平台持仓对比。
/// 如果平台持仓已平仓（不在链上持仓列表中或 size=0），则将本地记录更新为 closed，
/// 并尽量从平台数据（活动记录、持仓 realizedPnl）中补充平仓价格、时间和盈亏。
#[tauri::command]
pub async fn sync_open_positions(
    state: State<'_, AppState>,
) -> Result<SyncResult, String> {
    // 1. 获取钱包设置
    let settings = state
        .db
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))?;

    let wallet_addr = settings.wallet_address.clone();
    let funder = settings
        .funder_address
        .as_deref()
        .filter(|s| !s.is_empty());

    if wallet_addr.is_empty() {
        return Err("Wallet address is empty, please configure in settings".into());
    }

    // 2. 查询本地所有未平仓订单
    let open_trades = state
        .db
        .get_all_open_trades()
        .await
        .map_err(|e| format!("Failed to query open trades: {}", e))?;

    let total_open = open_trades.len();
    if total_open == 0 {
        return Ok(SyncResult {
            total_open: 0,
            synced: 0,
            still_open: 0,
            errors: 0,
            details: vec![],
        });
    }

    tracing::info!("sync_open_positions: checking {} open trades", total_open);

    let http = state.http.read().await;

    // 3. 查询 Polymarket 链上持仓（wallet + funder 双地址）
    let (wallet_positions, funder_positions) = tokio::join!(
        DataClient::get_positions(&http, &wallet_addr),
        async {
            match funder {
                Some(f) => DataClient::get_positions(&http, f).await,
                None => Ok(Vec::new()),
            }
        },
    );

    let mut positions: Vec<Position> = wallet_positions.unwrap_or_default();
    positions.extend(funder_positions.unwrap_or_default());

    // 分离 open / closed 持仓
    let mut open_pos_map: HashMap<String, &Position> = HashMap::new();
    let mut closed_pos_map: HashMap<String, &Position> = HashMap::new();

    for p in &positions {
        let key = normalize_token(&p.token_id);
        if p.size > 0.0 {
            open_pos_map.insert(key, p);
        } else {
            closed_pos_map.insert(key, p);
        }
    }

    tracing::info!(
        "sync_open_positions: {} chain positions ({} open, {} closed/zero-size)",
        positions.len(),
        open_pos_map.len(),
        closed_pos_map.len()
    );

    // 4. 查询 Polymarket 活动记录（用于获取卖出价格和时间）
    // SELL 活动：通过 token_id 匹配，price 即平仓价
    // REDEEM 活动：市场结算赎回，asset 为空，需通过 title 匹配 trade.question
    let mut sell_activities: HashMap<String, (f64, String)> = HashMap::new();
    let mut redeem_activities: HashMap<String, (f64, String)> = HashMap::new();

    let addresses: Vec<&str> = [Some(wallet_addr.as_str()), funder]
        .into_iter()
        .flatten()
        .collect();

    for addr in &addresses {
        match DataClient::get_activity(&http, addr).await {
            Ok(activities) => {
                for act in &activities {
                    let act_type = act.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let side = act.get("side").and_then(|v| v.as_str()).unwrap_or("");
                    let ts = extract_timestamp_from_json(act.get("timestamp"))
                        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

                    if act_type.eq_ignore_ascii_case("REDEEM") {
                        // 市场结算赎回：usdcSize / size = exit_price (1.0=赢, 0.0=输)
                        let size = act.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let usdc_size = act.get("usdcSize").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let title = act.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        if size > 0.0 && !title.is_empty() {
                            let exit_price = usdc_size / size;
                            // 保留每个 title 的第一条（最新）赎回记录
                            redeem_activities
                                .entry(title.to_string())
                                .or_insert((exit_price, ts));
                        }
                        continue;
                    }

                    if !side.eq_ignore_ascii_case("SELL") {
                        continue;
                    }
                    let price = match act.get("price").and_then(|v| v.as_f64()) {
                        Some(p) => p,
                        None => continue,
                    };
                    let token_id = match extract_token_id_from_json(act) {
                        Some(t) => t,
                        None => continue,
                    };
                    let key = normalize_token(&token_id);
                    // 保留每个 token 的第一条（最新）卖出记录
                    sell_activities.entry(key).or_insert((price, ts));
                }
            }
            Err(e) => {
                tracing::warn!("Activity API failed for {}: {}", addr, e);
            }
        }
    }

    // 5. 逐个处理本地未平仓订单
    let mut synced = 0;
    let mut still_open = 0;
    let mut errors = 0;
    let mut details = Vec::new();

    for trade in &open_trades {
        let target = normalize_token(&trade.token_id);

        // 5a. 持仓仍在链上 → 跳过
        if open_pos_map.contains_key(&target) {
            still_open += 1;
            details.push(SyncDetail {
                token_id: trade.token_id.clone(),
                city: trade.city.clone(),
                action: "still_open".to_string(),
                message: "Position still open on Polymarket".to_string(),
            });
            continue;
        }

        // 5b. 持仓已平仓，尝试获取平仓数据
        // 优先级 1：SELL 活动记录（通过 token_id 匹配，最准确）
        // 优先级 2：REDEEM 活动记录（市场结算赎回，通过 title 匹配 question）
        // 优先级 3：持仓数据中的 realizedPnl
        // 优先级 4：无数据，仅标记为 closed
        let (exit_price, exit_timestamp, realized_pnl, source) =
            if let Some((sell_price, sell_ts)) = sell_activities.get(&target) {
                let pnl = (sell_price - trade.entry_price) * trade.size;
                (*sell_price, sell_ts.clone(), pnl, "activity")
            } else if let Some((redeem_price, redeem_ts)) = redeem_activities.get(&trade.question) {
                let pnl = (redeem_price - trade.entry_price) * trade.size;
                (*redeem_price, redeem_ts.clone(), pnl, "redeem")
            } else if let Some(pos) = closed_pos_map.get(&target) {
                if pos.realized_pnl != 0.0 && trade.size > 0.0 {
                    let calc_exit = trade.entry_price + pos.realized_pnl / trade.size;
                    (
                        calc_exit,
                        chrono::Utc::now().to_rfc3339(),
                        pos.realized_pnl,
                        "position",
                    )
                } else {
                    (0.0, chrono::Utc::now().to_rfc3339(), 0.0, "unknown")
                }
            } else {
                (0.0, chrono::Utc::now().to_rfc3339(), 0.0, "unknown")
            };

        // 5c. 更新数据库
        match state
            .db
            .update_exit(&trade.id, exit_price, &exit_timestamp, realized_pnl, "closed")
            .await
        {
            Ok(()) => {
                synced += 1;
                let msg = match source {
                    "activity" => format!(
                        "Synced: exit_price={:.4}, pnl={:.2} (from sell activity)",
                        exit_price, realized_pnl
                    ),
                    "redeem" => format!(
                        "Synced: exit_price={:.4}, pnl={:.2} (from market redeem)",
                        exit_price, realized_pnl
                    ),
                    "position" => format!(
                        "Synced: exit_price={:.4}, pnl={:.2} (from position data)",
                        exit_price, realized_pnl
                    ),
                    _ => "Synced: marked as closed (no exit data available)".to_string(),
                };
                tracing::info!(
                    "sync_open_positions: synced trade {} ({}) exit_price={:.4} pnl={:.2} source={}",
                    trade.id, trade.city, exit_price, realized_pnl, source
                );
                details.push(SyncDetail {
                    token_id: trade.token_id.clone(),
                    city: trade.city.clone(),
                    action: "synced".to_string(),
                    message: msg,
                });
            }
            Err(e) => {
                errors += 1;
                tracing::error!(
                    "sync_open_positions: failed to update trade {}: {}",
                    trade.id, e
                );
                details.push(SyncDetail {
                    token_id: trade.token_id.clone(),
                    city: trade.city.clone(),
                    action: "error".to_string(),
                    message: format!("DB update failed: {}", e),
                });
            }
        }
    }

    tracing::info!(
        "sync_open_positions: complete — {} checked, {} synced, {} still open, {} errors",
        total_open, synced, still_open, errors
    );

    Ok(SyncResult {
        total_open,
        synced,
        still_open,
        errors,
        details,
    })
}

// ── Position Monitor commands ──

/// 启动持仓监控器（止损/止盈自动平仓）
///
/// 从 PriceStreamManager 获取价格缓存引用，启动后台监控任务。
/// 监控器每 500ms 检查所有 open 持仓的止损/止盈条件。
/// stop_loss_price / take_profit_price 为全局阈值，对所有持仓生效。
#[tauri::command]
pub async fn start_position_monitor(
    app: tauri::AppHandle,
    stop_loss_price: f64,
    take_profit_price: f64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut monitor_guard = state.position_monitor.lock().await;
    if let Some(ref m) = *monitor_guard {
        if m.is_running() {
            return Ok(false); // 已在运行
        }
    }

    // 获取价格缓存引用
    let cache = {
        let ps = state.price_stream.lock().await;
        ps.cache_handle()
    };

    let monitor = crate::infrastructure::position_monitor::PositionMonitor::start(
        app,
        cache,
        stop_loss_price,
        take_profit_price,
    );
    *monitor_guard = Some(monitor);

    Ok(true)
}

/// 停止持仓监控器
#[tauri::command]
pub async fn stop_position_monitor(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut monitor_guard = state.position_monitor.lock().await;
    if let Some(mut m) = monitor_guard.take() {
        m.stop();
        Ok(true)
    } else {
        Ok(false) // 未在运行
    }
}

/// 查询持仓监控器状态
#[tauri::command]
pub async fn get_position_monitor_status(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let monitor_guard = state.position_monitor.lock().await;
    Ok(monitor_guard
        .as_ref()
        .map(|m| m.is_running())
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lvl(price: f64, size: f64) -> OrderBookEntry {
        OrderBookEntry { price, size }
    }

    #[test]
    fn plan_fill_enough_depth_at_best_ask() {
        let asks = vec![lvl(0.96, 1000.0), lvl(0.97, 500.0)];
        let p = plan_fill(&asks, 450, 0.98);
        assert_eq!(p, FillPlan { size: 450, available: 1500, limit_price: 0.96 });
    }

    #[test]
    fn plan_fill_walks_levels_within_cap() {
        // 卖一 200 股不够，需吃到 0.975 这一档；0.985 超上限不计
        let asks = vec![lvl(0.96, 200.0), lvl(0.975, 300.0), lvl(0.985, 5000.0)];
        let p = plan_fill(&asks, 450, 0.98);
        assert_eq!(p, FillPlan { size: 450, available: 500, limit_price: 0.975 });
    }

    #[test]
    fn plan_fill_partial_when_depth_short() {
        let asks = vec![lvl(0.96, 200.0), lvl(0.97, 100.0), lvl(0.99, 9000.0)];
        let p = plan_fill(&asks, 450, 0.98);
        // 只能吃 300，少下；限价为用到的最深档 0.97
        assert_eq!(p, FillPlan { size: 300, available: 300, limit_price: 0.97 });
        // 低于 50% 目标 → 调用方应跳过
        assert!((p.available as f64) < 450.0 * MIN_FILL_RATIO * 2.0);
        assert!((p.available as f64) >= 450.0 * MIN_FILL_RATIO);
    }

    #[test]
    fn plan_fill_nothing_within_cap() {
        let asks = vec![lvl(0.99, 1000.0)];
        let p = plan_fill(&asks, 450, 0.98);
        assert_eq!(p.size, 0);
        assert_eq!(p.available, 0);
    }

    #[test]
    fn plan_fill_ignores_zero_size_levels() {
        // 旧接口无 size 字段时 size=0：不应把它当作可吃深度
        let asks = vec![lvl(0.96, 0.0), lvl(0.97, 600.0)];
        let p = plan_fill(&asks, 450, 0.98);
        assert_eq!(p, FillPlan { size: 450, available: 600, limit_price: 0.97 });
    }

    #[test]
    fn plan_fill_fractional_sizes_floor() {
        let asks = vec![lvl(0.96, 449.9)];
        let p = plan_fill(&asks, 450, 0.98);
        assert_eq!(p, FillPlan { size: 449, available: 449, limit_price: 0.96 });
    }
}
