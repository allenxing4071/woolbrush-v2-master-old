use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::infrastructure::price_stream::{CityTokenGroup, PriceSnapshot};
use crate::infrastructure::temperature::{load_city_markets, local_date, CityTempMarkets, TempThreshold};
use crate::state::AppState;

// ── Types matching frontend TypeScript interfaces ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPrice {
    pub token_id: String,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub timestamp: f64,
}

/// 从档位 label 中检测温度单位
///
/// label 格式如 "21°C"、"70°F"、"30°C or higher"
/// 返回 "°C" 或 "°F"，无法识别时返回 None
fn detect_unit_from_thresholds(thresholds: &[TempThreshold]) -> Option<&'static str> {
    for t in thresholds {
        if t.label.contains("°F") {
            return Some("°F");
        }
        if t.label.contains("°C") {
            return Some("°C");
        }
    }
    None
}

// ── Market Commands ──

/// 流式加载气温市场数据
///
/// 从 DB 读取城市列表，逐城市根据当地日期查询 Gamma API：
/// 1. 根据城市时区 (iana_tz) 计算当地当前日期
/// 2. 构造 slug: highest-temperature-in-{city}-on-{month}-{day}-{year}
/// 3. 调用 Gamma API /events?slug=... 获取完整事件（含 markets 数组）
/// 4. 解析 markets 提取温度档位（token_id、价格等）
/// 5. 通过 temperature://city-loaded 事件逐城市推送给前端
///
/// 关键：使用当地日期而非 UTC 日期，确保查到的是当地当天结算的市场。
/// 加载完成后自动启动 WS 价格流（按城市分组订阅）。
///
/// `city_slugs`：可选的城市过滤列表。None = 加载全部城市；
/// Some(list) = 仅加载列表中的城市。前端用它实现"只加载选中的城市"。
#[tauri::command]
pub async fn stream_temperature_cities(
    app: AppHandle,
    state: State<'_, AppState>,
    city_slugs: Option<Vec<String>>,
) -> Result<(), String> {
    // 防止 React StrictMode 双重调用：短暂持锁检查+设置标志位
    {
        let _guard = state.stream_guard.lock().await;
        if state.is_streaming.swap(true, std::sync::atomic::Ordering::SeqCst) {
            tracing::info!("stream_temperature_cities already in progress, skipping");
            return Ok(());
        }
    }

    tracing::info!("stream_temperature_cities started (filter: {:?})", city_slugs.as_ref().map(|v| v.len()));

    // 1. 从 DB 获取所有城市
    let mut cities = state
        .db
        .get_all_cities()
        .await
        .map_err(|e| format!("Failed to load cities: {}", e))?;

    // 应用城市过滤（仅加载用户选中的城市）
    if let Some(ref slugs) = city_slugs {
        let slug_set: std::collections::HashSet<&str> = slugs.iter().map(|s| s.as_str()).collect();
        cities.retain(|c| slug_set.contains(c.slug.as_str()));
    }

    let total = cities.len();
    tracing::info!("Streaming {} cities (after filter)", total);

    // 同步城市列表到 watch 通道（供跨天检测器使用）
    let city_tz_list: Vec<(String, String)> = cities
        .iter()
        .map(|c| (c.slug.clone(), c.iana_tz.clone()))
        .collect();
    let _ = state.cities_tx.send(city_tz_list.clone());

    // 初始化日期追踪器（仅对未记录的城市写入）
    state
        .date_tracker
        .init_from_cities(&city_tz_list)
        .await;

    let _ = app.emit(
        "temperature://progress",
        serde_json::json!({ "processed": 0, "total": total }),
    );

    let gamma = state.gamma.clone();
    let gamma_client = gamma.read().await.clone();

    // 收集按城市分组的 token_ids（用于 WS 订阅）
    let mut city_groups: Vec<CityTokenGroup> = Vec::new();

    // 2. 逐城市加载市场数据
    for (i, city) in cities.iter().enumerate() {
        let city_slug = &city.slug;
        let iana_tz = &city.iana_tz;

        match load_city_markets(&gamma_client, city_slug, iana_tz).await {
            Ok(markets) => {
                // 更新该城市的日期追踪记录
                if let Some(today) = local_date(iana_tz) {
                    state.date_tracker.record(city_slug, today).await;
                }

                let has_data = markets.highest.is_some();
                if has_data {
                    tracing::debug!(
                        "City {}/{} {}: loaded (highest={})",
                        i + 1, total, city_slug,
                        markets.highest.is_some(),
                    );

                    // 检测档位温度单位，与数据库不一致时更新
                    if let Some(ref highest) = markets.highest {
                        if let Some(detected_unit) = detect_unit_from_thresholds(&highest.thresholds) {
                            if detected_unit != city.unit {
                                tracing::info!(
                                    "Unit mismatch for {}: db={}, market={}, updating db",
                                    city_slug, city.unit, detected_unit
                                );
                                if let Err(e) = state.db.update_unit(city_slug, &detected_unit).await {
                                    tracing::warn!("Failed to update unit for {}: {}", city_slug, e);
                                }
                            }
                        }

                        // 收集该城市的 token_ids
                        let token_ids: Vec<String> = highest
                            .thresholds
                            .iter()
                            .map(|t| t.no_token_id.clone())
                            .collect();
                        if !token_ids.is_empty() {
                            city_groups.push(CityTokenGroup {
                                city: city_slug.clone(),
                                token_ids,
                            });
                        }
                    }
                }
                let _ = app.emit("temperature://city-loaded", &markets);
            }
            Err(e) => {
                tracing::warn!(
                    "City {}/{} {} failed: {}",
                    i + 1, total, city_slug, e
                );
                // 失败时仍推送空数据，前端显示 "No active markets"
                let empty = CityTempMarkets {
                    city: city_slug.clone(),
                    city_tz: iana_tz.clone(),
                    highest: None,
                };
                let _ = app.emit("temperature://city-loaded", &empty);
            }
        }

        // 推送进度
        let _ = app.emit(
            "temperature://progress",
            serde_json::json!({ "processed": i + 1, "total": total }),
        );
    }

    // 3. 全部完成
    tracing::info!("stream_temperature_cities done: {} cities processed", total);
    let _ = app.emit("temperature://all-loaded", ());

    // 4. 自动启动 WS 价格流（按城市分组订阅）
    let total_tokens: usize = city_groups.iter().map(|g| g.token_ids.len()).sum();
    tracing::info!(
        "Auto-starting WS price stream: {} cities, {} tokens",
        city_groups.len(),
        total_tokens
    );
    let proxy_url = state.proxy_url.read().await.clone();
    let mut manager = state.price_stream.lock().await;
    manager.start(app.clone(), city_groups, proxy_url).await;

    state.is_streaming.store(false, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}

/// Start WebSocket price stream — 按城市分组订阅
///
/// 接收前端传入的城市分组 token_ids，为每个城市启动一条独立 WS 连接，
/// 订阅该城市所有温度档位 token 的 best bid/ask。
/// 收到的价格更新通过 `price://update` 事件推送给前端。
#[tauri::command]
pub async fn start_price_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    city_groups: Vec<CityTokenGroup>,
) -> Result<(), String> {
    let total_tokens: usize = city_groups.iter().map(|g| g.token_ids.len()).sum();
    tracing::info!(
        "start_price_stream: {} cities, {} tokens total",
        city_groups.len(),
        total_tokens
    );

    let proxy_url = state.proxy_url.read().await.clone();

    let mut manager = state.price_stream.lock().await;
    manager.start(app, city_groups, proxy_url).await;

    Ok(())
}

/// Backfill prices via REST — WS 重连后补齐缺失价格
#[tauri::command]
pub async fn backfill_prices(
    state: State<'_, AppState>,
    token_ids: Vec<String>,
) -> Result<(), String> {
    tracing::debug!("backfill_prices: {} tokens", token_ids.len());

    let http = state.http.read().await;
    let manager = state.price_stream.lock().await;
    manager.backfill(&token_ids, &http).await;

    Ok(())
}

/// Get cached prices — 返回 WS 缓存的所有价格快照
#[tauri::command]
pub async fn get_cached_prices(state: State<'_, AppState>) -> Result<Vec<CachedPrice>, String> {
    let manager = state.price_stream.lock().await;
    let snapshots: Vec<PriceSnapshot> = manager.get_cached().await;
    tracing::debug!("get_cached_prices: {} snapshots", snapshots.len());
    Ok(snapshots
        .into_iter()
        .map(|s| CachedPrice {
            token_id: s.token_id,
            bid: s.bid,
            ask: s.ask,
            mid: s.mid,
            timestamp: s.timestamp,
        })
        .collect())
}

/// Stop price stream — 关闭所有城市的 WS 连接
#[tauri::command]
pub async fn stop_price_stream(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    tracing::info!("stop_price_stream");

    let mut manager = state.price_stream.lock().await;
    manager.stop_all().await;

    let _ = app.emit("ws://status", serde_json::json!({ "connected": false }));

    Ok(())
}

/// 手动触发跨天检测
///
/// 检查所有城市的当地日期是否已变化，如果有城市跨天则返回需要重新加载的城市列表。
/// 前端收到结果后可以决定是否调用 stream_temperature_cities 重新加载。
#[tauri::command]
pub async fn check_date_rollover(
    state: State<'_, AppState>,
) -> Result<DateRolloverResult, String> {
    let cities = state
        .db
        .get_all_cities()
        .await
        .map_err(|e| format!("Failed to load cities: {}", e))?;

    let city_tz_list: Vec<(String, String)> = cities
        .iter()
        .map(|c| (c.slug.clone(), c.iana_tz.clone()))
        .collect();

    let rolled = state
        .date_tracker
        .check_rollover_with_cities(&city_tz_list)
        .await;

    let has_rollover = !rolled.is_empty();

    Ok(DateRolloverResult {
        has_rollover,
        cities: rolled
            .iter()
            .map(|r| RolloverCityInfo {
                city: r.city_slug.clone(),
                tz: r.iana_tz.clone(),
                old_date: r.old_date.to_string(),
                new_date: r.new_date.to_string(),
            })
            .collect(),
    })
}

/// 跨天检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRolloverResult {
    pub has_rollover: bool,
    pub cities: Vec<RolloverCityInfo>,
}

/// 跨天城市信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloverCityInfo {
    pub city: String,
    pub tz: String,
    pub old_date: String,
    pub new_date: String,
}
