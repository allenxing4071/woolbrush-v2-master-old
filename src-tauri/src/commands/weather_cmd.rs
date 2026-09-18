use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::infrastructure::weather::{fetch_city_weather, fetch_weather_batch};
use crate::state::AppState;

/// 流式获取城市的天气数据（AWC 实况 + Open-Meteo 预报）
///
/// 逐城市通过 `weather://city-loaded` 事件推送给前端。
/// AWC: 从 METAR 获取当天最高温 + 实时温度。
/// Open-Meteo: 获取当地 10:00-17:00 的逐小时预报温度。
///
/// `city_slugs`：可选的城市过滤列表。None = 全部城市，Some(list) = 仅这些城市。
#[tauri::command]
pub async fn fetch_weather(
    app: AppHandle,
    state: State<'_, AppState>,
    city_slugs: Option<Vec<String>>,
) -> Result<(), String> {
    tracing::info!("fetch_weather started (filter: {:?})", city_slugs.as_ref().map(|v| v.len()));

    let mut cities = state
        .db
        .get_all_cities()
        .await
        .map_err(|e| format!("Failed to load cities: {}", e))?;

    // 应用城市过滤（仅抓取用户选中的城市）
    if let Some(ref slugs) = city_slugs {
        let slug_set: std::collections::HashSet<&str> = slugs.iter().map(|s| s.as_str()).collect();
        cities.retain(|c| slug_set.contains(c.slug.as_str()));
    }

    let total = cities.len();
    let http = state.http.clone();

    let _ = app.emit(
        "weather://progress",
        serde_json::json!({ "processed": 0, "total": total }),
    );

    for (i, city) in cities.iter().enumerate() {
        let weather = fetch_city_weather(
            &http,
            &state.met_cache,
            &city.slug,
            city.station_code.as_deref(),
            city.station_url.as_deref(),
            city.lat,
            city.lon,
            &city.iana_tz,
            &city.unit,
        )
        .await;

        let _ = app.emit("weather://city-loaded", &weather);

        let _ = app.emit(
            "weather://progress",
            serde_json::json!({ "processed": i + 1, "total": total }),
        );

        if (i + 1) % 10 == 0 || i + 1 == total {
            tracing::debug!("Weather progress: {}/{}", i + 1, total);
        }
    }

    let _ = app.emit("weather://all-loaded", ());
    tracing::info!("fetch_weather done: {} cities processed", total);

    Ok(())
}

/// 刷新单个城市的天气数据（AWC 实况 + Open-Meteo 预报）
///
/// 用于遍历引擎在处理每个城市前获取最新 AWC 温度。
/// 通过 `weather://city-loaded` 事件推送给前端，与批量加载共用同一事件通道。
#[tauri::command]
pub async fn fetch_weather_city(
    app: AppHandle,
    state: State<'_, AppState>,
    city_slug: String,
) -> Result<(), String> {
    let city = state
        .db
        .get_city(&city_slug)
        .await
        .map_err(|e| format!("Failed to load city {}: {}", city_slug, e))?
        .ok_or_else(|| format!("City not found: {}", city_slug))?;

    let weather = fetch_city_weather(
        &state.http,
        &state.met_cache,
        &city.slug,
        city.station_code.as_deref(),
        city.station_url.as_deref(),
        city.lat,
        city.lon,
        &city.iana_tz,
        &city.unit,
    )
    .await;

    let _ = app.emit("weather://city-loaded", &weather);
    tracing::debug!("fetch_weather_city done: {}", city_slug);

    Ok(())
}

/// 批量获取天气数据（并行）
///
/// 一次性批量获取所有城市的 AWC / ST / MET 数据，通过 `weather://city-loaded` 逐城市推送。
/// 相比 `fetch_weather` 的串行方式，速度提升 5-10 倍。
#[tauri::command]
pub async fn fetch_weather_batch_cmd(
    app: AppHandle,
    state: State<'_, AppState>,
    city_slugs: Option<Vec<String>>,
) -> Result<(), String> {
    tracing::info!(
        "fetch_weather_batch_cmd started (filter: {:?})",
        city_slugs.as_ref().map(|v| v.len())
    );

    let mut cities = state
        .db
        .get_all_cities()
        .await
        .map_err(|e| format!("Failed to load cities: {}", e))?;

    if let Some(ref slugs) = city_slugs {
        let slug_set: std::collections::HashSet<&str> = slugs.iter().map(|s| s.as_str()).collect();
        cities.retain(|c| slug_set.contains(c.slug.as_str()));
    }

    let total = cities.len();
    if total == 0 {
        let _ = app.emit("weather://all-loaded", ());
        return Ok(());
    }

    let _ = app.emit(
        "weather://progress",
        serde_json::json!({ "processed": 0, "total": total }),
    );

    let http = state.http.clone();
    let met_cache = state.met_cache.clone();

    let results = fetch_weather_batch(&http, &met_cache, &cities).await;

    for (i, weather) in results.iter().enumerate() {
        let _ = app.emit("weather://city-loaded", weather);
        let _ = app.emit(
            "weather://progress",
            serde_json::json!({ "processed": i + 1, "total": total }),
        );
    }

    let _ = app.emit("weather://all-loaded", ());
    tracing::info!("fetch_weather_batch_cmd done: {} cities", results.len());

    Ok(())
}

/// 启动天气定时刷新调度器
///
/// - ST + AWC：每 60 秒刷新一次（直接请求 API）
/// - MET：每 3600 秒刷新一次（通过 MET 缓存 TTL 控制，TTL=3600s）
///
/// 刷新结果通过 `weather://city-loaded` 事件推送给前端。
/// 在应用启动时由 `lib.rs` setup 调用。
pub fn spawn_weather_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 等待应用完全启动
        tokio::time::sleep(Duration::from_secs(8)).await;

        tracing::info!("Weather scheduler started (ST/AWC: 60s, MET: 3600s via cache TTL)");

        let state = app.state::<AppState>();

        let mut interval = tokio::time::interval(Duration::from_secs(60));
        // 第一次 tick 立即返回，用于初始化时获取数据
        interval.tick().await;

        let mut tick_count: u64 = 0;

        loop {
            interval.tick().await;
            tick_count += 1;

            let cities = match state.db.get_all_cities().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = ?e, "Weather scheduler: failed to load cities");
                    continue;
                }
            };

            if cities.is_empty() {
                continue;
            }

            let http = state.http.clone();
            let met_cache = state.met_cache.clone();

            let results = fetch_weather_batch(&http, &met_cache, &cities).await;

            for weather in &results {
                let _ = app.emit("weather://city-loaded", weather);
            }

            if tick_count % 10 == 0 {
                tracing::info!(
                    "Weather scheduler heartbeat: tick={} ({}min), cities={}",
                    tick_count,
                    tick_count,
                    results.len()
                );
            }
        }
    });
}
