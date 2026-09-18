use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::infrastructure::proxy::detect_system_proxy;

/// Polymarket CLOB WebSocket endpoint
const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// 单个 token 的最新价格快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub token_id: String,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub timestamp: f64,
}

/// 一个城市的 WS 连接句柄
struct CityStream {
    /// 控制连接关闭的信号
    shutdown: Arc<AtomicBool>,
    /// 后台任务句柄
    task: JoinHandle<()>,
}

/// 价格流管理器
///
/// 按城市分组管理多条 WS 连接，每条连接独立订阅该城市的 token_ids。
/// 所有收到的价格更新写入共享缓存并通过 `price://update` 事件推送给前端。
pub struct PriceStreamManager {
    /// city -> CityStream
    streams: HashMap<String, CityStream>,
    /// token_id -> PriceSnapshot（共享缓存）
    cache: Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    /// 全局 flush 任务：接收所有 WS 批次的价格更新，每 2 秒统一 emit
    flush_task: Option<JoinHandle<()>>,
}

impl PriceStreamManager {
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            cache: Arc::new(RwLock::new(HashMap::new())),
            flush_task: None,
        }
    }

    /// 启动按城市分组的价格流
    ///
    /// 将城市分批合并，每批一条 WS 连接（最多 BATCH_SIZE 个城市的 token），
    /// 减少 WS 连接数避免系统资源耗尽。
    /// 已存在的连接会先关闭再重建。
    pub async fn start(
        &mut self,
        app: AppHandle,
        city_groups: Vec<CityTokenGroup>,
        proxy_url: Option<String>,
    ) {
        // 先停止所有旧连接
        self.stop_all().await;

        let cache = self.cache.clone();

        // 全局 IPC 批量 flush channel：所有 WS 批次通过 sender 发送价格更新，
        // 单一 flush 任务每 2 秒统一 emit price://update-batch，
        // 将 N 次独立 emit 合并为 1 次，大幅减少 IPC 调用次数。
        let (price_tx, mut price_rx) = mpsc::unbounded_channel::<PriceSnapshot>();
        let app_flush = app.clone();
        let flush_task = tokio::spawn(async move {
            let mut buffer: Vec<PriceSnapshot> = Vec::new();
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tokio::select! {
                    msg = price_rx.recv() => match msg {
                        Some(snapshot) => buffer.push(snapshot),
                        None => {
                            // 所有 sender 已 drop，flush 残留后退出
                            if !buffer.is_empty() {
                                let _ = app_flush.emit("price://update-batch", &buffer);
                            }
                            break;
                        }
                    },
                    _ = ticker.tick() => {
                        if !buffer.is_empty() {
                            let _ = app_flush.emit("price://update-batch", &std::mem::take(&mut buffer));
                        }
                    }
                }
            }
        });
        self.flush_task = Some(flush_task);

        // 合并城市分组：每 BATCH_SIZE 个城市合并为一条 WS 连接
        const BATCH_SIZE: usize = 10;
        let batches: Vec<Vec<CityTokenGroup>> = city_groups
            .chunks(BATCH_SIZE)
            .map(|chunk| chunk.to_vec())
            .collect();

        for (batch_idx, batch) in batches.into_iter().enumerate() {
            // 收集这批所有 token_ids
            let all_token_ids: Vec<String> = batch
                .iter()
                .flat_map(|g| g.token_ids.clone())
                .collect();
            let batch_label = format!("batch-{}", batch_idx);
            let app_clone = app.clone();
            let cache_clone = cache.clone();
            let proxy = proxy_url.clone();
            let tx_clone = price_tx.clone();

            let shutdown = Arc::new(AtomicBool::new(false));
            let shutdown_clone = shutdown.clone();
            let city_label = batch_label.clone();

            let task = tokio::spawn(async move {
                run_city_ws(
                    app_clone,
                    cache_clone,
                    city_label,
                    all_token_ids,
                    proxy,
                    shutdown_clone,
                    tx_clone,
                )
                .await;
            });

            self.streams.insert(
                batch_label,
                CityStream {
                    shutdown,
                    task,
                },
            );
        }

        let count = self.streams.len();
        tracing::info!("Price stream started: {} batch connections ({} cities merged)", count, city_groups.len());
        let _ = app.emit(
            "ws://status",
            serde_json::json!({ "connected": true, "cities": count }),
        );
    }

    /// 停止所有 WS 连接
    pub async fn stop_all(&mut self) {
        if self.streams.is_empty() && self.flush_task.is_none() {
            return;
        }

        let count = self.streams.len();
        tracing::info!("Stopping {} city WS connections...", count);

        for (_, stream) in self.streams.drain() {
            stream.shutdown.store(true, Ordering::Relaxed);
            stream.task.abort();
        }

        // 停止全局 flush 任务
        if let Some(handle) = self.flush_task.take() {
            handle.abort();
        }

        // 清空缓存，避免跨天后旧 token_id 价格条目永久驻留
        self.cache.write().await.clear();

        tracing::info!("All {} city WS connections stopped", count);
    }

    /// 获取所有缓存的价格快照
    pub async fn get_cached(&self) -> Vec<PriceSnapshot> {
        let cache = self.cache.read().await;
        cache.values().cloned().collect()
    }

    /// 获取价格缓存的共享引用（供 position_monitor 直接读取，避免全量 clone）
    pub fn cache_handle(&self) -> Arc<RwLock<HashMap<String, PriceSnapshot>>> {
        Arc::clone(&self.cache)
    }

    /// REST backfill：通过 HTTP 批量查询 token 的 best bid/ask 并写入缓存
    ///
    /// 用于 WS 重连后补齐可能缺失的价格数据。
    /// 使用调用方传入的共享 HTTP 客户端，避免每次 backfill 新建 reqwest::Client。
    pub async fn backfill(&self, token_ids: &[String], client: &reqwest::Client) {
        if token_ids.is_empty() {
            return;
        }

        tracing::info!("Backfilling {} tokens via REST...", token_ids.len());

        // Polymarket CLOB REST: /book?token_id=xxx
        // 批量限制，每 50 个一组
        for (chunk_idx, chunk) in token_ids.chunks(50).enumerate() {
            let mut updates = Vec::new();

            for tid in chunk {
                let url = format!("https://clob.polymarket.com/book?token_id={}", tid);
                match client.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => match resp.json::<RestBookResponse>().await {
                        Ok(book) => {
                            let bid = best_price(&book.buys, true);
                            let ask = best_price(&book.sells, false);
                            if bid.is_finite() || ask.is_finite() {
                                let mid = if bid.is_finite() && ask.is_finite() {
                                    (bid + ask) / 2.0
                                } else if bid.is_finite() {
                                    bid
                                } else {
                                    ask
                                };
                                let ts = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                                updates.push(PriceSnapshot {
                                    token_id: tid.clone(),
                                    bid,
                                    ask,
                                    mid,
                                    timestamp: ts,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Backfill: failed to parse book for {}: {}", tid, e);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Backfill: request failed for {}: {}", tid, e);
                    }
                    _ => {
                        tracing::warn!("Backfill: non-200 for {}", tid);
                    }
                }
            }

            if !updates.is_empty() {
                {
                    let mut cache = self.cache.write().await;
                    for u in &updates {
                        cache.insert(u.token_id.clone(), u.clone());
                    }
                }
                tracing::debug!("Backfill: chunk {} - {} prices updated", chunk_idx, updates.len());
            }

            // 小延迟避免 rate limit
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        tracing::info!("Backfill complete ({} tokens processed)", token_ids.len());
    }
}

/// 城市token分组（前端传入）
#[derive(Debug, Clone, Deserialize)]
pub struct CityTokenGroup {
    /// 城市名（日志标识用，当前未在逻辑中读取）
    #[allow(dead_code)]
    pub city: String,
    pub token_ids: Vec<String>,
}

/// 运行单个城市的 WS 连接
///
/// - 连接 CLOB WS endpoint
/// - 发送 market channel 订阅消息（assets_ids = token_ids）
/// - 循环读取消息，解析 book / price_change
/// - 提取 best_bid / best_ask，计算 mid
/// - 写入共享缓存 + 通过 mpsc channel 发送给全局 flush 任务统一 emit
/// - 每 30 秒发送 WebSocket Ping 做心跳保活，防止代理空闲断连
/// - 每 60 秒对超过 90 秒未更新的 token 做 REST 刷新兜底
/// - 收到 shutdown 信号时退出
async fn run_city_ws(
    app: AppHandle,
    cache: Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    city: String,
    token_ids: Vec<String>,
    proxy_url: Option<String>,
    shutdown: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<PriceSnapshot>,
) {
    tracing::info!("[{}] WS task starting ({} tokens)", city, token_ids.len());

    // 用于 REST 兜底的 HTTP 客户端（与 WS 连接同生命周期，复用代理配置）
    let rest_client = {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(1);
        // 如果 WS 连接使用了代理，REST 客户端也走同一代理
        if let Some(ref proxy_str) = proxy_url {
            if !proxy_str.is_empty() {
                if let Ok(proxy) = reqwest::Proxy::all(proxy_str) {
                    builder = builder.proxy(proxy);
                    tracing::debug!("[{}] REST client using proxy: {}", city, proxy_str);
                }
            }
        }
        builder.build().unwrap_or_else(|_| reqwest::Client::new())
    };

    // 重连退避策略
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("[{}] WS task: shutdown signal received, exiting", city);
            return;
        }

        tracing::info!("[{}] Connecting to CLOB WS...", city);

        let ws_stream = match connect_ws_with_proxy(&proxy_url).await {
            Ok(s) => {
                tracing::info!("[{}] WS connected", city);
                backoff = std::time::Duration::from_secs(1);
                s
            }
            Err(e) => {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                tracing::warn!(
                    "[{}] WS connect failed: {} (retry in {:?})",
                    city,
                    e,
                    backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        let _ = app.emit(
            "ws://status",
            serde_json::json!({ "connected": true, "city": &city }),
        );

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        // 发送订阅消息
        let subscribe_msg = serde_json::json!({
            "type": "market",
            "assets_ids": token_ids,
        });
        let subscribe_str = serde_json::to_string(&subscribe_msg).unwrap_or_default();

        match ws_sender.send(Message::Text(subscribe_str)).await {
            Ok(()) => tracing::info!("[{}] Subscribed to {} tokens", city, token_ids.len()),
            Err(e) => {
                tracing::warn!("[{}] Failed to send subscribe: {}", city, e);
                continue;
            }
        }

        // 心跳定时器：每 30 秒发一次 Ping，防止代理空闲断连
        let mut last_ping = tokio::time::Instant::now();
        let ping_interval = std::time::Duration::from_secs(30);

        // REST 兜底定时器：每 60 秒检查一次，对超过 90 秒未更新的 token 做 REST 刷新
        let mut last_rest_refresh = tokio::time::Instant::now();
        let rest_interval = std::time::Duration::from_secs(60);
        let rest_stale_threshold = std::time::Duration::from_secs(90);

        // 消息循环
        loop {
            if shutdown.load(Ordering::Relaxed) {
                tracing::info!("[{}] WS task: shutdown during message loop", city);
                let _ = ws_sender.close().await;
                return;
            }

            // 心跳：超过间隔发送 Ping
            let now = tokio::time::Instant::now();
            if now.duration_since(last_ping) >= ping_interval {
                match ws_sender.send(Message::Ping(Vec::new())).await {
                    Ok(()) => {
                        last_ping = now;
                    }
                    Err(e) => {
                        tracing::warn!("[{}] Heartbeat Ping failed: {}, forcing reconnect", city, e);
                        break;
                    }
                }
            }

            // REST 兜底：检查并刷新过期 token
            if now.duration_since(last_rest_refresh) >= rest_interval {
                last_rest_refresh = now;
                let stale_ids = collect_stale_tokens(&cache, &token_ids, rest_stale_threshold).await;
                if !stale_ids.is_empty() {
                    tracing::debug!("[{}] REST refresh: {} stale tokens (>{}s)", city, stale_ids.len(), rest_stale_threshold.as_secs());
                    refresh_tokens_via_rest(&cache, &rest_client, &stale_ids, &city, &tx).await;
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    // 每 500ms 检查一次 shutdown 信号
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("[{}] WS task: shutdown during select", city);
                        let _ = ws_sender.close().await;
                        return;
                    }
                }
                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            handle_ws_message(&cache, &city, &text, &tx).await;
                        }
                        Some(Ok(Message::Binary(data))) => {
                            if let Ok(text) = String::from_utf8(data) {
                                handle_ws_message(&cache, &city, &text, &tx).await;
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = ws_sender.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(reason))) => {
                            tracing::info!("[{}] WS closed by server: {:?}", city, reason);
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::warn!("[{}] WS error: {}", city, e);
                            break;
                        }
                        None => {
                            tracing::info!("[{}] WS stream ended", city);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        // 连接断开，通知前端
        let _ = app.emit(
            "ws://status",
            serde_json::json!({ "connected": false, "city": &city }),
        );

        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        tracing::info!("[{}] Reconnecting in {:?}...", city, backoff);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);

        // 重连成功后触发 REST backfill
        let _ = app.emit("ws://reconnected", serde_json::json!({ "city": &city }));
    }
}

/// 收集缓存中超过阈值未更新的 token_id（用于 REST 兜底刷新）
async fn collect_stale_tokens(
    cache: &Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    token_ids: &[String],
    threshold: std::time::Duration,
) -> Vec<String> {
    let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let threshold_secs = threshold.as_secs() as f64;
    let guard = cache.read().await;
    token_ids
        .iter()
        .filter(|tid| {
            match guard.get(*tid) {
                Some(snap) => (now - snap.timestamp) > threshold_secs,
                None => true, // 缓存中没有 = 最过期
            }
        })
        .cloned()
        .collect()
}

/// 通过 REST API 批量刷新 token 价格并写入缓存 + 通过 channel 发送给全局 flush 任务
async fn refresh_tokens_via_rest(
    cache: &Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    client: &reqwest::Client,
    token_ids: &[String],
    city: &str,
    tx: &mpsc::UnboundedSender<PriceSnapshot>,
) {
    let now_ts = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let mut updates = Vec::new();

    for tid in token_ids {
        let url = format!("https://clob.polymarket.com/book?token_id={}", tid);
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<RestBookResponse>().await {
                    Ok(book) => {
                        let bid = best_price(&book.buys, true);
                        let ask = best_price(&book.sells, false);
                        if bid.is_finite() || ask.is_finite() {
                            let mid = if bid.is_finite() && ask.is_finite() {
                                (bid + ask) / 2.0
                            } else if bid.is_finite() {
                                bid
                            } else {
                                ask
                            };
                            updates.push(PriceSnapshot {
                                token_id: tid.clone(),
                                bid,
                                ask,
                                mid,
                                timestamp: now_ts,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::debug!("[{}] REST refresh parse failed for {}: {}", city, tid, e);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("[{}] REST refresh request failed for {}: {}", city, tid, e);
            }
            Ok(resp) => {
                tracing::debug!("[{}] REST refresh non-200 for {}: status={}", city, tid, resp.status());
            }
        }
        // 小延迟避免 rate limit
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    if updates.is_empty() {
        tracing::debug!("[{}] REST refresh: 0/{} tokens updated (all requests failed or no valid prices)", city, token_ids.len());
        return;
    }

    // 写入缓存
    {
        let mut guard = cache.write().await;
        for u in &updates {
            guard.insert(u.token_id.clone(), u.clone());
        }
    }

    // 通过 channel 发送给全局 flush 任务
    let update_count = updates.len();
    for u in updates {
        let _ = tx.send(u);
    }
    tracing::debug!("[{}] REST refresh: {} tokens updated", city, update_count);
}

/// 解析 WS 消息并更新缓存 + 通过 channel 发送给全局 flush 任务
///
/// 不再直接 emit 前端，由全局 flush 任务在 2 秒批量窗口内统一 emit。
async fn handle_ws_message(
    cache: &Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    city: &str,
    text: &str,
    tx: &mpsc::UnboundedSender<PriceSnapshot>,
) {
    // CLOB WS 可能一次发多条 JSON（JSON array）或单条
    let messages: Vec<serde_json::Value> = if text.trim_start().starts_with('[') {
        serde_json::from_str(text).unwrap_or_default()
    } else {
        serde_json::from_str(text)
            .map(|v| vec![v])
            .unwrap_or_default()
    };

    let now_ts = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let mut updates: Vec<PriceSnapshot> = Vec::new();

    for msg in &messages {
        let event_type = msg.get("event_type").and_then(|v| v.as_str());

        match event_type {
            Some("book") => {
                if let Some(asset_id) = msg.get("asset_id").and_then(|v| v.as_str()) {
                    // CLOB WS book 消息字段为 "bids" 和 "asks"
                    let bids = msg.get("bids").and_then(|v| v.as_array());
                    let asks = msg.get("asks").and_then(|v| v.as_array());

                    let bid = best_level_price(bids, true);
                    let ask = best_level_price(asks, false);

                    if bid.is_finite() || ask.is_finite() {
                        let mid = mid_price(bid, ask);
                        tracing::debug!(
                            "[{}] book asset={} bid={} ask={} (bid_levels={} ask_levels={})",
                            city, asset_id, bid, ask,
                            bids.map(|v| v.len()).unwrap_or(0),
                            asks.map(|v| v.len()).unwrap_or(0)
                        );
                        updates.push(PriceSnapshot {
                            token_id: asset_id.to_string(),
                            bid,
                            ask,
                            mid,
                            timestamp: now_ts,
                        });
                    }
                }
            }
            Some("price_change") => {
                // price_change 消息格式：
                // { "event_type":"price_change", "price_changes": [ { "asset_id":"...", "best_bid":"0.26", "best_ask":"0.3", ... }, ... ] }
                let price_changes = msg.get("price_changes").and_then(|v| v.as_array());
                if let Some(changes) = price_changes {
                    for change in changes {
                        let asset_id = change.get("asset_id").and_then(|v| v.as_str());
                        if let Some(asset_id) = asset_id {
                            // best_bid/best_ask 可能是字符串("0.26")或数字(0.26)，统一兼容
                            let bid = change
                                .get("best_bid")
                                .map(json_price)
                                .unwrap_or(f64::NAN);
                            let ask = change
                                .get("best_ask")
                                .map(json_price)
                                .unwrap_or(f64::NAN);

                            tracing::debug!(
                                "[{}] price_change asset={} bid={:?} ask={:?} raw_bid={:?} raw_ask={:?}",
                                city, asset_id, bid, ask,
                                change.get("best_bid"),
                                change.get("best_ask")
                            );

                            // 从缓存读取旧值做 fallback（仅在 bid/ask 缺失时）
                            let (bid, ask) = if !bid.is_finite() || !ask.is_finite() {
                                let cache_guard = cache.read().await;
                                if let Some(old) = cache_guard.get(asset_id) {
                                    (
                                        if bid.is_finite() { bid } else { old.bid },
                                        if ask.is_finite() { ask } else { old.ask },
                                    )
                                } else {
                                    (bid, ask)
                                }
                            } else {
                                (bid, ask)
                            };

                            if bid.is_finite() || ask.is_finite() {
                                let mid = mid_price(bid, ask);
                                updates.push(PriceSnapshot {
                                    token_id: asset_id.to_string(),
                                    bid,
                                    ask,
                                    mid,
                                    timestamp: now_ts,
                                });
                            }
                        }
                    }
                }
            }
            Some("last_trade_price") | Some("tick_size_change") => {
                // 忽略不相关的事件类型
            }
            _ => {
                if event_type.is_none() {
                    tracing::debug!(
                        "[{}] Unknown WS message: {}...",
                        city,
                        &text[..text.len().min(200)]
                    );
                }
            }
        }
    }

    if updates.is_empty() {
        return;
    }

    // 写入缓存 + 通过 channel 发送给全局 flush 任务
    // 消费 updates 而非引用遍历，避免每个元素 clone 一次
    {
        let mut cache_guard = cache.write().await;
        for u in &updates {
            cache_guard.insert(u.token_id.clone(), u.clone());
        }
    }
    for u in updates {
        let _ = tx.send(u);
    }
}

/// 计算 mid price：bid/ask 都有效取均值，否则取有效的那个
#[inline]
fn mid_price(bid: f64, ask: f64) -> f64 {
    if bid.is_finite() && ask.is_finite() {
        (bid + ask) / 2.0
    } else if bid.is_finite() {
        bid
    } else {
        ask
    }
}

/// 从 JSON 节点解析价格：兼容字符串("0.26")与数字(0.26)，解析失败返回 NaN
#[inline]
fn json_price(v: &serde_json::Value) -> f64 {
    v.as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .unwrap_or(f64::NAN)
}

/// 从盘口 levels 数组中取最优价格
///
/// book 消息的 bids/asks 是 `[{price: "0.95", size: "100"}, ...]`
///
/// 注意：实测 CLOB 返回的 bids/asks **均按 price 升序**（最低价在前），
/// 因此不能盲目取 first()：bids 的最优买价是**最高价**，asks 的最优卖价是**最低价**。
/// 本函数直接扫描数组取极值，不依赖排序；`take_highest=true` 取最大值（bids 侧），
/// `false` 取最小值（asks 侧）。只接受 (0, 1] 区间内的合法价格，过滤脏数据。
fn best_level_price(levels: Option<&Vec<serde_json::Value>>, take_highest: bool) -> f64 {
    let levels = match levels {
        Some(l) if !l.is_empty() => l,
        _ => return f64::NAN,
    };

    let mut best = f64::NAN;
    for level in levels {
        let price = level
            .get("price")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(f64::NAN);
        if !(price > 0.0 && price <= 1.0) {
            continue;
        }
        if best.is_nan() || (take_highest && price > best) || (!take_highest && price < best) {
            best = price;
        }
    }
    best
}

/// 从 REST /book 响应的 bids/asks 中取最优价格
///
/// 与 `best_level_price` 同理：bids 取最高价、asks 取最低价，不依赖排序。
fn best_price(levels: &[RestBookLevel], take_highest: bool) -> f64 {
    let mut best = f64::NAN;
    for level in levels {
        match level.price.parse::<f64>() {
            Ok(p) if p > 0.0 && p <= 1.0 => {
                if best.is_nan() || (take_highest && p > best) || (!take_highest && p < best) {
                    best = p;
                }
            }
            _ => {}
        }
    }
    best
}

/// WS 流类型别名
type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 连接 CLOB WS，支持 HTTP 代理隧道
///
/// 统一返回 `WsStream` 类型：
/// - 无代理：`connect_async` 直接建立 wss 连接
/// - 有代理：先 TCP 连代理 → CONNECT 隧道 → `client_async_tls` 升级 TLS + WS
async fn connect_ws_with_proxy(proxy_url: &Option<String>) -> Result<WsStream, Box<dyn std::error::Error + Send + Sync>> {
    let effective_proxy = proxy_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(detect_system_proxy);

    match effective_proxy {
        Some(proxy_str) => {
            tracing::info!("Connecting WS via proxy: {}", proxy_str);
            connect_ws_via_proxy(&proxy_str).await
        }
        None => {
            tracing::info!("Connecting WS direct (no proxy)");
            let (stream, _response) = tokio_tungstenite::connect_async(WS_URL).await?;
            Ok(stream)
        }
    }
}

/// 通过 HTTP CONNECT 隧道连接 WS（用于代理场景）
///
/// 流程：TCP 连代理 → 发 CONNECT → 读取 200 → 在隧道上 `client_async_tls` 升级 TLS+WS
async fn connect_ws_via_proxy(proxy_url: &str) -> Result<WsStream, Box<dyn std::error::Error + Send + Sync>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let proxy = url::Url::parse(proxy_url)
        .map_err(|e| format!("Invalid proxy URL '{}': {}", proxy_url, e))?;

    let proxy_host = proxy.host_str().ok_or("Proxy URL missing host")?;
    let proxy_port = proxy.port().unwrap_or(if proxy.scheme() == "https" { 443 } else { 8080 });

    let ws_url = url::Url::parse(WS_URL)?;
    let ws_host = ws_url.host_str().ok_or("WS URL missing host")?;
    let ws_port = ws_url.port_or_known_default().unwrap_or(443);

    tracing::debug!("Proxy: {}:{}, Target: {}:{}", proxy_host, proxy_port, ws_host, ws_port);

    // 1. TCP 连接到代理服务器
    let tcp_stream = tokio::net::TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|e| format!("Failed to connect to proxy {}:{}: {}", proxy_host, proxy_port, e))?;

    // 2. 发送 CONNECT 隧道请求
    let connect_request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
        ws_host, ws_port, ws_host, ws_port
    );

    let mut tcp_stream = tcp_stream;
    tcp_stream.write_all(connect_request.as_bytes()).await?;

    // 3. 读取代理响应
    let mut buf = [0u8; 1024];
    let n = tcp_stream.read(&mut buf).await?;
    let response = String::from_utf8_lossy(&buf[..n]);

    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "Proxy CONNECT failed: {}",
            response.lines().next().unwrap_or("unknown")
        )
        .into());
    }

    tracing::debug!("Proxy CONNECT established, upgrading to TLS + WS...");

    // 4. 在隧道上用 client_async_tls 升级 TLS + WebSocket
    //    返回 MaybeTlsStream<TcpStream> 类型，与 connect_async 一致
    let request = WS_URL.into_client_request()?;

    match tokio_tungstenite::client_async_tls(request, tcp_stream).await {
        Ok((stream, _response)) => Ok(stream),
        Err(e) => {
            use tokio_tungstenite::tungstenite::Error as WsError;
            match &e {
                WsError::Http(resp) => {
                    tracing::warn!("WS handshake rejected: HTTP {}", resp.status());
                }
                other => {
                    tracing::warn!("WS handshake error: {}", other);
                }
            }
            Err(format!("WS handshake failed: {}", e).into())
        }
    }
}

// ── REST book response types ──

#[derive(Debug, Deserialize)]
struct RestBookResponse {
    #[serde(default, rename = "bids")]
    buys: Vec<RestBookLevel>,
    #[serde(default, rename = "asks")]
    sells: Vec<RestBookLevel>,
}

#[derive(Debug, Deserialize)]
struct RestBookLevel {
    price: String,
    #[allow(dead_code)]
    size: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 用真实抓包的 book 数据形态验证：bids/asks 均为升序，
    /// bids 必须取最高价（最优买价），asks 取最低价（最优卖价）。
    #[test]
    fn best_level_price_respects_direction() {
        // 真实 WS 帧样本（AMS 21C NO）：bids 0.001..0.996 升序
        let bids = vec![
            json!({"price": "0.001", "size": "2031.55"}),
            json!({"price": "0.4", "size": "50"}),
            json!({"price": "0.996", "size": "12.34"}),
        ];
        let asks = vec![
            json!({"price": "0.004", "size": "100"}),
            json!({"price": "0.01", "size": "200"}),
            json!({"price": "0.999", "size": "10"}),
        ];
        assert_eq!(best_level_price(Some(&bids), true), 0.996);
        assert_eq!(best_level_price(Some(&asks), false), 0.004);
        // 空盘口返回 NaN
        assert!(best_level_price(Some(&Vec::new()), true).is_nan());
        assert!(best_level_price(None, true).is_nan());
        // 脏数据（0 / >1 / 非数字）被过滤
        let dirty = vec![
            json!({"price": "0", "size": "1"}),
            json!({"price": "1.5", "size": "1"}),
            json!({"price": "abc", "size": "1"}),
            json!({"price": "0.5", "size": "1"}),
        ];
        assert_eq!(best_level_price(Some(&dirty), true), 0.5);
    }

    /// REST /book 同一规则：buys 侧取最高价、sells 侧取最低价
    #[test]
    fn best_price_respects_direction() {
        let levels = vec![
            RestBookLevel { price: "0.001".into(), size: "1".into() },
            RestBookLevel { price: "0.994".into(), size: "1".into() },
            RestBookLevel { price: "0.996".into(), size: "1".into() },
        ];
        assert_eq!(best_price(&levels, true), 0.996);
        assert_eq!(best_price(&levels, false), 0.001);
    }

    /// price_change 的 best_bid/best_ask 可能是字符串或数字，需全部兼容
    #[test]
    fn json_price_handles_str_and_number() {
        assert_eq!(json_price(&json!("0.26")), 0.26);
        assert_eq!(json_price(&json!(0.26)), 0.26);
        assert_eq!(json_price(&json!(1)), 1.0);
        assert!(json_price(&json!(null)).is_nan());
        assert!(json_price(&json!("abc")).is_nan());
        assert!(json_price(&json!(true)).is_nan());
    }
}
