use serde::{Deserialize, Serialize};
use tauri::State;

use crate::infrastructure::db::CityRow;
use crate::infrastructure::weather::fetch_missing_coords;
use crate::state::AppState;

// ── Types ──

/// update_cities 返回给前端的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCitiesResult {
    /// Gamma API 发现的所有城市 slug
    pub discovered: Vec<String>,
    /// 新增到 DB 的城市 slug
    pub added: Vec<String>,
    /// 活跃市场已不存在、已从 DB 删除的城市 slug
    pub removed: Vec<String>,
    /// 气象站信息有变化的城市 slug
    pub station_changed: Vec<String>,
    /// 更新前 DB 城市总数
    pub total_before: usize,
    /// 更新后 DB 城市总数
    pub total_after: usize,
}

// ── Commands ──

/// 获取所有城市列表（含气象站信息）
#[tauri::command]
pub async fn get_cities(state: State<'_, AppState>) -> Result<Vec<CityRow>, String> {
    let cities = state
        .db
        .get_all_cities()
        .await
        .map_err(|e| e.to_string())?;
    Ok(cities)
}

/// 从 Polymarket Gamma API 更新城市列表和气象站信息
#[tauri::command]
pub async fn update_cities(state: State<'_, AppState>) -> Result<UpdateCitiesResult, String> {
    tracing::info!("update_cities started");

    let db = state.db.clone();
    let gamma = state.gamma.clone();

    // 1. 获取 DB 现有城市
    let existing_cities = db.get_all_cities().await.map_err(|e| e.to_string())?;
    let total_before = existing_cities.len();
    let existing_map: std::collections::HashMap<String, CityRow> = existing_cities
        .iter()
        .map(|c| (c.slug.clone(), c.clone()))
        .collect();

    // 2. 从 Gamma API 拉取活跃气温事件
    let api_cities = gamma
        .read()
        .await
        .fetch_active_temperature_cities()
        .await
        .map_err(|e| e.to_string())?;

    let discovered: Vec<String> = api_cities.iter().map(|(c, _, _, _, _)| c.clone()).collect();
    tracing::info!("Gamma API discovered {} cities", discovered.len());

    // 3. Diff: 找新增城市和气象站/头像变化
    let mut added = Vec::new();
    let mut station_changed = Vec::new();

    for (city_slug, st_name, st_code, avatar, st_url) in &api_cities {
        if let Some(existing) = existing_map.get(city_slug) {
            // 已有城市：检查气象站、采集站点网址和头像是否有变化
            let station_changed_flag = !station_eq(&existing.station_name, st_name)
                || !station_eq(&existing.station_code, st_code)
                || !station_eq(&existing.station_url, st_url);
            let avatar_changed = !station_eq(&existing.avatar, avatar);

            if station_changed_flag || avatar_changed {
                db.update_station(city_slug, st_name, st_code, st_url, avatar, None, None)
                    .await
                    .map_err(|e| e.to_string())?;
                station_changed.push(city_slug.to_string());
                if avatar_changed {
                    tracing::info!(
                        "Avatar updated for {}: {} -> {:?}",
                        city_slug,
                        existing.avatar.as_deref().unwrap_or("(empty)"),
                        avatar
                    );
                }
                if station_changed_flag {
                    tracing::info!(
                        "Station updated for {}: {} -> {:?}",
                        city_slug,
                        existing.station_name.as_deref().unwrap_or("(empty)"),
                        st_name
                    );
                }
            }
        } else {
            // 新城市：插入 DB
            let now = chrono::Utc::now().to_rfc3339();
            let row = CityRow {
                slug: city_slug.to_string(),
                city_name: city_slug_to_name(city_slug),
                utc_offset: "UTC".to_string(),
                iana_tz: "UTC".to_string(),
                unit: "\u{00B0}C".to_string(),
                station_name: st_name.clone(),
                station_code: st_code.clone(),
                station_url: st_url.clone(),
                avatar: avatar.clone(),
                lat: None,
                lon: None,
                updated_at: now,
            };
            db.insert_city(&row).await.map_err(|e| e.to_string())?;
            added.push(city_slug.to_string());
            tracing::info!("New city added: {}", city_slug);
        }
    }

// 4. 删除活跃市场已不存在的城市（DB 有但 API 未返回）
    let api_slugs: std::collections::HashSet<&str> =
        api_cities.iter().map(|(s, _, _, _, _)| s.as_str()).collect();
    let stale: Vec<String> = existing_map
        .keys()
        .filter(|slug| !api_slugs.contains(slug.as_str()))
        .cloned()
        .collect();
    let mut removed = Vec::new();
    for slug in &stale {
        if let Err(e) = db.delete_city(slug).await {
            tracing::error!("Failed to delete city {}: {}", slug, e);
        } else {
            removed.push(slug.clone());
            tracing::info!("City removed (no active market): {}", slug);
        }
    }

    // 5. 补充缺失坐标：对所有 lat/lon 为 None 的城市，通过 AWC METAR 获取观测站坐标
    let cities_after = db.get_all_cities().await.map_err(|e| e.to_string())?;
    let missing_coords = fetch_missing_coords(&state.http, &cities_after).await;
    if !missing_coords.is_empty() {
        if let Err(e) = db.update_city_coords(&missing_coords).await {
            tracing::warn!(error = ?e, "Failed to write city coordinates during sync");
        } else {
            tracing::info!(count = missing_coords.len(), "City coordinates updated during sync");
        }
    }

    // 6. 统计结果
    let total_after = cities_after.len();
    let result = UpdateCitiesResult {
        discovered,
        added,
        removed,
        station_changed,
        total_before,
        total_after,
    };

    tracing::info!(
        "update_cities done: added={}, station_changed={}, removed={}, total {}->{}",
        result.added.len(),
        result.station_changed.len(),
        result.removed.len(),
        result.total_before,
        result.total_after
    );

    Ok(result)
}

/// 更新城市的气象站编号（station_code），前端城市编辑页调用
#[tauri::command]
pub async fn update_station_code(
    state: State<'_, AppState>,
    slug: String,
    station_code: Option<String>,
) -> Result<(), String> {
    state
        .db
        .update_station_code(&slug, &station_code)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "Station code updated for {}: {:?}",
        slug,
        station_code
    );
    Ok(())
}

/// 比较两个 Option<String> 是否相等（都 None 或都 Some 且值相同）
fn station_eq(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// 将城市 slug 转为展示名（如 "new-york" -> "New York"）
fn city_slug_to_name(slug: &str) -> String {
    slug.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
