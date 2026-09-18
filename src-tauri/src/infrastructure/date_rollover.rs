use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDate;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

use crate::infrastructure::temperature::{load_city_markets, local_date, CityTempMarkets};
use crate::state::AppState;

/// 城市日期追踪器
///
/// 记录每个城市上一次加载时的当地日期。
/// 跨天检测时对比当前日期与记录日期，判断是否需要重新加载。
pub struct DateRolloverTracker {
    /// city_slug -> 上次加载时的当地日期
    dates: Arc<RwLock<HashMap<String, NaiveDate>>>,
}

impl DateRolloverTracker {
    pub fn new() -> Self {
        Self {
            dates: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 初始化/更新某城市的日期记录
    pub async fn record(&self, city_slug: &str, date: NaiveDate) {
        let mut guard = self.dates.write().await;
        guard.insert(city_slug.to_string(), date);
    }

    /// 批量初始化：从城市列表构建初始日期映射
    ///
    /// 仅对尚未记录的城市写入，已有记录的不覆盖。
    pub async fn init_from_cities(&self, cities: &[(String, String)]) {
        let mut guard = self.dates.write().await;
        for (slug, iana_tz) in cities {
            if !guard.contains_key(slug) {
                if let Some(date) = local_date(iana_tz) {
                    guard.insert(slug.clone(), date);
                }
            }
        }
    }

    /// 检测跨天（需要传入城市时区信息）
    ///
    /// 返回发生日期变化的城市列表。
    pub async fn check_rollover_with_cities(
        &self,
        cities: &[(String, String)],
    ) -> Vec<RolloverCity> {
        let guard = self.dates.read().await;
        let mut rolled = Vec::new();

        for (city_slug, iana_tz) in cities {
            let Some(old_date) = guard.get(city_slug) else {
                continue;
            };

            let Some(new_date) = local_date(iana_tz) else {
                continue;
            };

            if *old_date != new_date {
                rolled.push(RolloverCity {
                    city_slug: city_slug.clone(),
                    iana_tz: iana_tz.clone(),
                    old_date: *old_date,
                    new_date,
                });
            }
        }

        rolled
    }

    /// 更新某城市的日期记录为新日期
    pub async fn update_date(&self, city_slug: &str, new_date: NaiveDate) {
        let mut guard = self.dates.write().await;
        guard.insert(city_slug.to_string(), new_date);
    }
}

/// 发生跨天的城市信息
#[derive(Debug, Clone)]
pub struct RolloverCity {
    pub city_slug: String,
    pub iana_tz: String,
    pub old_date: NaiveDate,
    pub new_date: NaiveDate,
}

/// 启动跨天检测后台任务
///
/// 每 60 秒检测一次所有城市的当地日期是否变化。
/// 如果检测到跨天，直接在后端重载该城市的最新市场数据，
/// 通过 `temperature://city-loaded` 事件推送，前端自动更新。
/// 同时对新 token_ids 做 REST backfill 补齐价格。
pub async fn spawn_rollover_detector(
    app: AppHandle,
    tracker: Arc<DateRolloverTracker>,
    cities_rx: tokio::sync::watch::Receiver<Vec<(String, String)>>,
) {
    tokio::spawn(async move {
        let mut current_cities: Vec<(String, String)> = cities_rx.borrow().clone();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        let mut tick_count: u64 = 0;

        tracing::info!("Date rollover detector started (check interval: 60s)");

        loop {
            // 检查城市列表是否有更新
            if cities_rx.has_changed().unwrap_or(false) {
                current_cities = cities_rx.borrow().clone();
                tracing::debug!(
                    "Rollover detector: cities updated, {} cities tracked",
                    current_cities.len()
                );
            }

            interval.tick().await;
            tick_count += 1;

            // 每 10 分钟输出一次心跳 + 内存使用量，确认检测器仍在运行
            if tick_count % 10 == 0 {
                // 获取进程内存使用量（Windows: 通过 sysinfo 风格的手动查询）
                let mem_mb = get_process_memory_mb();
                tracing::info!(
                    "Rollover detector heartbeat: tick={}, cities={}, mem={}MB",
                    tick_count,
                    current_cities.len(),
                    mem_mb
                );

                // 强制 mimalloc 归还空闲内存给 OS
                // 高频 WS 消息解析产生大量小对象分配/释放，mimalloc 默认保留 free list
                // 不归还，导致 working_set 持续增长。定期调用 mi_collect 触发垃圾回收。
                unsafe { mi_collect(true) };
                let after_mb = get_process_memory_mb();
                if after_mb < mem_mb {
                    tracing::info!(
                        "mimalloc collect: mem {}MB -> {}MB (reclaimed {}MB)",
                        mem_mb, after_mb, mem_mb - after_mb
                    );
                }
            }

            let rolled = tracker.check_rollover_with_cities(&current_cities).await;

            if rolled.is_empty() {
                continue;
            }

            // 有城市跨天了
            for r in &rolled {
                tracing::info!(
                    "Date rollover detected: {} ({}): {} -> {}",
                    r.city_slug, r.iana_tz, r.old_date, r.new_date
                );
            }

            // 直接在后端重载跨天城市的最新市场数据
            reload_rolled_cities(&app, &rolled).await;

            // 更新 tracker 中的日期，避免重复触发
            for r in &rolled {
                tracker.update_date(&r.city_slug, r.new_date).await;
            }

            // 通知前端哪些城市已重载（前端可选：刷新天气数据等）
            let _ = app.emit(
                "date://rollover",
                serde_json::json!({
                    "count": rolled.len(),
                    "cities": rolled.iter().map(|r| {
                        serde_json::json!({
                            "city": r.city_slug,
                            "tz": r.iana_tz,
                            "old_date": r.old_date.to_string(),
                            "new_date": r.new_date.to_string(),
                        })
                    }).collect::<Vec<_>>()
                }),
            );
        }
    });
}

/// 重载跨天城市的最新市场数据
///
/// 对每个跨天城市：
/// 1. 调用 Gamma API 获取当地新一天的市场数据
/// 2. 通过 `temperature://city-loaded` 事件推送给前端（前端自动更新）
/// 3. 对新的 token_ids 做 REST backfill 补齐价格
/// 4. 更新温度单位到数据库
async fn reload_rolled_cities(app: &AppHandle, rolled: &[RolloverCity]) {
    let state = app.state::<AppState>();
    let gamma = state.gamma.read().await.clone();
    let proxy_url = state.proxy_url.read().await.clone();
    let http = state.http.read().await;

    tracing::info!(
        "reload_rolled_cities: {} cities to reload, proxy={:?}",
        rolled.len(),
        proxy_url
    );

    for (i, r) in rolled.iter().enumerate() {
        tracing::info!(
            "reload_rolled_cities: [{}/{}] reloading {} ({} -> {})",
            i + 1,
            rolled.len(),
            r.city_slug,
            r.old_date,
            r.new_date
        );

        match load_city_markets(&gamma, &r.city_slug, &r.iana_tz).await {
            Ok(markets) => {
                // 收集新市场的 token_ids 用于 backfill
                let mut new_token_ids: Vec<String> = Vec::new();
                if let Some(ref highest) = markets.highest {
                    new_token_ids = highest.thresholds.iter().map(|t| t.no_token_id.clone()).collect();
                }

                // 推送给前端，前端已有的 temperature://city-loaded 监听器会自动更新
                let _ = app.emit("temperature://city-loaded", &markets);

                tracing::info!(
                    "Reloaded city {} ({} -> {}): {} tokens",
                    r.city_slug,
                    r.old_date,
                    r.new_date,
                    new_token_ids.len()
                );

                // 对新 token_ids 做 REST backfill 补齐价格
                if !new_token_ids.is_empty() {
                    tracing::info!(
                        "reload_rolled_cities: backfilling {} tokens for {}",
                        new_token_ids.len(),
                        r.city_slug
                    );
                    // 注意：不在此处持有 price_stream 锁，backfill 内部会自行获取 cache 的写锁
                    let manager = state.price_stream.lock().await;
                    manager.backfill(&new_token_ids, &http).await;
                    // manager 锁在 backfill 完成后释放
                    tracing::info!(
                        "reload_rolled_cities: backfill done for {}",
                        r.city_slug
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to reload city {} after rollover: {}",
                    r.city_slug,
                    e
                );
                // 推送空数据，前端显示 "No active markets"
                let empty = CityTempMarkets {
                    city: r.city_slug.clone(),
                    city_tz: r.iana_tz.clone(),
                    highest: None,
                };
                let _ = app.emit("temperature://city-loaded", &empty);
            }
        }
    }

    tracing::info!("reload_rolled_cities: all {} cities done", rolled.len());
}

/// 获取当前进程的内存使用量（MB），用于定期日志监控内存泄漏
fn get_process_memory_mb() -> u64 {
    #[cfg(target_os = "windows")]
    {
        // 使用 Windows API GetProcessMemoryInfo
        // 避免引入 sysinfo 重依赖，直接调用 kernel32
        use std::mem::MaybeUninit;
        use std::os::raw::c_void;

        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: u64,
            working_set_size: u64,
            quota_peak_paged_pool_usage: u64,
            quota_paged_pool_usage: u64,
            quota_peak_non_paged_pool_usage: u64,
            quota_non_paged_pool_usage: u64,
            pagefile_usage: u64,
            peak_pagefile_usage: u64,
        }

        extern "system" {
            fn GetCurrentProcess() -> *mut c_void;
            fn GetProcessMemoryInfo(
                process: *mut c_void,
                counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }

        unsafe {
            let mut counters: MaybeUninit<ProcessMemoryCounters> = MaybeUninit::uninit();
            let cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
            (*counters.as_mut_ptr()).cb = cb;
            let process = GetCurrentProcess();
            let result = GetProcessMemoryInfo(process, counters.as_mut_ptr(), cb);
            if result != 0 {
                let counters = counters.assume_init();
                (counters.working_set_size / 1024 / 1024) as u64
            } else {
                0
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

// 强制 mimalloc 回收空闲内存并归还给 OS
//
// mimalloc 默认将释放的内存保留在 free list 中以加速后续分配，
// 但在高频 WS 消息解析（每秒数十条 x 5 batch）的分配-释放 churn 下，
// working_set 会持续增长而不回落。定期调用 mi_collect(true) 强制回收。
extern "C" {
    fn mi_collect(force: bool);
}
