use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

use crate::commands::trade_cmd::{execute_close_position, TradeRecord};
use crate::infrastructure::price_stream::PriceSnapshot;
use crate::state::AppState;

/// 监控轮询间隔：500ms
const MONITOR_INTERVAL: Duration = Duration::from_millis(500);

/// open trades 列表刷新间隔：5s
const TRADES_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// 止损确认所需的连续命中次数（500ms × 3 = 1.5s），过滤薄盘口瞬时闪价
const STOP_LOSS_CONFIRM_TICKS: u8 = 3;

/// 价格缓存类型别名
type PriceCache = Arc<RwLock<HashMap<String, PriceSnapshot>>>;

/// 持仓监控器
///
/// 独立后台 tokio 任务，从价格缓存轮询最新 bid/ask，
/// 使用全局止损/止盈阈值检查所有 open 持仓，触发时立即平仓。
///
/// 设计要点：
/// - 500ms 轮询缓存（WebSocket 已实时写入缓存，延迟可忽略）
/// - open trades 列表每 5s 从 DB 刷新一次，避免频繁查库
/// - 正在平仓的 token 加入 in_progress 集合，避免重复触发
/// - 平仓后通过 Tauri event 通知前端刷新
pub struct PositionMonitor {
    /// 关闭信号
    shutdown: Arc<AtomicBool>,
    /// 后台任务句柄
    task: Option<tokio::task::JoinHandle<()>>,
}

impl PositionMonitor {
    /// 启动持仓监控后台任务
    ///
    /// `stop_loss_price` / `take_profit_price` 为全局阈值，对所有 open 持仓生效。
    /// 值为 0.0 时表示不触发该方向。
    pub fn start(
        app: AppHandle,
        cache: PriceCache,
        stop_loss_price: f64,
        take_profit_price: f64,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let task = tokio::spawn(async move {
            run_monitor_loop(app, cache, stop_loss_price, take_profit_price, shutdown_clone).await;
        });

        tracing::info!(
            "Position monitor started (interval={:?}, stop_loss={}, take_profit={})",
            MONITOR_INTERVAL, stop_loss_price, take_profit_price
        );

        Self {
            shutdown,
            task: Some(task),
        }
    }

    /// 停止监控
    pub fn stop(&mut self) {
        if self.task.is_some() {
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(handle) = self.task.take() {
                handle.abort();
            }
            tracing::info!("Position monitor stopped");
        }
    }

    /// 监控是否正在运行
    pub fn is_running(&self) -> bool {
        self.task.is_some() && !self.shutdown.load(Ordering::Relaxed)
    }
}

/// 监控主循环
async fn run_monitor_loop(
    app: AppHandle,
    cache: PriceCache,
    stop_loss_price: f64,
    take_profit_price: f64,
    shutdown: Arc<AtomicBool>,
) {
    // open trades 缓存
    let mut open_trades: Vec<TradeRecord> = Vec::new();
    // 正在平仓的 token_id 集合（避免重复触发）
    let mut closing_tokens: HashSet<String> = HashSet::new();
    // 止损连续命中计数：token_id -> 连续满足 bid <= stop_loss 的 tick 数
    let mut sl_hits: HashMap<String, u8> = HashMap::new();

    let mut last_trades_refresh = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(MONITOR_INTERVAL);

    // 初次加载 open trades
    refresh_open_trades(&app, &mut open_trades).await;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("Position monitor: shutdown signal received, exiting");
            return;
        }

        ticker.tick().await;

        // 定期刷新 open trades 列表
        if last_trades_refresh.elapsed() >= TRADES_REFRESH_INTERVAL {
            refresh_open_trades(&app, &mut open_trades).await;
            last_trades_refresh = tokio::time::Instant::now();
        }

        // 没有持仓或没有价格数据，跳过
        if open_trades.is_empty() {
            continue;
        }

        // 读取价格缓存（只持有读锁很短时间）
        let snapshots: Vec<(String, f64, f64)> = {
            let guard = cache.read().await;
            open_trades
                .iter()
                .filter_map(|t| {
                    let snap = guard.get(&t.token_id)?;
                    Some((t.token_id.clone(), snap.bid, snap.ask))
                })
                .collect()
        };

        if snapshots.is_empty() {
            continue;
        }

        // 构建快速查找 map: token_id -> (bid, ask)
        let price_map: HashMap<&str, (f64, f64)> = snapshots
            .iter()
            .map(|(id, bid, ask)| (id.as_str(), (*bid, *ask)))
            .collect();

        // 检查每个 open trade
        let mut to_close: Vec<(&TradeRecord, &str)> = Vec::new();

        for trade in &open_trades {
            // 跳过正在平仓的 token
            if closing_tokens.contains(&trade.token_id) {
                continue;
            }

            let (bid, ask) = match price_map.get(trade.token_id.as_str()) {
                Some(&(b, a)) => (b, a),
                None => continue,
            };

            // 跳过无效价格
            if !bid.is_finite() || !ask.is_finite() {
                continue;
            }

            // 止损检查：仅看 bid（实际卖出成交价）。
            // 复盘：denver 88-89°F 崩盘时 bid 0.867 / ask 0.959，旧规则要求 ask 也 <= 止损价，
            // 盘口拉宽导致止损迟迟不触发，最终在 0.034 才卖出。
            // 为过滤薄盘口闪价，要求连续 STOP_LOSS_CONFIRM_TICKS 个 tick 命中才触发。
            if stop_loss_price > 0.0 && bid > 0.0 && bid <= stop_loss_price {
                let hits = sl_hits.entry(trade.token_id.clone()).or_insert(0);
                *hits = hits.saturating_add(1);
                if *hits >= STOP_LOSS_CONFIRM_TICKS {
                    to_close.push((trade, "stop_loss"));
                } else {
                    tracing::debug!(
                        "Position monitor: stop_loss candidate token={} bid={:.4} ask={:.4} hits={}/{}",
                        trade.token_id, bid, ask, hits, STOP_LOSS_CONFIRM_TICKS
                    );
                }
                continue;
            } else {
                sl_hits.remove(&trade.token_id);
            }

            // 止盈检查：bid >= take_profit AND ask >= take_profit（阈值为 0 时不触发）
            if take_profit_price > 0.0 && bid >= take_profit_price && ask >= take_profit_price {
                to_close.push((trade, "take_profit"));
            }
        }

        if to_close.is_empty() {
            continue;
        }

        // 执行平仓
        for (trade, reason) in to_close {
            let token_id = trade.token_id.clone();
            let city = trade.city.clone();
            let entry_price = trade.entry_price;

            tracing::info!(
                "Position monitor: triggering {} for token={} city={} entry={:.4} bid/ask in threshold",
                reason, token_id, city, entry_price
            );

            // 标记为正在平仓
            closing_tokens.insert(token_id.clone());

            // 获取 AppState 并执行平仓
            let state = app.state::<AppState>();
            match execute_close_position(&state, &token_id, reason).await {
                Ok(result) => {
                    tracing::info!(
                        "Position monitor: {} closed token={} size={} exit={:.4} pnl={:.2}",
                        reason, result.token_id, result.size, result.exit_price, result.realized_pnl
                    );

                    // 通知前端
                    let _ = app.emit("position://auto-closed", &result);
                }
                Err(e) => {
                    tracing::error!(
                        "Position monitor: failed to close token={} ({}): {}",
                        token_id, reason, e
                    );
                }
            }

            // 从正在平仓集合中移除（无论成功失败），并重置止损计数
            closing_tokens.remove(&token_id);
            sl_hits.remove(&token_id);
        }

        // 平仓后立即刷新 open trades 列表
        refresh_open_trades(&app, &mut open_trades).await;
        last_trades_refresh = tokio::time::Instant::now();
    }
}

/// 从数据库刷新 open trades 列表
async fn refresh_open_trades(app: &AppHandle, open_trades: &mut Vec<TradeRecord>) {
    let state = app.state::<AppState>();
    match state.db.get_all_open_trades().await {
        Ok(trades) => {
            if trades.len() != open_trades.len() {
                tracing::info!(
                    "Position monitor: loaded {} open trades (was {})",
                    trades.len(),
                    open_trades.len()
                );
            }
            *open_trades = trades;
        }
        Err(e) => {
            tracing::warn!("Position monitor: failed to refresh open trades: {}", e);
        }
    }
}
